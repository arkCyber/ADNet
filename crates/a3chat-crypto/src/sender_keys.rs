//! Sender Keys for group chat — Signal-style.
//!
//! ## Why Sender Keys
//!
//! - Single sender has O(N) state for an N-member group (one chain
//!   per group, not per member).
//! - Receiver needs no ratchet state per sender — just the latest
//!   chain key.
//! - Trade-off: no forward secrecy per-member; a sender-key
//!   compromise reveals that sender's whole history. Mitigated by
//!   re-keying on member add/remove (see [`SenderKeyChain::rotate`]).
//!
//! ## Wire format
//!
//! Each chain step:
//!
//! ```text
//! chain_key ─HKDF─▶ message_key ─AEAD─▶ ciphertext || tag(16)
//! chain_key'    ─HKDF─▶ next chain_key (discarded after use)
//! ```
//!
//! The chain key advances deterministically; the message key is a
//! one-shot derived key, then discarded. Receivers iterate the same
//! HKDF in lock-step.

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, CryptoResult};
use crate::random::random_bytes;

/// Per-group unique identifier of a Sender Key. A new `SenderKeyId`
/// is generated whenever the group rotates (member add/remove).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SenderKeyId(pub [u8; 16]);

impl SenderKeyId {
    pub fn random() -> Self {
        let bytes: [u8; 16] = random_bytes(16)
            .try_into()
            .expect("random_bytes(16) returns 16 bytes");
        Self(bytes)
    }
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// The full chain key — 32-byte secret + 32-bit iteration counter.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SenderKey {
    /// `id` is not secret material — public identifier of the chain.
    /// `#[zeroize(skip)]` keeps the derive happy while preserving
    /// the correct security invariant (the chain_key bytes are wiped).
    #[zeroize(skip)]
    pub id: SenderKeyId,
    chain_key: [u8; 32],
    iteration: u32,
    /// Hard cap on messages per Sender Key (forces rotation).
    pub max_iterations: u32,
}

impl SenderKey {
    /// Build a fresh Sender Key with 32 bytes of random chain key
    /// material.
    pub fn generate(id: SenderKeyId) -> Self {
        let mut ck = [0u8; 32];
        let bytes = random_bytes(32);
        ck.copy_from_slice(&bytes);
        Self {
            id,
            chain_key: ck,
            iteration: 0,
            max_iterations: 100_000,
        }
    }

    /// Build from existing chain key bytes (used when receiving a
    /// distributed key).
    pub fn from_bytes(id: SenderKeyId, chain_key: [u8; 32]) -> Self {
        Self {
            id,
            chain_key,
            iteration: 0,
            max_iterations: 100_000,
        }
    }

    pub fn chain_key(&self) -> &[u8; 32] {
        &self.chain_key
    }
    pub fn iteration(&self) -> u32 {
        self.iteration
    }

    /// Advance the chain by one step: derive a fresh message key, then
    /// derive the next chain key.
    ///
    /// Returns `(message_key, new_chain_key)`. The caller is
    /// responsible for `Zeroize`ing the message key after the AEAD
    /// encryption is done.
    fn step(&mut self) -> CryptoResult<(MessageKey, [u8; 32])> {
        if self.iteration >= self.max_iterations {
            return Err(CryptoError::SenderKeyExhausted {
                id: self.id,
                iteration: self.iteration,
            });
        }
        // HKDF-Expand twice: once for the message key, once for the
        // next chain key.
        let hk = Hkdf::<Sha256>::new(None, &self.chain_key);
        let mut mk = [0u8; 32];
        let mut next_ck = [0u8; 32];
        hk.expand(b"a3chat/sk/message_key/v1", &mut mk)
            .map_err(|e| CryptoError::Internal(format!("hkdf mk: {e}")))?;
        hk.expand(b"a3chat/sk/next_chain/v1", &mut next_ck)
            .map_err(|e| CryptoError::Internal(format!("hkdf next: {e}")))?;
        self.chain_key = next_ck;
        self.iteration += 1;
        Ok((MessageKey(mk), next_ck))
    }
}

/// One-shot key derived from a Sender Key chain step. Erased after the
/// AEAD encryption is complete.
#[derive(Zeroize, ZeroizeOnDrop)]
struct MessageKey([u8; 32]);

/// The full chain — what gets persisted on disk for each group +
/// member combination.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SenderKeyChain {
    #[zeroize(skip)]
    pub id: SenderKeyId,
    /// Replica of `SenderKey::chain_key`; duplicated here so the chain
    /// is self-contained and serializable.
    chain_key: [u8; 32],
    iteration: u32,
}

impl SenderKeyChain {
    pub fn new(key: &SenderKey) -> Self {
        Self {
            id: key.id,
            chain_key: *key.chain_key(),
            iteration: key.iteration,
        }
    }

