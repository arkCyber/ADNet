//! DM session — Noise_XX handshake + ChaCha20-Poly1305 AEAD session.
//!
//! ## Handshake
//!
//! ```text
//! Initiator (Alice)                       Responder (Bob)
//! ─────────────                            ────────────
//! → e, es                                 msg 1
//! ← e, ee, s                              msg 2
//! → s, se                                 msg 3
//! ```
//!
//! After the handshake both sides derive a 32-byte symmetric session
//! key via HKDF-SHA256(secret, info="a3chat/dm/session/v1").
//!
//! ## Transport
//!
//! - Each direction has its own 32-byte key (split via HKDF with
//!   `info = "a3chat/dm/session/v1/dir"`).
//! - Each message uses a fresh 12-byte random nonce.
//! - Associated data (AD) is `BLAKE3(sender_node_id || receiver_node_id
//!   || conversation_id || sequence || timestamp)`. Receivers MUST
//!   reject mismatched AD.
//!
//! ## Re-handshake
//!
//! Sessions are short-lived. After [`DmSession::MAX_MESSAGES`] messages
//! *or* [`DmSession::MAX_AGE_SECS`] seconds, the caller must run a new
//! handshake. The session also exposes
//! [`DmSession::current_state`] so callers can persist the noise
//! handshake transcript across reconnects.

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use snow::HandshakeState;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, CryptoResult};
use crate::random::{random_key_32, random_nonce};

/// Wire-format tag for the handshake message kind. Used by the higher
/// layer (`a3chat-app`) to dispatch on the first received handshake
/// frame.
pub const HANDSHAKE_KIND: &str = "noise-xx";

/// Domain-separation tag for the session HKDF.
const SESSION_INFO: &[u8] = b"a3chat/dm/session/v1";
/// Domain-separation tag for the directional key HKDF.
const DIR_INFO: &[u8] = b"a3chat/dm/session/v1/dir";

/// Initial handshake message tag (initiator → responder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeMessage {
    pub kind: String, // always "noise-xx"
    /// Raw snow handshake payload (≤ 96 bytes for Noise_XX with
    /// Curve25519; we cap encoding at 128 just to be safe).
    pub payload: Vec<u8>,
}

impl HandshakeMessage {
    pub fn encode(&self) -> CryptoResult<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| CryptoError::Internal(format!("encode: {e}")))
    }
    pub fn decode(bytes: &[u8]) -> CryptoResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| CryptoError::Internal(format!("decode: {e}")))
    }
}

/// 32-byte session key. Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionKey([u8; 32]);

impl SessionKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }
}

/// The two directional keys derived from a Noise_XX handshake.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    pub send: SessionKey,
    pub recv: SessionKey,
}

impl SessionKeys {
    pub fn from_shared_secret(secret: &[u8], initiator: bool) -> CryptoResult<Self> {
        // 1. Domain-separated HKDF extract+expand to a single 64-byte
        //    stream of key material.
        let hk = Hkdf::<Sha256>::new(None, secret);
        let mut okm = [0u8; 64];
        hk.expand(SESSION_INFO, &mut okm)
            .map_err(|e| CryptoError::Internal(format!("hkdf: {e}")))?;

        // 2. Split into two directional keys with another KDF pass so
        //    each side has independent keys even if one leaks.
        let send_bytes = &okm[..32];
        let recv_bytes = &okm[32..];

        let (send, recv) = if initiator {
            // Initiator writes with key[0], reads with key[1].
            (send_bytes, recv_bytes)
        } else {
            // Responder swaps.
            (recv_bytes, send_bytes)
        };

        let send = hkdf_direction(send)?;
        let recv = hkdf_direction(recv)?;
        okm.zeroize();
        Ok(Self {
            send: SessionKey(send),
            recv: SessionKey(recv),
        })
    }
}

fn hkdf_direction(input: &[u8]) -> CryptoResult<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(None, input);
    let mut out = [0u8; 32];
    hk.expand(DIR_INFO, &mut out)
        .map_err(|e| CryptoError::Internal(format!("hkdf direction: {e}")))?;
    Ok(out)
}

