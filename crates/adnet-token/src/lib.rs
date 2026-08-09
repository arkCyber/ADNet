#![forbid(unsafe_code)]
#![deny(unused_must_use)]

//! `adnet-token` — relay-billing tokens for ADNet.
//!
//! A token is a *self-contained*, signed payment pledge. It is **not** a live
//! chain transaction; it is a promise that the bearer will pay the relay
//! for some service. The relay accepts it without contacting any chain, and
//! later settles it out-of-band (today: an explicit claim step; tomorrow:
//! batched on-chain submission).
//!
//! ## Wire format
//!
//! Tokens serialize to a single printable string of the form
//!
//! ```text
//! adnet-token://<chain_id>/<contract_addr>/<token_addr>/<amount_atomic>/
//! <recipient_addr>/<nonce_hex>/<expiry_unix>/<r_hex><s_hex><v_dec>
//! ```
//!
//! or to a JSON / postcard binary form for in-process use. The URL form is
//! what a producer would put into a QR code or a deep link; the binary form
//! is what travels inside the ADNet gossip layer.
//!
//! ## Why a separate crate
//!
//! - `adnet-types` must stay free of crypto deps to keep the wire contract
//!   cheap to compile.
//! - `adnet-identity` owns the *primitives* (wallets, signatures). This crate
//!   owns the *application* of those primitives to relay billing.
//! - Relay operators may run a version without any token feature; this crate
//!   is opt-in by being a separate `path` dependency.

pub mod claim;
pub mod error;
pub mod pledge;
pub mod receipt;

pub use claim::Claim;
pub use error::{Result, TokenError};
pub use pledge::{MAX_AMOUNT_ATOMIC, Pledge, PledgeBody};
pub use receipt::{Receipt, ReceiptBody};
