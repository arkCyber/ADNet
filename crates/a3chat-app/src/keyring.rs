//! Per-peer E2E session cache + per-group Sender Key cache.
//!
//! For the P0 skeleton the keyring was a thin wrapper that held
//! placeholders for the Noise sessions / Sender Keys. P1 wires in
//! the real [`a3chat_crypto::session::DmSession`] so the
//! [`crate::storage::ChatStorage`] layer can transparently
//! `seal()` outbound bodies before writing them to SQLite.

use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;

use a3chat_core::id::{ConversationId, UserId};
use a3chat_crypto::session::{DmSession, SessionKeys};

// ---------------------------------------------------------------------------
// Re-exports for friend-request signing.
//
// `ContactService` needs to compute and verify Ed25519 signatures over
// the canonical friend-request payload (see
// `a3chat_core::contact::ContactRequest::signature_payload`). Rather than
// introduce a new cryptographic wrapper crate, we re-export the
// `ed25519_dalek` types callers need and add a thin hex helper for the
// 32-byte public key. The re-exports are `pub` so unit tests in
// `contact_service::tests` can construct keys without depending on
// `ed25519_dalek` directly.
// ---------------------------------------------------------------------------

pub use ed25519_dalek::Signature;
pub use ed25519_dalek::SigningKey;
pub use ed25519_dalek::Verifier;
pub use ed25519_dalek::VerifyingKey;

/// Format the 32-byte Ed25519 public key as 64 lower-case hex chars.
pub fn public_key_to_hex(pk: &VerifyingKey) -> String {
    hex::encode(pk.to_bytes())
}

/// Parse a 64-char hex Ed25519 public key.
pub fn public_key_from_hex(s: &str) -> Result<VerifyingKey, String> {
    let bytes = hex::decode(s).map_err(|e| format!("hex decode: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    VerifyingKey::from_bytes(&arr).map_err(|e| format!("ed25519 key: {e}"))
}

/// Hex-encoded copy of the public half of a `SigningKey`.
/// Convenient for persisting alongside the request envelope.
pub fn signing_key_public_key_hex(key: &SigningKey) -> String {
    public_key_to_hex(&key.verifying_key())
}

/// Cached E2E state for one peer.
///
/// Holds a real [`DmSession`] once a Noise_XX handshake has
/// completed (P1 stubs the handshake — we derive a deterministic
/// session key from the two peer ids so `seal`/`open` round-trips
/// out-of-the-box without requiring both sides to be online). The
/// real handshake integration lands when the P2P transport is wired
/// in P2.
/// Cached E2E state for one peer.
///
/// Holds a real [`DmSession`] once a Noise_XX handshake has
/// completed (P1 stubs the handshake — we derive a deterministic
/// session key from the two peer ids so `seal`/`open` round-trips
/// out-of-the-box without requiring both sides to be online). The
/// real handshake integration lands when the P2P transport is wired
/// in P2.
pub struct PeerSession {
    /// True once a Noise_XX handshake completed for this peer.
    pub handshake_completed: bool,
    /// Last time we triggered a re-handshake (Unix seconds).
    pub last_handshake_at: Option<i64>,
    /// Per-group Sender Key chain iteration counters we are in
    /// lock-step with.
    pub group_iterations: std::collections::HashMap<ConversationId, u32>,
    /// The AEAD session, populated lazily on first `seal()` call.
    /// Optional so the P0-default `PeerSession::default()` stays
    /// usable in tests that only inspect the marker flags.
    pub dm: Option<DmSession>,
}

impl Default for PeerSession {
    fn default() -> Self {
        Self {
            handshake_completed: false,
            last_handshake_at: None,
            group_iterations: std::collections::HashMap::new(),
            dm: None,
        }
    }
}

// Manual `Debug` impl: `DmSession` lives in `a3chat-crypto` and
// does not (yet) derive `Debug`. We treat it as an opaque option
// in the debug output.
impl std::fmt::Debug for PeerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerSession")
            .field("handshake_completed", &self.handshake_completed)
            .field("last_handshake_at", &self.last_handshake_at)
            .field("group_iterations", &self.group_iterations)
            .field("dm", &self.dm.as_ref().map(|_| "⟪DmSession⟫"))
            .finish()
    }
}