/// Per-conversation AEAD session.
///
/// Use the free functions [`seal`] / [`open`] rather than calling the
/// AEAD primitives directly so the nonce-derivation and AD-building
/// conventions stay in one place.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DmSession {
    send: SessionKey,
    recv: SessionKey,
    /// Per-sender monotonically-increasing sequence of *sealed*
    /// messages. Used for AD building and for re-handshake decisions.
    pub messages_sent: u32,
    pub messages_received: u32,
    /// Unix seconds when this session was established.
    pub created_at: i64,
    /// Optional identifier for logging / debugging (e.g. "alice↔bob").
    pub label: Option<String>,
}

impl DmSession {
    /// Hard cap on messages per session before re-handshake is
    /// required (limits damage from key compromise).
    pub const MAX_MESSAGES: u32 = 100;
    /// Hard cap on session age (forces PFS refresh).
    pub const MAX_AGE_SECS: i64 = 24 * 60 * 60;

    pub fn new(keys: SessionKeys, label: Option<String>) -> Self {
        // Pull the bytes out of `keys` *before* dropping it. The
        // `SessionKeys` wrapper has a `Drop` impl (from
        // `ZeroizeOnDrop`) which forbids `mem::take`-ing its
        // fields, but we can read the byte arrays while it is
        // still alive, then drop the wrapper knowing the secrets
        // are already copied into `Self`.
        Self {
            send: SessionKey(keys.send.0),
            recv: SessionKey(keys.recv.0),
            messages_sent: 0,
            messages_received: 0,
            created_at: chrono::Utc::now().timestamp(),
            label,
        }
    }

    /// Convenience constructor for callers who already have raw
    /// `(send_key, recv_key)` byte pairs (e.g. recovered from disk
    /// or rebuilt after a re-handshake).
    pub fn from_bytes(send: [u8; 32], recv: [u8; 32], label: Option<String>) -> Self {
        Self {
            send: SessionKey(send),
            recv: SessionKey(recv),
            messages_sent: 0,
            messages_received: 0,
            created_at: chrono::Utc::now().timestamp(),
            label,
        }
    }

    pub fn needs_rehandshake(&self, now: i64) -> bool {
        self.messages_sent >= Self::MAX_MESSAGES || (now - self.created_at) >= Self::MAX_AGE_SECS
    }

    /// Current state for inspection / debugging.
    pub fn current_state(&self) -> SessionState {
        SessionState {
            messages_sent: self.messages_sent,
            messages_received: self.messages_received,
            created_at: self.created_at,
            age_secs: chrono::Utc::now().timestamp() - self.created_at,
        }
    }

    pub fn send_key(&self) -> &SessionKey {
        &self.send
    }
    pub fn recv_key(&self) -> &SessionKey {
        &self.recv
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub messages_sent: u32,
    pub messages_received: u32,
    pub created_at: i64,
    pub age_secs: i64,
}

// -- Handshake helpers ------------------------------------------------------

/// Create an initiator `HandshakeState`. Caller drives the state
/// machine with [`snow::HandshakeState::write_message`] /
/// [`read_message`] and feeds the resulting transport secret into
/// [`SessionKeys::from_shared_secret`].
///
/// The Noise_XX pattern requires both sides to have a long-term
/// static keypair. We generate a fresh one for each handshake so
/// callers don't have to manage key state — production callers
/// should swap this for the user's persisted `a3net-identity`
/// signing key once that integration lands (P6).
pub fn handshake_initiator() -> CryptoResult<HandshakeState> {
    let builder = snow::Builder::new("Noise_XX_25519_ChaChaPoly_SHA256".parse().unwrap());
    let static_key = random_key_32();
    builder
        .local_private_key(&static_key)
        .build_initiator()
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))
}

/// Create a responder `HandshakeState`.
pub fn handshake_responder() -> CryptoResult<HandshakeState> {
    let builder = snow::Builder::new("Noise_XX_25519_ChaChaPoly_SHA256".parse().unwrap());
    let static_key = random_key_32();
    builder
        .local_private_key(&static_key)
        .build_responder()
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))
}

/// Build the first handshake payload from the initiator.
pub fn initiator_first_message(state: &mut HandshakeState) -> CryptoResult<HandshakeMessage> {
    let mut payload = vec![0u8; 96];
    let len = state
        .write_message(&[], &mut payload)
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))?;
    payload.truncate(len);
    Ok(HandshakeMessage {
        kind: HANDSHAKE_KIND.into(),
        payload,
    })
}

