//! Bridge between a3chat-crypto's domain-level APIs and
//! `a3net-crypto`'s generic `KeyProvider` abstraction.
//!
//! ## Why this module exists
//!
//! `a3chat-crypto` ships higher-level primitives — Noise XX
//! sessions, Sender Keys, Argon2id KEK — that all need *some*
//! long-term identity key. Before this module existed, every
//! consumer rolled its own file-backed key store, which led to
//! three independent on-disk formats and three independent
//! rotation ceremonies.
//!
//! By relying on `a3net_crypto::KeyProvider` (the abstraction
//! shared by every A3Net sub-crate), `a3chat-crypto` gains:
//!
//! * the `FileKeyProvider` persistence helper used by the
//!   Tauri shell and the headless node — same on-disk format;
//! * a single `derive_user_root(passphrase, salt) -> EncryptionKey`
//!   helper that uses the OWASP-recommended Argon2id parameters
//!   shared across A3Net;
//! * `Arc<dyn KeyProvider>` so a runtime can swap memory-backed,
//!   file-backed, or HSM-backed providers without recompiling.
//!
//! This module is **thin on purpose**: it does not reimplement
//! the underlying crypto. It only adapts the A3Net types to
//! a3chat's id-style and exposes typed helpers to the rest of
//! the workspace.

use std::sync::Arc;

use a3net_crypto::key::EncryptionKey;
use a3net_crypto::kdf;
use a3net_crypto::provider::{KeyProvider, KeyStoreKdfParams};

use a3chat_core::id::UserId;

use crate::error::{CryptoError, CryptoResult};

/// Hard-coded label used when the a3chat E2E keyring wraps a
/// `KeyProvider`. The label is part of the key-derivation info,
/// so changing it would invalidate every persisted key — DO
/// NOT change without a migration.
pub const A3CHAT_ROOT_KEY_LABEL: &str = "a3chat/identity/root/v1";

/// Recommended Argon2id parameters for a3chat KEKs. Matches
/// `a3net_crypto::kdf::{ARGON2_MEM_COST_KIB,ARGON2_T_COST,ARGON2_P_COST}`
/// but pinned here as a `KeyStoreKdfParams` so a future bump in
/// a3net-crypto doesn't silently rotate every bundle on disk.
pub const A3CHAT_KDF_PARAMS: KeyStoreKdfParams = KeyStoreKdfParams {
    m_cost_kib: kdf::ARGON2_MEM_COST_KIB,
    t_cost: kdf::ARGON2_T_COST,
    p_cost: kdf::ARGON2_P_COST,
};

/// Build an `Arc<dyn KeyProvider>` for a3chat by loading (or
/// creating) a `FileKeyProvider` at `path`. The provider is
/// backed by Argon2id KDF and exposes the same API surface as
/// every other A3Net sub-crate.
///
/// If the file does not exist, a fresh one is initialised with
/// `passphrase`. If it exists, the passphrase is verified and
/// the inner key is derived.
pub fn file_key_provider(
    path: &std::path::Path,
    passphrase: &str,
) -> CryptoResult<Arc<dyn KeyProvider>> {
    use a3net_crypto::provider::FileKeyProvider;
    if path.exists() {
        let provider = FileKeyProvider::open(passphrase.as_bytes(), path)
            .map_err(|e| CryptoError::Argon2(format!("a3net_key_provider open: {e}")))?;
        Ok(Arc::new(provider))
    } else {
        // Ensure parent exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CryptoError::Argon2(format!("create_dir_all({}): {e}", parent.display()))
            })?;
        }
        let provider = FileKeyProvider::init(passphrase.as_bytes(), path)
            .map_err(|e| CryptoError::Argon2(format!("a3net_key_provider init: {e}")))?;
        Ok(Arc::new(provider))
    }
}

