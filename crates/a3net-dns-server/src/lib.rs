//! `a3net-dns-server` — Self-hostable authoritative DNS server for A3Net.
//!
//! This crate serves a pkarr-compatible zone to public DNS resolvers.
//! Two kinds of records are served:
//!
//!   * `_a3net.<ipns-name>.<zone>` → TXT record carrying the IPNS
//!     payload as base64 (so any open DNS resolver can fetch it).
//!   * `<relay-name>.<zone>` → A/AAAA records advertising the
//!     embedded A3Net relay on this host.
//!
//! ## Why a separate crate
//!
//! Pkarr is *federated*: a record published to a public relay is
//! visible to every other relay. Most operators don't want that —
//! they want to host their own zone (`*.a3net.example`) and let the
//! public pkarr relays serve that zone alongside `pkarr.pub`'s free
//! zone. This crate is the missing server-side counterpart.
//!
//! ## ACKing against the Iroh / iroh-dns-server conventions
//!
//! Wire format is identical to `iroh-dns-server`'s pkarr server:
//! the same TXT payload bytes are served at the same key. Operators
//! that already run `iroh-dns-server` can switch to this crate and
//! keep their existing zone data files.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod server;
pub mod zone;
pub mod http;

pub use config::DnsServerConfig;
pub use server::DnsServerHandle;
pub use zone::ZoneStore;