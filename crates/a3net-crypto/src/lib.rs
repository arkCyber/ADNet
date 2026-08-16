//! `a3net-crypto` — unified cryptographic primitives for A3Net.
//!
//! This crate is the canonical home for every crypto primitive that is
//! **not** specific to a higher-level concern (storage, gossip, identity).
//! Downstream crates (`a3net-blobstore`, `a3net-agent`, `a3net-ffi`,
//! …) depend on this layer so that:
//!
//! * **One error type** (`CryptoError`) describes every cryptographic
//!   failure mode across the workspace.
//! * **One key type** (`EncryptionKey`) is shared, so cross-crate
//!   helpers (e.g. an FFI shim) never have to translate between
//!   near-identical `EncryptionKey` definitions living in
//!   `a3net-blobstore` and `a3net-backup`.
//! * **One key file format** (`KeyFile`) is on-disk, so a node's
//!   `storage.encrypt.*` config and a future `agent.chat.encrypt.*`
//!   config can share the same `<data_dir>/keys/…` layout.
//!
//! ## Threat model
//!
//! `a3net-crypto` defends against an attacker who can read arbitrary
//! files inside `<data_dir>` but cannot run arbitrary code on the
//! host. It does **not** defend against memory dumps, side-channel
//! observation, or a malicious operator with root. Hardware-backed
//! isolation (TEE / iOS Keychain / Android Keystore) is layered on top
//! by a future `KeyProvider` trait — see the roadmap in
//! `ROADMAP.md`.
//!
//! ## Layout
//!
//! ```text
//! a3net-crypto
//! ├── aead       — raw XChaCha20-Poly1305 seal/open, AEAD overhead const
//! ├── kdf        — Argon2id parameters + derivation helpers
//! ├── key        — EncryptionKey (32-byte, Zeroize-on-Drop), KeyWriteAccess
//! ├── store      — KeyStore (file-backed) + KeyFile JSON format
//! ├── envelope   — X25519+AES-GCM sealed envelope (sibling API surface
//!                  for cross-crate E2E; implemented in a follow-up PR)
//! └── error      — CryptoError, CryptoResult
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod aead;
pub mod error;
pub mod kdf;
pub mod key;
pub mod provider;
pub mod secret;
pub mod store;

pub use error::{CryptoError, CryptoResult};
pub use key::{EncryptionKey, KeyWriteAccess};
pub use provider::{
    FileKeyProvider, InMemoryKeyProvider, KeyProvider, KeyStoreFile, KeyStoreKdf,
    KeyStoreKdfParams, KEYSTORE_FILE_VERSION, KEYSTORE_MAGIC, WrappedLabel, arc,
};
pub use secret::Secret;
pub use store::{CURRENT_KEY_VERSION, KeyFile, KeyFileKdf, KeyFileKdfParams, KeyStore};

pub use aead::AEAD_OVERHEAD;
