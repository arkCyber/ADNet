//! Configuration for the self-hosted DNS server.
//!
//! The config is intentionally minimal: bind addresses, the zone
//! name, an optional upstream relay to federate with, and a path
//! to a state file where published IPNS records persist between
//! restarts. Everything else (per-record TTLs, EDNS sizing, etc.)
//! is derived from the zone's records.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsServerConfig {
    /// Bind address for the UDP+TCP DNS listener.
    pub bind: SocketAddr,
    /// Zone the server is authoritative for (e.g. `a3net.example`).
    pub zone: String,
    /// Optional upstream pkarr relay to fetch records from when a
    /// query misses locally. Useful during cold start.
    pub pkarr_relay: Option<String>,
    /// Path to the on-disk record journal.
    pub state_path: Option<PathBuf>,
    /// Hard timeout for any upstream lookup. DNS queries must be
    /// answered inside this window or fall back to NXDOMAIN.
    pub upstream_timeout: Duration,
    /// Worker count for the request executor.
    pub workers: usize,
}

impl Default for DnsServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:53".parse().expect("static socket"),
            zone: "a3net.local".into(),
            pkarr_relay: None,
            state_path: None,
            upstream_timeout: Duration::from_millis(500),
            workers: 4,
        }
    }
}

impl DnsServerConfig {
    pub fn with_zone(mut self, zone: impl Into<String>) -> Self {
        self.zone = zone.into();
        self
    }

    pub fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    pub fn with_state_path(mut self, p: PathBuf) -> Self {
        self.state_path = Some(p);
        self
    }

    pub fn with_pkarr_relay(mut self, url: impl Into<String>) -> Self {
        self.pkarr_relay = Some(url.into());
        self
    }
}