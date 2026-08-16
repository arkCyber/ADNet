//! `a3net-nat-traversal` — NAT traversal for A3Net.
//!
//! Provides comprehensive NAT traversal capabilities:
//!
//! - **STUN** (RFC 5389): Discover public IP and NAT type
//! - **TURN** (RFC 5766): Relay traffic when direct connection fails
//! - **UPnP IGD**: Automatically configure port forwarding
//! - **Hole Punching**: UDP/TCP symmetric NAT hole punching
//!
//! ## Architecture
//!
//! ```text
//! +-------------------+
//! |   Application     |
//! +--------+----------+
//!          |
//! +--------v----------+
//! | NatTraversalMgr  | <-- Orchestrates all NAT traversal methods
//! +--------+----------+
//!          |
//! +--------v----------+ +--------v----------+ +--------v----------+
//! |     STUN         | |     TURN         | |     UPnP         |
//! | (Discovery)      | | (Relay)          | | (Port Mapping)   |
//! +------------------+ +------------------+ +------------------+
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use a3net_nat_traversal::{NatTraversalManager, NatConfig};
//!
//! let config = NatConfig::default();
//! let manager = NatTraversalManager::new(config);
//!
//! // Discover NAT type and public endpoints
//! let info = manager.discover().await?;
//! println!("Public IP: {}", info.public_ip);
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod stun;
pub mod turn;
pub mod upnp;
pub mod hole_punch;
pub mod config;
pub mod error;
pub mod manager;

pub use config::{NatConfig, NatType, PortMappingProtocol, StunServer};
pub use error::{NatError, NatResult};
pub use manager::{NatInfo, NatTraversalManager};
pub use stun::{StunClient, StunResponse};
pub use turn::{TurnClient, TurnCredentials};
pub use upnp::{UpnpClient, PortMapping};
pub use hole_punch::{HolePunch, HolePunchResult};