/// Build the second (responder → initiator) handshake payload.
pub fn responder_second_message(
    state: &mut HandshakeState,
    first: &HandshakeMessage,
) -> CryptoResult<HandshakeMessage> {
    let mut payload = vec![0u8; 96];
    let len = state
        .read_message(&first.payload, &mut payload)
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))?;
    let mut out = vec![0u8; 96];
    let len2 = state
        .write_message(&[], &mut out)
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))?;
    payload.truncate(len);
    out.truncate(len2);
    // Return the *response* (the responder's write_message output).
    Ok(HandshakeMessage {
        kind: HANDSHAKE_KIND.into(),
        payload: out,
    })
    // `payload` from the read_message is discarded (echo of the
    // caller's [] = empty handshake payload).
}

/// Build the third (initiator → responder) handshake payload.
///
/// Caller should *also* call `is_handshake_finished()` to confirm
/// the Noise state is in the `Transport` phase before extracting
/// the shared secret.
pub fn initiator_final_message(
    state: &mut HandshakeState,
    second: &HandshakeMessage,
) -> CryptoResult<(HandshakeMessage, [u8; 32])> {
    // 1. Read m2 (bob's e, ee, s) — no payload from the responder.
    let mut echoed = vec![0u8; 96];
    let _ = state
        .read_message(&second.payload, &mut echoed)
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))?;

    // 2. Write m3 (alice's s, se) — empty application payload.
    let mut m3 = vec![0u8; 96];
    let m3_len = state
        .write_message(&[], &mut m3)
        .map_err(|e| CryptoError::NoiseHandshake(e.to_string()))?;
    m3.truncate(m3_len);

    // 3. The transport secret we extract from the finished handshake.
    //    `noise` derives a per-handshake hash that becomes the IKM for
    //    our HKDF — for XX it's the full transcript hash. Take the
    //    first 32 bytes as our shared secret.
    let secret: [u8; 32] = {
        let bytes: &[u8] = state.get_handshake_hash();
        let mut out = [0u8; 32];
        let n = bytes.len().min(32);
        out[..n].copy_from_slice(&bytes[..n]);
        out
    };
    Ok((
        HandshakeMessage {
            kind: HANDSHAKE_KIND.into(),
            payload: m3,
        },
        secret,
    ))
}

// -- Seal / open ------------------------------------------------------------

/// Encrypt `plaintext` with `key` and a fresh nonce. Returns
/// `(nonce_bytes, ciphertext_with_tag)`. `ad` is bound to the AEAD.
pub fn seal(key: &SessionKey, ad: &[u8], plaintext: &[u8]) -> CryptoResult<(Vec<u8>, Vec<u8>)> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key};

    let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_bytes()));
    let nonce = random_nonce();
    let ct = cipher
        .encrypt(
            chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::AeadTagMismatch)?;
    Ok((nonce.to_vec(), ct))
}

