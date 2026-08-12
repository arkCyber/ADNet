//! `adnet-qr` — QR payloads, parsers, and SVG rendering for ADNet.
//!
//! ## What this crate does
//!
//! - Parses QR payloads from the on-the-wire family used by Delta Chat /
//!   chatmail: [`DCACCOUNT`](https://github.com/deltachat/interface/blob/master/uri-schemes.md#DCACCOUNT),
//!   [`DCLOGIN`](https://github.com/deltachat/interface/blob/master/uri-schemes.md#DCLOGIN),
//!   [`DCBACKUP`](../qr/dclogin_scheme.rs), `OPENPGP4FPR:`, `mailto:`,
//!   `MATMSG:`, `BEGIN:VCARD`, and ad-hoc URL / text fallbacks.
//! - Renders QR codes as SVG using the [`qrcodegen`] crate (same library
//!   chatmail@core uses), so ADNet produces Delta-Chat-interoperable
//!   visual output.
//! - Adds ADNet-native payloads on top: share bundles for peer / blob
//!   tickets (`adnet-peer://`, `adnet-blob://`, signed peer tickets) and
//!   relay-payment pledges (`adnet-token://`).
//! - With the `mail` feature, converts a `dclogin:` payload into an
//!   `adnet_mail::Account` so a UI flow can dial SMTP / IMAP without
//!   re-implementing the parser.
//!
//! ## What this crate does *not* do
//!
//! - It does **not** decode scanned images. Pair it with a QR-camera
//!   crate at the call site; this crate is pure-data.
//! - It does **not** call out to chatmail@core. The implementation is a
//!   clean-room port of the public URI scheme specs (linked above) and
//!   the public chatmail@core source. License is MPL-2.0 to match
//!   upstream — see `LICENSE-MPL-2.0` in this crate.
//!
//! ## Crate layout
//!
//! - [`error`]           — typed error (`QrError`).
//! - [`payload`]         — typed [`payload::QrPayload`] enum (the parsed result).
//! - [`scan`]            — top-level [`scan::check_qr`] entry point.
//! - [`generator`]       — SVG generator [`generator::create_qr_svg`].
//! - [`adnet`]           — `adnet-…` URL parsers and encoders for ADNet tickets/tokens.
//! - [`chatmail`]        — `DCACCOUNT` / `DCLOGIN` / `DCBACKUP` parsers.
//! - [`dclogin_scheme`]  — `dclogin:` parser (split out for unit tests).
//!
//! ## Example
//!
//! ```rust
//! use adnet_qr::{scan, payload::QrPayload};
//!
//! let raw = "mailto:alice@example.com?subject=Hi&body=Hello%20there";
//! let parsed = scan::check_qr(raw).unwrap();
//! assert!(matches!(parsed, QrPayload::Email { .. }));
//!
//! // Render the same payload as an SVG.
//! let svg = adnet_qr::generator::create_qr_svg(raw).unwrap();
//! assert!(svg.starts_with("<svg"));
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod adnet;
pub mod chatmail;
pub mod dclogin_scheme;
pub mod error;
pub mod generator;
pub mod payload;
pub mod scan;

pub use error::{QrError, Result};
pub use payload::QrPayload;
pub use scan::check_qr;
