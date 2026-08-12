//! Noise_XX handshake running over the WebRTC DataChannel.
//!
//! The handshake is identical in shape to the one used by `adnet-dht`:
//! three messages, no pre-shared secret, mutual authentication of static
//! keys. We pin `Noise_XX_25519_ChaChaPoly_SHA256` because it matches the
//! primitives already used by ADNet (x25519, ChaCha20-Poly1305, SHA-256).
//!
//! After the handshake, both peers derive the same pair of cipher states
//! (one for sending, one for receiving). The remote static key is then
//! bound to a `NodeId` via BLAKE3.
//!
//! ## Wire format
//!
//! Each Noise message is length-prefixed (`u32 BE` + body), up to 65535
//! bytes. The body is the raw Noise transport message (no additional
//! framing). After the handshake, application frames flow over the same
//! channel, using the standard ADNet [`Frame`](adnet_types) codec —
//! i.e. the byte boundary between Noise handshake and data frames is the
//! first byte after the third handshake message.

use std::sync::Arc;

use adnet_types::node::{NodeId, NODE_ID_BYTES};
use snow::{Builder, Keypair, TransportState};
use tokio::sync::Mutex;

use crate::error::{WebRtcError, WebRtcResult};

/// The Noise pattern we use. Documented in the Noise spec
/// (<https://noiseprotocol.org/noise.html>) as pattern 1 in the XX family.
pub const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// Maximum Noise message size. Messages larger than this are rejected by
/// the codec; in practice we never exceed ~128 bytes during the
/// handshake.
pub const MAX_NOISE_MSG: usize = 1024;

/// A 32-byte Noise static public key. Used to derive the NodeId.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticPub([u8; NODE_ID_BYTES]);

impl StaticPub {
    /// Derive the NodeId from the static public key.
    pub fn to_node_id(&self) -> NodeId {
        // BLAKE3 → first 32 bytes → hex.
        let hash = blake3::hash(&self.0);
        let bytes: [u8; NODE_ID_BYTES] = hash.as_bytes()[..NODE_ID_BYTES]
            .try_into()
            .expect("blake3 always returns 32 bytes");
        NodeId::from_bytes(&bytes).expect("32 bytes is the right length")
    }

    /// Raw bytes of the static public key.
    pub fn as_bytes(&self) -> &[u8; NODE_ID_BYTES] {
        &self.0
    }
}

/// Handshake role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Initiator. Sends the first message.
    Initiator,
    /// Responder. Replies to the first message.
    Responder,
}

/// Result of a successful Noise handshake. Holds the cipher state
/// (send + receive on the same `TransportState`, serialised via a
/// `Mutex`) and the remote static public key.
pub struct NoiseSession {
    /// Single cipher state. `snow::TransportState` is `!Sync` and not
    /// `Clone`, so we wrap it in a `Mutex`. The lock is held only for the
    /// duration of one encrypt/decrypt call, so contention is not a
    /// concern.
    inner: Arc<Mutex<TransportState>>,
    /// The remote peer's static public key, derived from the handshake.
    remote_static: StaticPub,
}