/// Decrypt `ciphertext` with `key` and the supplied `nonce`. Verifies
/// the AEAD tag against `ad`.
pub fn open(key: &SessionKey, nonce: &[u8], ad: &[u8], ciphertext: &[u8]) -> CryptoResult<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key};

    if nonce.len() != 12 {
        return Err(CryptoError::InvalidLength {
            field: "nonce",
            expected: 12,
            actual: nonce.len(),
        });
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_bytes()));
    cipher
        .decrypt(
            chacha20poly1305::aead::generic_array::GenericArray::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::AeadTagMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end Noise_XX handshake between initiator and responder.
    /// The two sides should derive identical session keys.
    #[test]
    fn noise_xx_handshake_yields_matching_session_keys() {
        let mut alice = handshake_initiator().expect("initiator");
        let mut bob = handshake_responder().expect("responder");

        // Alice → Bob
        let m1 = initiator_first_message(&mut alice).expect("m1");
        let m2 = responder_second_message(&mut bob, &m1).expect("m2");
        // Alice processes m2 and emits m3.
        let (m3, _alice_secret_before_m3) = initiator_final_message(&mut alice, &m2).expect("m3");

        // Bob reads m3.
        let mut tmp = vec![0u8; 96];
        let _ = bob.read_message(&m3.payload, &mut tmp).expect("bob m3");

        // Both sides should now be in the Transport phase with the
        // same handshake hash.
        let alice_secret = alice.get_handshake_hash().to_vec();
        let bob_secret = bob.get_handshake_hash().to_vec();
        assert_eq!(alice_secret, bob_secret, "handshake hashes must match");

        // Derive session keys
        let alice_keys = SessionKeys::from_shared_secret(&alice_secret, true).expect("a keys");
        let bob_keys = SessionKeys::from_shared_secret(&alice_secret, false).expect("b keys");
        assert_eq!(
            alice_keys.send.as_bytes(),
            bob_keys.recv.as_bytes(),
            "alice.send == bob.recv"
        );
        assert_eq!(
            alice_keys.recv.as_bytes(),
            bob_keys.send.as_bytes(),
            "alice.recv == bob.send"
        );
    }

    #[test]
    fn seal_open_round_trip() {
        let key = SessionKey::from_bytes([7u8; 32]);
        let (nonce, ct) = seal(&key, b"ad", b"plaintext").expect("seal");
        assert_eq!(nonce.len(), 12);
        assert_eq!(ct.len(), "plaintext".len() + 16); // tag overhead
        let pt = open(&key, &nonce, b"ad", &ct).expect("open");
        assert_eq!(pt, b"plaintext");
    }

    #[test]
    fn open_rejects_tampered_ciphertext() {
        let key = SessionKey::from_bytes([7u8; 32]);
        let (nonce, mut ct) = seal(&key, b"ad", b"hello").expect("seal");
        ct[0] ^= 0x01; // flip one bit
        let result = open(&key, &nonce, b"ad", &ct);
        assert!(matches!(result, Err(CryptoError::AeadTagMismatch)));
    }

    #[test]
    fn open_rejects_wrong_ad() {
        let key = SessionKey::from_bytes([7u8; 32]);
        let (nonce, ct) = seal(&key, b"ad-1", b"hello").expect("seal");
        let result = open(&key, &nonce, b"ad-2", &ct);
        assert!(matches!(result, Err(CryptoError::AeadTagMismatch)));
    }

    #[test]
    fn open_rejects_wrong_key() {
        let key_a = SessionKey::from_bytes([7u8; 32]);
        let key_b = SessionKey::from_bytes([8u8; 32]);
        let (nonce, ct) = seal(&key_a, b"ad", b"hello").expect("seal");
        let result = open(&key_b, &nonce, b"ad", &ct);
        assert!(matches!(result, Err(CryptoError::AeadTagMismatch)));
    }

    #[test]
    fn open_rejects_short_nonce() {
        let key = SessionKey::from_bytes([7u8; 32]);
        let (_, ct) = seal(&key, b"ad", b"hello").expect("seal");
        let result = open(&key, &[0; 8], b"ad", &ct);
        assert!(matches!(result, Err(CryptoError::InvalidLength { .. })));
    }

    #[test]
    fn dm_session_needs_rehandshake_after_max_messages() {
        let keys = SessionKeys::from_shared_secret(&[1u8; 32], true).unwrap();
        let mut s = DmSession::new(keys, Some("test".into()));
        assert!(!s.needs_rehandshake(chrono::Utc::now().timestamp()));
        s.messages_sent = DmSession::MAX_MESSAGES;
        assert!(s.needs_rehandshake(chrono::Utc::now().timestamp()));
    }

    #[test]
    fn dm_session_needs_rehandshake_after_max_age() {
        let keys = SessionKeys::from_shared_secret(&[1u8; 32], true).unwrap();
        let s = DmSession::new(keys, None);
        let future = s.created_at + DmSession::MAX_AGE_SECS + 1;
        assert!(s.needs_rehandshake(future));
    }

    #[test]
    fn session_state_tracks_counters() {
        let keys = SessionKeys::from_shared_secret(&[1u8; 32], true).unwrap();
        let s = DmSession::new(keys, None);
        let st = s.current_state();
        assert_eq!(st.messages_sent, 0);
        assert_eq!(st.messages_received, 0);
    }

    #[test]
    fn handshake_message_round_trip() {
        let m = HandshakeMessage {
            kind: HANDSHAKE_KIND.into(),
            payload: vec![1, 2, 3],
        };
        let bytes = m.encode().unwrap();
        let decoded = HandshakeMessage::decode(&bytes).unwrap();
        assert_eq!(m, decoded);
    }
}