    /// Encrypt the next group message. Returns the encrypted bytes
    /// plus the iteration index used.
    pub fn seal_next(&mut self, ad: &[u8], plaintext: &[u8]) -> CryptoResult<(u32, Vec<u8>)> {
        // We re-derive from the same HKDF inputs each call so
        // callers don't have to share a `SenderKey` struct between
        // threads.
        if self.iteration >= 100_000 {
            return Err(CryptoError::SenderKeyExhausted {
                id: self.id,
                iteration: self.iteration,
            });
        }
        let hk = Hkdf::<Sha256>::new(None, &self.chain_key);
        let mut mk = [0u8; 32];
        let mut next_ck = [0u8; 32];
        hk.expand(b"a3chat/sk/message_key/v1", &mut mk)
            .map_err(|e| CryptoError::Internal(format!("hkdf mk: {e}")))?;
        hk.expand(b"a3chat/sk/next_chain/v1", &mut next_ck)
            .map_err(|e| CryptoError::Internal(format!("hkdf next: {e}")))?;
        let iteration = self.iteration;
        // Encrypt.
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use chacha20poly1305::{ChaCha20Poly1305, Key};
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&mk));
        let nonce = crate::random::random_nonce();
        let ct = cipher
            .encrypt(
                chacha20poly1305::aead::generic_array::GenericArray::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: ad,
                },
            )
            .map_err(|_| CryptoError::AeadTagMismatch)?;
        mk.zeroize();
        self.chain_key.copy_from_slice(&next_ck);
        self.iteration += 1;
        // Pack iteration + nonce + ciphertext into one buffer so the
        // receiver doesn't need separate fields.
        let mut out = Vec::with_capacity(4 + 12 + ct.len());
        out.extend_from_slice(&iteration.to_le_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok((iteration, out))
    }

    /// Decrypt the next message in the chain (the sender's
    /// `iteration` MUST match `self.iteration` or the chain is
    /// out-of-sync).
    pub fn open_next(
        &mut self,
        ad: &[u8],
        blob: &[u8],
        expected_iteration: u32,
    ) -> CryptoResult<Vec<u8>> {
        if blob.len() < 4 + 12 + 16 {
            return Err(CryptoError::InvalidLength {
                field: "sender_key blob",
                expected: 32,
                actual: blob.len(),
            });
        }
        if expected_iteration != self.iteration {
            return Err(CryptoError::SenderKeyExhausted {
                id: self.id,
                iteration: expected_iteration,
            });
        }
        let hk = Hkdf::<Sha256>::new(None, &self.chain_key);
        let mut mk = [0u8; 32];
        let mut next_ck = vec![0u8; 32];
        hk.expand(b"a3chat/sk/message_key/v1", &mut mk)
            .map_err(|e| CryptoError::Internal(format!("hkdf mk: {e}")))?;
        hk.expand(b"a3chat/sk/next_chain/v1", &mut next_ck)
            .map_err(|e| CryptoError::Internal(format!("hkdf next: {e}")))?;
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        use chacha20poly1305::{ChaCha20Poly1305, Key};
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&mk));
        let nonce = &blob[4..16];
        let ct = &blob[16..];
        let pt = cipher
            .decrypt(
                chacha20poly1305::aead::generic_array::GenericArray::from_slice(nonce),
                Payload { msg: ct, aad: ad },
            )
            .map_err(|_| CryptoError::AeadTagMismatch)?;
        mk.zeroize();
        self.chain_key.copy_from_slice(&next_ck);
        self.iteration += 1;
        Ok(pt)
    }

    /// Rotate — discard the current chain and start a fresh one.
    /// Triggered when a member joins/leaves the group.
    pub fn rotate(&mut self, new_id: SenderKeyId, new_chain_key: [u8; 32]) {
        self.id = new_id;
        self.chain_key = new_chain_key;
        self.iteration = 0;
    }

    pub fn id(&self) -> SenderKeyId {
        self.id
    }
    pub fn iteration(&self) -> u32 {
        self.iteration
    }
}

/// Wire-format distribution message — what the group owner sends to a
/// new member when onboarding them to a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderKeyDistribution {
    pub conversation_id: a3chat_core::id::ConversationId,
    pub sender_key_id: SenderKeyId,
    pub chain_key: String, // 32 bytes, hex-encoded
    pub iteration: u32,
}

impl SenderKeyDistribution {
    pub fn encode(&self) -> CryptoResult<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| CryptoError::Internal(format!("encode: {e}")))
    }
    pub fn decode(bytes: &[u8]) -> CryptoResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| CryptoError::Internal(format!("decode: {e}")))
    }
}

