//! NAT traversal configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Supported NAT types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NatType {
    /// No NAT detected - directly reachable from internet
    OpenInternet,
    /// Full cone NAT - any external host can send to internal host
    FullCone,
    /// Restricted cone NAT - external host must have been contacted first
    RestrictedCone,
    /// Port restricted cone NAT - restricts by port as well
    PortRestrictedCone,
    /// Symmetric NAT - different external port per destination
    Symmetric,
    /// Unknown NAT type
    Unknown,
}

impl NatType {
    /// Check if direct peer-to-peer connection is possible.
    pub fn supports_direct_p2p(&self) -> bool {
        matches!(
            self,
            NatType::OpenInternet
                | NatType::FullCone
                | NatType::RestrictedCone
                | NatType::PortRestrictedCone
        )
    }

    /// Check if hole punching is feasible.
    pub fn supports_hole_punching(&self) -> bool {
        !matches!(self, NatType::Symmetric | NatType::Unknown)
    }

    /// Check if TURN relay is required.
    pub fn requires_turn(&self) -> bool {
        matches!(self, NatType::Symmetric | NatType::Unknown)
    }

    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            NatType::OpenInternet => "Direct internet connection (no NAT)",
            NatType::FullCone => "Full cone NAT - easy to traverse",
            NatType::RestrictedCone => "Restricted cone NAT - requires contact first",
            NatType::PortRestrictedCone => "Port-restricted cone NAT - harder to traverse",
            NatType::Symmetric => "Symmetric NAT - requires TURN relay",
            NatType::Unknown => "Unknown NAT type - assume worst case",
        }
    }
}

impl Default for NatType {
    fn default() -> Self {
        NatType::Unknown
    }
}

/// Protocol for port mappings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMappingProtocol {
    Tcp,
    Udp,
}

impl PortMappingProtocol {
    /// Get the default external port range start.
    pub fn default_port_start(&self) -> u16 {
        match self {
            PortMappingProtocol::Tcp => 40000,
            PortMappingProtocol::Udp => 40000,
        }
    }
}

/// STUN server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StunServer {
    /// Server address (host:port)
    pub addr: SocketAddr,
    /// Optional name for logging
    pub name: Option<String>,
}

impl StunServer {
    /// Create a new STUN server.
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr, name: None }
    }

    /// Create with a name.
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Get default public STUN servers.
    pub fn default_servers() -> Vec<Self> {
        vec![
            StunServer::new(([128, 6, 4, 6], 3478).into()).with_name("STUN1"),
            StunServer::new(([129, 6, 15, 1], 3478).into()).with_name("STUN2"),
            StunServer::new(([209, 132, 177, 10], 3478).into()).with_name("Google"),
            StunServer::new(([64, 69, 149, 139], 3478).into()).with_name("Twilio"),
        ]
    }
}

/// Configuration for NAT traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatConfig {
    /// Enable STUN for NAT type detection
    pub stun_enabled: bool,
    /// STUN servers to use
    pub stun_servers: Vec<StunServer>,
    /// STUN request timeout
    pub stun_timeout_ms: u64,
    /// STUN retries
    pub stun_retries: u32,

    /// Enable UPnP for port forwarding
    pub upnp_enabled: bool,
    /// UPnP lease duration in seconds
    pub upnp_lease_seconds: u32,
    /// Preferred external port (0 = auto)
    pub upnp_preferred_port: u16,

    /// Enable TURN as fallback
    pub turn_enabled: bool,
    /// TURN server URL
    pub turn_server: Option<String>,
    /// TURN credentials
    pub turn_username: Option<String>,
    /// TURN password
    pub turn_password: Option<String>,

    /// Enable hole punching
    pub hole_punch_enabled: bool,
    /// Hole punch timeout
    pub hole_punch_timeout_ms: u64,
    /// Hole punch retries
    pub hole_punch_retries: u32,

    /// Enable NAT type detection
    pub detect_nat_type: bool,

    /// Preferred local port range start
    pub local_port_start: u16,
    /// Preferred local port range end
    pub local_port_end: u16,
}

impl Default for NatConfig {
    fn default() -> Self {
        Self {
            stun_enabled: true,
            stun_servers: StunServer::default_servers(),
            stun_timeout_ms: 3000,
            stun_retries: 3,

            upnp_enabled: true,
            upnp_lease_seconds: 3600,
            upnp_preferred_port: 0,

            turn_enabled: true,
            turn_server: None,
            turn_username: None,
            turn_password: None,

            hole_punch_enabled: true,
            hole_punch_timeout_ms: 5000,
            hole_punch_retries: 5,

            detect_nat_type: true,

            local_port_start: 40000,
            local_port_end: 60000,
        }
    }
}

impl NatConfig {
    /// Create a new config with custom STUN servers.
    pub fn with_stun_servers(mut self, servers: Vec<StunServer>) -> Self {
        self.stun_servers = servers;
        self
    }

    /// Enable STUN.
    pub fn with_stun(mut self, enabled: bool) -> Self {
        self.stun_enabled = enabled;
        self
    }

    /// Enable UPnP.
    pub fn with_upnp(mut self, enabled: bool) -> Self {
        self.upnp_enabled = enabled;
        self
    }

    /// Enable TURN.
    pub fn with_turn(mut self, server: &str, username: &str, password: &str) -> Self {
        self.turn_enabled = true;
        self.turn_server = Some(server.to_string());
        self.turn_username = Some(username.to_string());
        self.turn_password = Some(password.to_string());
        self
    }

    /// Enable hole punching.
    pub fn with_hole_punch(mut self, enabled: bool) -> Self {
        self.hole_punch_enabled = enabled;
        self
    }

    /// Set port range.
    pub fn with_port_range(mut self, start: u16, end: u16) -> Self {
        self.local_port_start = start;
        self.local_port_end = end;
        self
    }
}
