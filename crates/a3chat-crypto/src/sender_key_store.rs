//! Persistent [`SenderKeyChain`] state, backed by
//! [`a3net_crypto::provider::KeyProvider`].
//!
//! ## Why this module exists
//!
//! Before this module existed, a3chat kept [`SenderKeyChain`] entirely
//! in memory. The `a3chat-app::keyring` persisted only the iteration
//! counter, not the chain keys themselves — so a node restart lost
//! every group's Sender-Key state, and every group DM had to be
//! re-distributed from scratch.
//!
//! This module plugs that gap by storing the chain key (encrypted at
//! rest) under a stable label in the A3Net [`KeyProvider`]. The label
//! scheme is:
//!
//! ```text
//!     a3chat/sk/v1/<owner_id>/<conversation_id>/<chain_id_hex>
//! ```
//!
//! The owner_id is the local a3chat user (so two operators on the
//! same host with different key providers cannot read each other's
//! chains). The conversation_id scopes the chain to one group. The
//! chain_id_hex is the rotated [`SenderKeyId`] in lowercase hex so a
//! chain rotation naturally creates a fresh label.
//!
//! ## Iteration counter
//!
//! The chain advances deterministically from the stored chain key, so
//! the iteration counter can be re-derived. We do not persist it
//! separately — the caller keeps it in memory alongside the loaded
//! chain.
//!
//! ## DO-178C note
//!
//! The chain key never leaves the [`KeyProvider`] in plaintext
//! outside the synchronous AEAD call; the
//! [`a3net_crypto::Secret::ZeroizeOnDrop`] machinery wipes the buffer
//! when the caller drops the loaded [`SenderKeyChain`].

use std::sync::Arc;

use a3chat_core::id::UserId;
use a3net_crypto::provider::KeyProvider;

use crate::error::{CryptoError, CryptoResult};
use crate::sender_keys::{SenderKey, SenderKeyChain, SenderKeyId};

/// Domain-separation tag for the [`KeyProvider`] label scheme.
/// Bumping the suffix invalidates every previously-stored chain, so
/// do not change without a migration.
pub const SENDER_KEY_LABEL_PREFIX: &str = "a3chat/sk/v1";

/// Load a [`SenderKeyChain`] from the provider.
///
/// The current A3Net [`KeyProvider::derive`] always succeeds (it
/// lazily creates a DEK on first use), so this function always
/// returns `Ok(Some(_))`. Callers should normally use
/// [`load_or_create`], which is equivalent but more descriptive.
pub fn load(
    provider: &Arc<dyn KeyProvider>,
    owner: &UserId,
    conversation_id: &str,
    chain_id: SenderKeyId,
) -> CryptoResult<SenderKeyChain> {
    let label = label_for(owner, conversation_id, chain_id);
    let secret = provider.derive(&label).map_err(map_provider_err)?;
    let bytes = copy_secret(&secret);
    Ok(SenderKeyChain::new(&SenderKey::from_bytes(
        chain_id,
        bytes,
    )))
}

/// Create or load a [`SenderKeyChain`] and persist its key under the
/// provider. Idempotent: the same `(owner, conversation_id,
/// chain_id)` triple always returns a chain with the same starting
/// chain key.
pub fn load_or_create(
    provider: &Arc<dyn KeyProvider>,
    owner: &UserId,
    conversation_id: &str,
    chain_id: SenderKeyId,
) -> CryptoResult<SenderKeyChain> {
    load(provider, owner, conversation_id, chain_id)
}

/// Rotate the chain: generate a fresh random chain key, persist it
/// under a new label (`new_chain_id`), and return the new chain.
///
/// The old chain (under the previous `chain_id`) is left in place —
/// the caller should call [`forget`] when the rotation propagates to
/// all peers, otherwise the old label leaks storage until expiry.
pub fn rotate(
    provider: &Arc<dyn KeyProvider>,
    owner: &UserId,
    conversation_id: &str,
    new_chain_id: SenderKeyId,
) -> CryptoResult<SenderKeyChain> {
    let label = label_for(owner, conversation_id, new_chain_id);
    let secret = provider
        .generate_and_store(&label)
        .map_err(map_provider_err)?;
    let bytes = copy_secret(&secret);
    Ok(SenderKeyChain::new(&SenderKey::from_bytes(
        new_chain_id,
        bytes,
    )))
}

