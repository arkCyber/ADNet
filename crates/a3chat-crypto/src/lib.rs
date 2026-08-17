//! `a3chat-crypto` — end-to-end encryption for a3chat.
//!
//! Layers:
//!
//! - [`session`]      — Noise_XX handshake + ChaCha20-Poly1305 AEAD session
//! - [`sender_keys`]  — Signal-style Sender Keys for group chat
//! - [`kek`]          — Argon2id KEK for cross-device bundle backup
//! - [`random`]       — secure-random helpers (nonces, salts)
//! - [`error`]        — [`CryptoError`] unifying every failure mode
//! - [`key_provider`] — bridge to `a3net-crypto::KeyProvider`
//!                      (so a3chat shares the same on-disk key
//!                      format as every other A3Net sub-crate)
//!
//! ## Cryptographic design (frozen at v0)
//!
//! - **Algorithm**: `chacha20-poly1305-v1` (IETF variant, 12-byte nonce,
//!   16-byte tag).
//! - **Key derivation**: HKDF-SHA256 over the shared secret with
//!   `info = "a3chat/dm/session/v1"` or `"a3chat/group/sender_key/v1"`.
//! - **Forward secrecy**: each session is short-lived. Re-handshakes
//!   happen every N messages (P1 default = 100) or every 24 h.
//! - **Authentication**: AEAD tag. No separate HMAC.
//!
//! See `A3CHAT_DESIGN.md` §4 for the threat model.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod error;
pub mod kek;
pub mod key_provider;
pub mod random;
pub mod sender_keys;
pub mod session;

pub use error::{CryptoError, CryptoResult};
pub use key_provider::{
    A3CHAT_KDF_PARAMS, A3CHAT_ROOT_KEY_LABEL, derive_user_root, file_key_provider,
};
pub use kek::{EncryptedBundle, KdfParams, decrypt_bundle, derive_kek, encrypt_bundle};
pub use random::{random_bytes, random_nonce};
pub use sender_keys::{
    SenderKey, SenderKeyChain, SenderKeyDistribution, SenderKeyId, step_sender_key as StepSenderKey,
};
pub use session::{
    DmSession, HandshakeMessage, SessionKey, SessionKeys,
    handshake_initiator, handshake_responder,
    initiator_first_message, initiator_final_message, responder_second_message,
    open, seal,
};