/// Public re-export of the chain-step helper used by tests.
pub fn step_sender_key(key: &mut SenderKey) -> CryptoResult<()> {
    let _ = key
        .step()
        .map_err(|e| CryptoError::Internal(format!("{e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3chat_core::id::ConversationId;

    fn ad() -> Vec<u8> {
        b"a3chat-group-ad-v1".to_vec()
    }

    #[test]
    fn sender_key_id_is_random_and_distinct() {
        let a = SenderKeyId::random();
        let b = SenderKeyId::random();
        assert_ne!(a, b);
        assert_eq!(a.as_hex().len(), 32);
    }

    #[test]
    fn round_trip_sender_key_distribution() {
        let dist = SenderKeyDistribution {
            conversation_id: ConversationId::from("grp:abc"),
            sender_key_id: SenderKeyId([1u8; 16]),
            chain_key: hex::encode([2u8; 32]),
            iteration: 5,
        };
        let bytes = dist.encode().unwrap();
        let decoded = SenderKeyDistribution::decode(&bytes).unwrap();
        assert_eq!(dist, decoded);
    }

    #[test]
    fn chain_seal_open_round_trip() {
        let id = SenderKeyId::random();
        let mut sender_chain = SenderKeyChain::new(&SenderKey::from_bytes(id, [0x42u8; 32]));
        let initial_ck = sender_chain.chain_key; // copy
        let (_iter, blob) = sender_chain.seal_next(&ad(), b"hello world").expect("seal");
        // Build a receiver chain that starts in lock-step.
        let mut recv_chain = SenderKeyChain::new(&SenderKey::from_bytes(id, initial_ck));
        let pt = recv_chain.open_next(&ad(), &blob, 0).expect("open");
        assert_eq!(pt, b"hello world");
        assert_eq!(sender_chain.iteration(), recv_chain.iteration());
    }

    #[test]
    fn chain_out_of_sync_is_rejected() {
        let id = SenderKeyId::random();
        let mut sender = SenderKeyChain::new(&SenderKey::from_bytes(id, [0x42u8; 32]));
        let (_, blob) = sender.seal_next(&ad(), b"hi").expect("seal");
        // Receiver expects iteration 5 — wrong.
        let mut recv = SenderKeyChain::new(&SenderKey::from_bytes(id, [0x42u8; 32]));
        let result = recv.open_next(&ad(), &blob, 5);
        assert!(matches!(
            result,
            Err(CryptoError::SenderKeyExhausted { .. })
        ));
    }

    #[test]
    fn chain_rejects_too_short_blob() {
        let id = SenderKeyId::random();
        let mut chain = SenderKeyChain::new(&SenderKey::from_bytes(id, [0x42u8; 32]));
        let result = chain.open_next(&ad(), &[0; 4], 0);
        assert!(matches!(result, Err(CryptoError::InvalidLength { .. })));
    }

    #[test]
    fn chain_rejects_tampered_blob() {
        let id = SenderKeyId::random();
        let initial_ck = [0x42u8; 32];
        let mut sender = SenderKeyChain::new(&SenderKey::from_bytes(id, initial_ck));
        let (_, mut blob) = sender.seal_next(&ad(), b"hi").expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        let mut recv = SenderKeyChain::new(&SenderKey::from_bytes(id, initial_ck));
        let result = recv.open_next(&ad(), &blob, 0);
        assert!(matches!(result, Err(CryptoError::AeadTagMismatch)));
    }

    #[test]
    fn chain_rotate_resets_iteration() {
        let id = SenderKeyId::random();
        let mut chain = SenderKeyChain::new(&SenderKey::from_bytes(id, [0x42u8; 32]));
        let (_, _) = chain.seal_next(&ad(), b"a").expect("1");
        let (_, _) = chain.seal_next(&ad(), b"b").expect("2");
        assert_eq!(chain.iteration(), 2);
        chain.rotate(SenderKeyId::random(), [0x99u8; 32]);
        assert_eq!(chain.iteration(), 0);
    }

    #[test]
    fn send_open_100_messages_in_lockstep() {
        let id = SenderKeyId::random();
        let initial_ck = [0xABu8; 32];
        let mut sender = SenderKeyChain::new(&SenderKey::from_bytes(id, initial_ck));
        let mut recv = SenderKeyChain::new(&SenderKey::from_bytes(id, initial_ck));
        for i in 0u32..100 {
            let msg = format!("message {i}");
            let (it, blob) = sender.seal_next(&ad(), msg.as_bytes()).unwrap();
            assert_eq!(it, i);
            let pt = recv.open_next(&ad(), &blob, i).unwrap();
            assert_eq!(pt, msg.as_bytes());
        }
        assert_eq!(sender.iteration(), 100);
        assert_eq!(recv.iteration(), 100);
    }
}