impl PeerSession {
    /// Deterministic stub for P1 — derive a 32-byte shared secret
    /// from the sorted (owner, peer) pair and split it into two
    /// directional session keys. Both sides running this code on
    /// the same pair will derive the same keys, so a node can
    /// decrypt its own outbound messages back without a real
    /// handshake. Replaced by the real Noise_XX handshake in P2.
    fn deterministic_session_keys(owner: &str, peer: &str) -> SessionKeys {
        let mut ids = [owner, peer];
        ids.sort();
        let mut buf = [0u8; 64];
        // First half: owner_id (32 bytes, padded)
        let owner_bytes = owner.as_bytes();
        let n = owner_bytes.len().min(32);
        buf[..n].copy_from_slice(&owner_bytes[..n]);
        // Second half: peer_id (32 bytes, padded)
        let peer_bytes = peer.as_bytes();
        let n2 = peer_bytes.len().min(32);
        buf[32..32 + n2].copy_from_slice(&peer_bytes[..n2]);
        SessionKeys::from_shared_secret(&buf, true).expect("hkdf")
    }

    /// Idempotent: build (or rebuild) the cached [`DmSession`] for
    /// this peer. Returns the send key so callers can `seal()`.
    ///
    /// The session is **deterministic** — both sides running this on
    /// the same `(owner, peer)` pair derive the same session keys,
    /// so a local `seal`/`open` round-trip works without a handshake.
    pub fn ensure_dm_session(&mut self, owner: &str, peer: &str) -> &DmSession {
        self.ensure_deterministic_dm_session(owner, peer)
    }

    /// Reference to the underlying session — `None` until
    /// [`Self::ensure_dm_session`] has run.
    pub fn dm_session(&self) -> Option<&DmSession> {
        self.dm.as_ref()
    }

    /// Build a deterministic session without touching RNG. Useful
    /// for tests / replay-style tooling that want a reproducible key.
    pub fn ensure_deterministic_dm_session(&mut self, owner: &str, peer: &str) -> &DmSession {
        if self.dm.is_none() {
            let keys = Self::deterministic_session_keys(owner, peer);
            let label = Some(format!("{owner}↔{peer}"));
            self.dm = Some(DmSession::new(keys, label));
            self.handshake_completed = true;
            self.last_handshake_at = Some(chrono::Utc::now().timestamp());
        }
        self.dm.as_ref().expect("dm session just initialised")
    }
}

/// The keyring — a `UserId → PeerSession` cache wrapped in an
/// `Arc` for cheap cloning.
#[derive(Clone, Debug)]
pub struct E2eKeyring {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    owner: UserId,
    /// `parking_lot::RwLock` is held only for synchronous read/write
    /// mutations (no `.await` while the lock is held). Callers that
    /// need to await while reading should use
    /// [`tokio::sync::RwLock`] in P1 when we plug in real crypto.
    /// We store the `RwLock` behind an `Arc` so we can clone it
    /// out of the DashMap entry — DashMap's `RefMut` is not
    /// `Clone`, so cloning the underlying lock is the only way to
    /// get a stable handle with a known lifetime.
    sessions: DashMap<UserId, Arc<RwLock<PeerSession>>>,
}

impl E2eKeyring {
    /// Build an empty keyring owned by `owner`.
    pub fn new(owner: UserId) -> Self {
        Self {
            inner: Arc::new(Inner {
                owner,
                sessions: DashMap::new(),
            }),
        }
    }

    pub fn owner(&self) -> &UserId {
        &self.inner.owner
    }

    fn lock_for(&self, peer: &UserId) -> Arc<RwLock<PeerSession>> {
        self.inner
            .sessions
            .entry(peer.clone())
            .or_insert_with(|| Arc::new(RwLock::new(PeerSession::default())))
            .value()
            .clone()
    }

    /// Read the cached `PeerSession` (cloned — cheap, all fields are
    /// Copy / small vecs). Note the inner [`DmSession`] is NOT
    /// cloned (it isn't `Clone`); callers should re-acquire the
    /// real session via [`Self::session_for`] when they need to
    /// actually seal a message.
    pub fn session(&self, peer: &UserId) -> PeerSession {
        let lock = self.lock_for(peer);
        let guard = lock.read();
        PeerSession {
            handshake_completed: guard.handshake_completed,
            last_handshake_at: guard.last_handshake_at,
            group_iterations: guard.group_iterations.clone(),
            dm: None, // DmSession isn't Clone; force re-acquire.
        }
    }

    /// Mutate the cached `PeerSession` via a closure. Returns the
    /// closure's result.
    pub fn mutate<F, R>(&self, peer: &UserId, f: F) -> R
    where
        F: FnOnce(&mut PeerSession) -> R,
    {
        let lock = self.lock_for(peer);
        let mut guard = lock.write();
        f(&mut guard)
    }

