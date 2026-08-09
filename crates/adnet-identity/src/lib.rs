//! `adnet-identity` — identity, signatures, and end-to-end encryption for ADNet.
//!
//! This crate is intentionally **separate from `adnet-types`** so that the
//! stable wire types stay free of heavy crypto dependencies. Anything that
//! touches wallets, signing, or application-layer encryption lives here.
//!
//! ## Design
//!
//! - **Wallet keys** are `secp256k1` (EVM-compatible). An ADNet wallet is a
//!   plain `secp256k1::SecretKey`; the public address is the last 20 bytes of
//!   `keccak256(pubkey_uncompressed[1..])`, matching `0x…` EVM addresses.
//! - **Encryption keys** are a *separate* `x25519-dalek` keypair. We do
//!   **not** reuse the wallet key for static encryption — that keeps key
//!   rotation, hardware-wallet migration, and audit trails simple.
//! - **Signatures** use EIP-191 `personal_sign` (`\x19Ethereum Signed Message:\n`
//!   prefix) so any EVM wallet can verify them.
//! - **Static envelopes** are ECIES-style: ephemeral X25519 ECDH → HKDF-SHA256
//!   → AES-256-GCM. Every envelope is a fresh `(ephemeral_pub, nonce, ct)` so
//!   no two ciphertexts for the same recipient share a key.
//!
//! The on-the-wire ticket format for a signed receipt is described in
//! `adnet-token` (which builds on this crate).
//!
//! ## What this crate does *not* do
//!
//! - It does **not** talk to any chain. Signing is local; verification is local.
//! - It does **not** manage BIP-39 mnemonics or HD derivation. The wallet is
//!   loaded from raw 32 bytes.
//! - It does **not** do ratchet / double-ratchet session encryption. Each
//!   envelope is a one-shot static encryption to the recipient's x25519
//!   public key.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod address;
pub mod announcement;
pub mod ecies;
pub mod envelope;
pub mod error;
pub mod signing;
pub mod treasury;
pub mod wallet;
pub mod x25519;

pub use address::{Address, EVM_ADDRESS_LEN};
pub use adnet_types::WalletAddress as ProtocolWalletAddress;
pub use announcement::{
    announcement_digest, sign_announcement, signature_with_scheme, verify_announcement,
};
pub use ecies::{EciesCiphertext, EciesError, EciesPublicKey, EciesSecretKey};
pub use envelope::{ADR_ENVELOPE_MAGIC, ADR_ENVELOPE_VERSION, EncryptedPayload, SealedEnvelope};
pub use error::{IdentityError, Result};
pub use signing::{EIP191_PREFIX, PERSONAL_SIGN_VERSION, PersonalSignature};
pub use treasury::{ReceiptWallet, ReceiptWalletView, Treasury, TreasuryView};
pub use wallet::{Wallet, WalletPublic};
pub use x25519::{X25519PublicKey, X25519SecretKey};

/// Scheme tag stored as the first byte of `signature` blobs in
/// [`adnet_types::Announcement`]. `0` = EIP-191 over secp256k1 (the
/// only scheme supported today). The high nibble is reserved for
/// future schemes (Ed25519, BLS, PQ).
pub const SIG_SCHEME_EIP191_SECP256K1: u8 = 0;

// -- interop with `adnet_types::WalletAddress` ---------------------------
//
// `adnet-types` deliberately defines its own `WalletAddress` (20 raw
// bytes, no crypto deps) so protocol crates don't pull in secp256k1.
// `adnet-identity` is where the crypto lives; we provide cheap lossless
// conversions in both directions.

impl From<Address> for ProtocolWalletAddress {
    fn from(a: Address) -> Self {
        ProtocolWalletAddress::from_bytes(*a.as_bytes())
    }
}

impl From<ProtocolWalletAddress> for Address {
    fn from(a: ProtocolWalletAddress) -> Self {
        // The bytes are the same length and we trust the type contract.
        let mut out = [0u8; 20];
        out.copy_from_slice(a.as_bytes());
        Address::from_bytes(out)
    }
}

#[cfg(test)]
mod interop_tests {
    use super::*;

    #[test]
    fn round_trip() {
        let a = Address::from_bytes([0x42u8; 20]);
        let w: ProtocolWalletAddress = a.into();
        let b: Address = w.into();
        assert_eq!(a, b);
    }
}
