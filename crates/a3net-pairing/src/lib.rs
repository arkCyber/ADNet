//! `a3net-pairing` — secure device-pairing protocol for A3Net.
//!
//! The existing wire types (`a3net-types::SignedPeerTicket`,
//! `a3net-types::PeerTicket`, …) hand a peer ticket plus a wallet
//! signature around, but they do **not** guarantee that the signer
//! actually controls the transport identity (`NodeId`). That gap is
//! the focus of this crate.
//!
//! # Threat model
//!
//! - **MITM** on a captured QR code. A signed peer ticket alone is
//!   not enough: an attacker can produce a NodeId they own, sign a
//!   ticket over it, and ship a fake QR. The pairing ceremony must
//!   prove control of the transport identity itself.
//! - **Replay**. A captured signed ticket is valid forever in the
//!   current `SignedPeerTicket`. Pairing requests carry a fresh
//!   nonce + expiry + clock-skew window.
//! - **Privilege escalation**. Pairing today grants "everything";
//!   we expose a [`Capability`] bitfield so the issuer can scope a
//!   grant.
//! - **Lost device**. Pairing credentials are
//!   [`TrustedDeviceRecord`] entries in a [`TrustedDeviceStore`]
//!   keyed by `credential_id`. Lost devices are revoked by deleting
//!   the entry; the store is consulted at accept time.
//!
//! # Cryptography
//!
//! - Challenge-response proof of possession of the transport
//!   identity uses **Ed25519** because `NodeId` is the 32-byte
//!   public-key view of an Ed25519 secret key when the iroh
//!   transport is enabled (see `a3net-transport/src/iroh/identity.rs`
//!   — "EndpointId is copied byte-for-byte into A3Net's NodeId").
//! - Capability grants and issuer signatures use the **EIP-191**
//!   personal-sign path on a secp256k1 wallet, exactly like
//!   [`a3net_token::Pledge`] and the existing `SignedPeerTicket`.
//!   This means a UI can reuse the existing `Wallet::sign_personal`
//!   code path with no new key material.
//!
//! # Wire layout
//!
//! The crate is pure-data with no IO. Wire formats live alongside
//! the structs:
//!
//! - [`PairingInvitation`] — JSON, ~256 bytes; transport-agnostic.
//!   Used by [`crate`]'s QR (`a3net-pairing://...`) and email
//!   invitation flows.
//! - [`PairingRequest`] / [`PairingResponse`] — JSON, exchanged
//!   over the first auth frame on the iroh QUIC connection.
//!
//! # Feature flags
//!
//! The crate is intentionally feature-free; everything compiles by
//! default. `a3net-transport` depends on it with no extra cargo
//! features.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod capability;
pub mod code;
pub mod error;
pub mod invitation;
pub mod store;
pub mod transport_identity;
pub mod trusted_device;
pub mod wire;

// Re-export the signing Wallet so downstream crates (CLI, invite
// mailer, …) can build `SignedInvitation`s without having to also
// depend on `a3net-identity` for the type itself.
pub use a3net_identity::wallet::{Wallet, WalletPublic};

pub use capability::{Capability, CapabilitySet};
pub use code::InvitationCode;
pub use error::{PairingError, PairingResult};
pub use invitation::{InvitationPayload, SignedInvitation};
pub use store::{TrustedDeviceStore, TrustedDeviceStoreConfig};
pub use transport_identity::{
    MAX_TIMESTAMP_SKEW_SECONDS, PairingRequest, PairingRequestBuilder,
    PairingResponse, PairingResponseBuilder, TRANSPORT_SCHEME_ED25519, pairing_invitation_digest,
    pairing_request_digest, pairing_response_digest, verify_pairing_request,
    verify_pairing_response,
};
pub use trusted_device::{TrustedDeviceRecord, TrustedDeviceStatus};
pub use wire::PairingInvitation;