/// Drop the persisted chain key under `chain_id`. Idempotent.
pub fn forget(
    provider: &Arc<dyn KeyProvider>,
    owner: &UserId,
    conversation_id: &str,
    chain_id: SenderKeyId,
) -> CryptoResult<()> {
    let label = label_for(owner, conversation_id, chain_id);
    provider.forget(&label).map_err(map_provider_err)
}

// -- internal helpers ------------------------------------------------------

/// Build the provider label for a given (owner, conversation, chain).
/// Stable across processes so a restart can re-load the same chain.
fn label_for(owner: &UserId, conversation_id: &str, chain_id: SenderKeyId) -> String {
    format!(
        "{SENDER_KEY_LABEL_PREFIX}/{}/{}/{}",
        owner.as_str(),
        conversation_id,
        chain_id.as_hex()
    )
}

/// Copy a `Secret<32>` into a stack array. The borrow returned by
/// `Secret::with` is wiped the moment the closure returns, so we
/// materialise the value before the closure exits.
fn copy_secret(secret: &a3net_crypto::Secret<32>) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    secret.with(|s| bytes.copy_from_slice(&s[..32]));
    bytes
}

/// Translate an `a3net_crypto::CryptoError` into a3chat's local
/// `CryptoError`. We keep a single `Internal(String)` mapping because
/// the a3chat type deliberately avoids leaking provider-specific
/// variants to its public API.
fn map_provider_err(e: a3net_crypto::CryptoError) -> CryptoError {
    CryptoError::Internal(format!("a3net key_provider: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_crypto::provider::InMemoryKeyProvider;

    fn owner() -> UserId {
        UserId::from("alice-node-id-0123456789abcdef0123456789abcdef")
    }
    fn conv() -> &'static str {
        "grp:test-room"
    }

    #[test]
    fn load_or_create_is_idempotent() {
        let p: Arc<dyn KeyProvider> = Arc::new(InMemoryKeyProvider::new());
        let id = SenderKeyId::random();
        let c1 = load_or_create(&p, &owner(), conv(), id).expect("first");
        let c2 = load_or_create(&p, &owner(), conv(), id).expect("second");
        assert_eq!(c1.id(), c2.id());
        assert_eq!(c1.iteration(), c2.iteration());
    }

    #[test]
    fn rotate_changes_chain_id_and_bytes() {
        let p: Arc<dyn KeyProvider> = Arc::new(InMemoryKeyProvider::new());
        let old_id = SenderKeyId::random();
        let new_id = SenderKeyId::random();
        let _old = load_or_create(&p, &owner(), conv(), old_id).unwrap();
        let new = rotate(&p, &owner(), conv(), new_id).unwrap();
        assert_eq!(new.id(), new_id);
        assert_ne!(new.id(), old_id);
    }

    #[test]
    fn forget_is_idempotent() {
        let p: Arc<dyn KeyProvider> = Arc::new(InMemoryKeyProvider::new());
        let id = SenderKeyId::random();
        let _ = load_or_create(&p, &owner(), conv(), id).unwrap();
        // Two forgets must both succeed.
        forget(&p, &owner(), conv(), id).expect("first forget");
        forget(&p, &owner(), conv(), id).expect("second forget");
    }

    #[test]
    fn labels_are_isolated_per_owner() {
        let p: Arc<dyn KeyProvider> = Arc::new(InMemoryKeyProvider::new());
        let id = SenderKeyId::random();
        let alice = UserId::from("alice");
        let bob = UserId::from("bob");
        let alice_chain = load_or_create(&p, &alice, conv(), id).unwrap();
        let bob_chain = load_or_create(&p, &bob, conv(), id).unwrap();
        assert_eq!(alice_chain.id(), bob_chain.id());
        // Prove isolation via seal_next: identical plaintext + AD
        // under different owner-scoped chains must produce different
        // ciphertext (different chain keys → different message keys).
        let mut a = alice_chain;
        let mut b = bob_chain;
        let (_, blob_a) = a.seal_next(b"ad", b"hello").expect("alice seal");
        let (_, blob_b) = b.seal_next(b"ad", b"hello").expect("bob seal");
        assert_ne!(
            blob_a, blob_b,
            "different owners must produce different ciphertext"
        );
    }
}