impl NoiseSession {
    /// Encrypt and return a ciphertext for the next outgoing plaintext.
    pub async fn encrypt(&self, plaintext: &[u8]) -> WebRtcResult<Vec<u8>> {
        let mut guard = self.inner.lock().await;
        let mut buf = vec![0u8; plaintext.len() + 16];
        let len = guard
            .write_message(plaintext, &mut buf)
            .map_err(|e| WebRtcError::Noise(format!("encrypt: {e}")))?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Decrypt the next ciphertext.
    pub async fn decrypt(&self, ciphertext: &[u8]) -> WebRtcResult<Vec<u8>> {
        let mut guard = self.inner.lock().await;
        let mut buf = vec![0u8; ciphertext.len()];
        let len = guard
            .read_message(ciphertext, &mut buf)
            .map_err(|e| WebRtcError::Noise(format!("decrypt: {e}")))?;
        buf.truncate(len);
        Ok(buf)
    }

    /// The remote peer's static public key, derivable into a NodeId.
    pub fn remote_static(&self) -> &StaticPub {
        &self.remote_static
    }

    /// The remote NodeId derived from the remote static key.
    pub fn remote_node_id(&self) -> NodeId {
        self.remote_static.to_node_id()
    }
}

/// Drive a Noise_XX handshake to completion over a bidirectional byte
/// stream.
///
/// `io_send` and `io_recv` are async closures used to exchange handshake
/// messages: `io_send(msg)` is called when we have a message to send,
/// `io_recv()` is awaited to read the next message from the peer.
///
/// The role dictates the order:
/// - Initiator: send → recv → send.
/// - Responder: recv → send → recv.
pub async fn run_noise_handshake<I, O, FutIn, FutOut>(
    role: Role,
    local_keypair: &Keypair,
    mut io_send: I,
    mut io_recv: O,
) -> WebRtcResult<NoiseSession>
where
    I: FnMut(Vec<u8>) -> FutOut,
    O: FnMut() -> FutIn,
    FutIn: std::future::Future<Output = WebRtcResult<Vec<u8>>>,
    FutOut: std::future::Future<Output = WebRtcResult<()>>,
{
    let builder = Builder::new(NOISE_PATTERN.parse().expect("valid pattern"))
        .local_private_key(&local_keypair.private);

    let mut state = match role {
        Role::Initiator => builder.build_initiator().map_err(noise_err)?,
        Role::Responder => builder.build_responder().map_err(noise_err)?,
    };

    let mut buf = [0u8; MAX_NOISE_MSG];
    let mut payload_buf = [0u8; MAX_NOISE_MSG];

    // Drive the three-message exchange.
    let steps: &[NoiseStep] = match role {
        Role::Initiator => &[NoiseStep::Send, NoiseStep::Recv, NoiseStep::Send],
        Role::Responder => &[NoiseStep::Recv, NoiseStep::Send, NoiseStep::Recv],
    };

    for step in steps {
        match step {
            NoiseStep::Send => {
                let len = state
                    .write_message(&[], &mut buf)
                    .map_err(|e| WebRtcError::Noise(format!("write msg: {e}")))?;
                io_send(buf[..len].to_vec()).await?;
            }
            NoiseStep::Recv => {
                let msg = io_recv().await?;
                if msg.len() > MAX_NOISE_MSG {
                    return Err(WebRtcError::Noise(format!(
                        "handshake msg too large: {} > {MAX_NOISE_MSG}",
                        msg.len()
                    )));
                }
                state
                    .read_message(&msg, &mut payload_buf)
                    .map_err(|e| WebRtcError::Noise(format!("read msg: {e}")))?;
            }
        }
    }

    let remote_static_bytes = state
        .get_remote_static()
        .ok_or_else(|| WebRtcError::Noise("no remote static key after handshake".into()))?
        .to_vec();

    let mut remote = [0u8; NODE_ID_BYTES];
    let copy_len = remote_static_bytes.len().min(NODE_ID_BYTES);
    remote[..copy_len].copy_from_slice(&remote_static_bytes[..copy_len]);

    let remote_static = StaticPub(remote);

    // Convert the handshake state into a transport state. snow's API
    // consumes `state` here. We wrap the single TransportState in a Mutex
    // and serialise both encrypt and decrypt onto it (each call holds the
    // lock for microseconds). This is correct because `TransportState`
    // is `!Sync` and not `Clone`; the lock makes it safe to share between
    // tasks.
    let transport = state.into_transport_mode().map_err(noise_err)?;

    Ok(NoiseSession {
        inner: Arc::new(Mutex::new(transport)),
        remote_static,
    })
}

#[derive(Debug, Clone, Copy)]
enum NoiseStep {
    Send,
    Recv,
}

fn noise_err(e: snow::Error) -> WebRtcError {
    WebRtcError::Noise(e.to_string())
}

/// Generate a fresh Noise static keypair.
pub fn generate_keypair() -> WebRtcResult<Keypair> {
    let builder = Builder::new(NOISE_PATTERN.parse().expect("valid pattern"));
    builder.generate_keypair().map_err(noise_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Duplex {
        inner: StdMutex<DuplexInner>,
    }

    #[derive(Default)]
    struct DuplexInner {
        queue: VecDeque<Vec<u8>>,
        waiter: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Duplex {
        fn push(&self, msg: Vec<u8>) {
            let mut guard = self.inner.lock().unwrap();
            guard.queue.push_back(msg);
            if let Some(w) = guard.waiter.take() {
                w.send(()).ok();
            }
        }

        async fn recv(self: Arc<Self>) -> Vec<u8> {
            loop {
                let waiter_rx = {
                    let mut guard = self.inner.lock().unwrap();
                    if let Some(msg) = guard.queue.pop_front() {
                        return msg;
                    }
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    guard.waiter = Some(tx);
                    rx
                };
                let _ = waiter_rx.await;
            }
        }
    }

    async fn drive_pair(a_kp: &Keypair, b_kp: &Keypair) -> (NoiseSession, NoiseSession) {
        let a_to_b = Arc::new(Duplex::default());
        let b_to_a = Arc::new(Duplex::default());

        let a_send = |msg: Vec<u8>| {
            let b_to_a = b_to_a.clone();
            async move {
                b_to_a.push(msg);
                Ok(())
            }
        };
        let a_recv = || {
            let a_to_b = a_to_b.clone();
            async move { Ok(a_to_b.clone().recv().await) }
        };
        let b_send = |msg: Vec<u8>| {
            let a_to_b = a_to_b.clone();
            async move {
                a_to_b.push(msg);
                Ok(())
            }
        };
        let b_recv = || {
            let b_to_a = b_to_a.clone();
            async move { Ok(b_to_a.clone().recv().await) }
        };

        let a_fut = run_noise_handshake(Role::Initiator, a_kp, a_send, a_recv);
        let b_fut = run_noise_handshake(Role::Responder, b_kp, b_send, b_recv);

        let (a, b) = tokio::join!(a_fut, b_fut);
        (a.expect("a handshake ok"), b.expect("b handshake ok"))
    }

    #[tokio::test]
    async fn handshake_yields_distinct_remote_ids() {
        // A and B derive *different* NodeIds from each other because
        // they each hash the *other's* static public key. This is the
        // property the test should verify — the IDs are deterministic
        // functions of the remote static key.
        let a_kp = generate_keypair().unwrap();
        let b_kp = generate_keypair().unwrap();
        let (a, b) = drive_pair(&a_kp, &b_kp).await;

        // A's view of B is the BLAKE3-32 of B's static public key.
        let b_pub_bytes: [u8; 32] = b_kp.public[..32].try_into().unwrap();
        let expected_a_sees_b = StaticPub(b_pub_bytes).to_node_id();
        // B's view of A is the BLAKE3-32 of A's static public key.
        let a_pub_bytes: [u8; 32] = a_kp.public[..32].try_into().unwrap();
        let expected_b_sees_a = StaticPub(a_pub_bytes).to_node_id();

        assert_eq!(a.remote_node_id(), expected_a_sees_b);
        assert_eq!(b.remote_node_id(), expected_b_sees_a);
        assert_ne!(a.remote_node_id(), b.remote_node_id());
    }

    #[tokio::test]
    async fn encrypt_decrypt_roundtrip() {
        let a_kp = generate_keypair().unwrap();
        let b_kp = generate_keypair().unwrap();
        let (a, b) = drive_pair(&a_kp, &b_kp).await;

        let ct = a.encrypt(b"hello").await.unwrap();
        let pt = b.decrypt(&ct).await.unwrap();
        assert_eq!(pt, b"hello");

        let ct2 = b.encrypt(b"world").await.unwrap();
        let pt2 = a.decrypt(&ct2).await.unwrap();
        assert_eq!(pt2, b"world");
    }

    #[test]
    fn node_id_derivation_is_stable() {
        // Generate a fixed-bytes keypair by hand to get a deterministic NodeId.
        // We don't need real crypto for this test — we just want to verify
        // that the BLAKE3 derivation is consistent.
        let kp = generate_keypair().unwrap();
        let bytes: [u8; 32] = kp.public[..32].try_into().expect("key is 32 bytes");
        let node_a = StaticPub(bytes).to_node_id();
        let node_b = StaticPub(bytes).to_node_id();
        assert_eq!(node_a, node_b);
    }
}
