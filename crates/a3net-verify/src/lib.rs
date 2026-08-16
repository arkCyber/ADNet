//! A3Net Formal Verification Suite
//!
//! This crate provides formal verification for A3Net protocols using:
//! - **TLA+** specifications in `verification/tla/`
//! - **Kani** model checker integration
//!
//! ## Verification Coverage
//!
//! | Protocol | TLA+ | Kani |
//! |----------|------|------|
//! | DHT/Kademlia | ✅ | ✅ |
//! | Gossip | ✅ | In Progress |
//! | Bitswap | ✅ | In Progress |
//!
//! ## Running Verification
//!
//! ### TLA+ Specifications
//!
//! ```bash
//! cd verification/tla
//! java -jar tla2tools.jar -deadlock -config DHT.cfg DHT.tla
//! ```
//!
//! ### Kani Model Checker
//!
//! ```bash
//! cargo kani --package a3net-verify
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod dht;
pub mod gossip;
pub mod bitswap;

/// Re-exports for convenience
pub use dht::{RoutingTable, KBucketEntry, xor_distance};

/// Version info
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