/// Derive a per-user root key from a passphrase + salt. Two
/// calls with the same triple `(user, passphrase, salt)`
/// produce byte-identical keys (deterministic — DO-178C §6.1).
pub fn derive_user_root(
    user: &UserId,
    passphrase: &str,
    salt: &[u8],
) -> CryptoResult<EncryptionKey> {
    let master = kdf::derive_argon2id(passphrase.as_bytes(), salt)
        .map_err(|e| CryptoError::Argon2(format!("derive_argon2id: {e}")))?;
    // Domain-separate per user with HKDF-SHA256.
    let info = format!("{A3CHAT_ROOT_KEY_LABEL}/user/{}", user.as_str());
    let derived = hkdf_derive(&master, info.as_bytes(), 32)?;
    EncryptionKey::from_bytes(&derived)
        .map_err(|e| CryptoError::Argon2(format!("EncryptionKey::from_bytes: {e}")))
}

/// HKDF-SHA256 wrapper. Pulled from `a3net-crypto`'s
/// `EncryptionKey::from_bytes` API (the actual implementation
/// lives in `a3net_crypto::kdf`).
fn hkdf_derive(ikm: &[u8], info: &[u8], out_len: usize) -> CryptoResult<Vec<u8>> {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = vec![0u8; out_len];
    hk.expand(info, &mut okm)
        .map_err(|e| CryptoError::Argon2(format!("hkdf expand: {e}")))?;
    Ok(okm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> UserId {
        UserId::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    }

    #[test]
    fn derive_user_root_is_deterministic() {
        let user = alice();
        let salt = b"a3chat-test-salt-32-bytes________";
        let k1 = derive_user_root(&user, "hunter2", salt).unwrap();
        let k2 = derive_user_root(&user, "hunter2", salt).unwrap();
        // Same input must produce the same AEAD seal (deterministic
        // for a given nonce-free seal? No — but we can prove
        // equality via a round-trip: seal with k1, open with k2).
        let pt = b"a3chat-test-plaintext";
        let ct = k1.seal(pt).unwrap();
        let back = k2.open(&ct).unwrap();
        assert_eq!(back, pt);
    }

    #[test]
    fn derive_user_root_differs_per_user() {
        let bob = UserId::from("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let salt = b"a3chat-test-salt-32-bytes________";
        let ka = derive_user_root(&alice(), "hunter2", salt).unwrap();
        let kb = derive_user_root(&bob, "hunter2", salt).unwrap();
        let pt = b"a3chat-test-plaintext";
        let ct = ka.seal(pt).unwrap();
        // bob's key must NOT decrypt alice's seal.
        assert!(kb.open(&ct).is_err(), "different users must have different keys");
    }

    #[test]
    fn kdf_params_are_stable() {
        // Regression guard: bumping these constants would rotate
        // every existing bundle on disk. If this test starts
        // failing, you owe the operator a migration plan.
        assert_eq!(A3CHAT_KDF_PARAMS.m_cost_kib, 19 * 1024);
        assert_eq!(A3CHAT_KDF_PARAMS.t_cost, 2);
    }

    #[test]
    fn file_key_provider_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let p1 = file_key_provider(&path, "hunter2").unwrap();
        // Generate + store a label, then read it back via the
        // derived Secret.
        let s1 = p1.derive("test-label").unwrap();
        let p2 = file_key_provider(&path, "hunter2").unwrap();
        let s2 = p2.derive("test-label").unwrap();
        // Same label produces the same DEK.
        assert_eq!(*s1.expose(), *s2.expose());
    }

    #[test]
    fn file_key_provider_rejects_wrong_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.json");
        let _ = file_key_provider(&path, "hunter2").unwrap();
        // Wrong passphrase must NOT panic; it must error.
        let r = file_key_provider(&path, "wrong");
        assert!(r.is_err(), "wrong passphrase must error");
    }

    #[test]
    fn file_key_provider_is_send_and_sync() {
        // Compile-time assertion.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn KeyProvider>>();
    }
}