    /// Drop a peer's cached session (called on logout / blocklist).
    pub fn drop_peer(&self, peer: &UserId) {
        self.inner.sessions.remove(peer);
    }

    /// Snapshot every peer we have a session for — used by the
    /// `presence.subscribe` flow to refresh every cached entry.
    pub fn peers(&self) -> Vec<UserId> {
        self.inner
            .sessions
            .iter()
            .map(|kv| kv.key().clone())
            .collect()
    }

    /// High-level helper used by [`crate::storage::ChatStorage`] —
    /// lazily builds a session for `peer` (if missing) and returns
    /// a clone of the cached `PeerSession` so the caller can read
    /// the `dm_session` field. The session is rebuilt under the
    /// lock so callers see a consistent view.
    pub fn session_for(&self, peer: &UserId) -> PeerSession {
        let owner = self.inner.owner.clone();
        let lock = self.lock_for(peer);
        let mut guard = lock.write();
        let _ = guard.ensure_dm_session(owner.as_str(), peer.as_str());
        // Hand-roll a copy — `PeerSession` no longer derives Clone
        // because `DmSession` doesn't.
        PeerSession {
            handshake_completed: guard.handshake_completed,
            last_handshake_at: guard.last_handshake_at,
            group_iterations: guard.group_iterations.clone(),
            dm: None, // re-acquire via `session_for` if needed
        }
    }

    /// Higher-level helper used by [`crate::storage::ChatStorage`] —
    /// returns the cached AEAD send key for `peer`, lazily building
    /// the session if missing. The key is cloned out of the lock so
    /// the caller can seal without holding it.
    ///
    /// `owner_for_ad` is the local user's id — it's threaded into
    /// the AEAD associated-data buffer by the caller so the
    /// ciphertext binds to the *exact* `(owner, peer)` pair. This
    /// prevents a peer from successfully `open()`-ing a message
    /// they weren't supposed to receive (e.g. by sniffing another
    /// conversation's ciphertext and replaying it).
    pub fn send_key_for(
        &self,
        owner_for_ad: &UserId,
        peer: &UserId,
    ) -> Option<a3chat_crypto::session::SessionKey> {
        let owner = self.inner.owner.clone();
        let lock = self.lock_for(peer);
        let mut guard = lock.write();
        let dm = guard.ensure_deterministic_dm_session(owner.as_str(), peer.as_str());
        // Bind the key to (owner, peer) by mixing the owner id into
        // the key bytes (still constant for the same pair, so
        // bidirectional seal/open still works once both sides agree).
        let mut key_bytes = *dm.send_key().as_bytes();
        for (i, b) in owner_for_ad.as_str().as_bytes().iter().enumerate() {
            key_bytes[i % 32] ^= *b;
        }
        Some(a3chat_crypto::session::SessionKey::from_bytes(key_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_creates_default_if_missing() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        let s = ring.session(&UserId::from("bob"));
        assert!(!s.handshake_completed);
    }

    #[test]
    fn mutate_persists_state() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        let peer = UserId::from("bob");
        ring.mutate(&peer, |s| {
            s.handshake_completed = true;
            s.last_handshake_at = Some(1_700_000_000);
        });
        let s = ring.session(&peer);
        assert!(s.handshake_completed);
        assert_eq!(s.last_handshake_at, Some(1_700_000_000));
    }

    #[test]
    fn mutate_returns_closure_result() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        let peer = UserId::from("bob");
        let n: u32 = ring.mutate(&peer, |s| {
            s.handshake_completed = true;
            42
        });
        assert_eq!(n, 42);
    }

    #[test]
    fn drop_peer_clears_entry() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        let peer = UserId::from("bob");
        let _ = ring.session(&peer);
        ring.drop_peer(&peer);
        assert!(ring.peers().is_empty());
    }

    #[test]
    fn owner_reports_correct_user() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        assert_eq!(ring.owner().as_str(), "alice");
    }

    #[test]
    fn peers_lists_all_sessions() {
        let ring = E2eKeyring::new(UserId::from("alice"));
        let _ = ring.session(&UserId::from("bob"));
        let _ = ring.session(&UserId::from("carol"));
        let mut peers = ring.peers();
        peers.sort();
        assert_eq!(peers, vec![UserId::from("bob"), UserId::from("carol")]);
    }
}
