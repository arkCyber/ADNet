//! `a3net-magicdns` — name resolution for the A3Net mesh VPN.
//!
//! ## What it does
//!
//! A mesh member is reachable by name through three forms:
//!
//! - `<hostname>.<network>.ray` — full form, equivalent to
//!   `alice.gaming.ray`. The `.ray` TLD is the canonical
//!   marker for a mesh network.
//! - `<hostname>.<network>` — short form, useful for
//!   non-public resolvers.
//! - `<hostname>.ray` — flat lookup that walks every
//!   network the local node belongs to and returns the
//!   first match.
//!
//! The resolver takes a (network, hostname) tuple and
//! returns the deterministic [`VirtualIp`] of the member
//! owning that hostname. There is no allocation per query
//! beyond the cache lookup; the cache is bounded by the
//! mesh membership size and invalidated when the
//! coordinator publishes a roster update.
//!
//! ## What it does NOT do
//!
//! - It is **not** a UDP DNS server. The daemon binary
//!   uses [`crate::Resolver`] as a library; binding
//!   `:53` and forwarding `*.ray` queries to it is a
//!   separate concern (see `a3net-cli`).
//! - It does not implement recursive resolution. A query
//!   for `foo.bar.baz.ray` is rejected as malformed; the
//!   mesh name space is `<host>.<net>.ray` or
//!   `<host>.ray` only.
//!
//! ## Wire format
//!
//! `MagicQuery` is the parsed view of a single name
//! resolution request. It is what the daemon's
//! `resolve_query()` accepts.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod error;
pub mod forwarder;
pub mod query;
pub mod resolver;
pub mod server;

pub use config::ResolverConfig;
pub use error::{MagicError, MagicResult};
pub use forwarder::{ForwarderError, ForwarderResult, TunDnsForwarder};
pub use query::{MagicName, MagicQuery, MAX_NAME_LEN, TLD_SUFFIX};
pub use resolver::{Resolver, ResolverSnapshot};
pub use server::{DnsServer, DnsServerHandle, ServeError};
