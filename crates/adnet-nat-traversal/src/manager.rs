//! NAT traversal manager that orchestrates all traversal methods.
//!
//! The manager tries NAT traversal methods in order of preference:
//! 1. Direct connection (no NAT)
//! 2. UPnP port forwarding
//! 3. Hole punching
//! 4. TURN relay fallback

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::config::{NatConfig, NatType, PortMappingProtocol};
use crate::error::{NatError, NatResult};
use crate::stun::StunClient;
use crate::turn::{TurnClient, RelayAllocation};
use crate::upnp::UpnpClient;
use crate::hole_punch::HolePunch;

/// Discovered NAT information.
#[derive(Debug, Clone)]
pub struct NatInfo {
    /// The detected NAT type
    pub nat_type: NatType,
    /// Our local address
    pub local_addr: SocketAddr,
    /// Our public address (as seen from outside)
    pub public_addr: SocketAddr,
    /// Whether hole punching is supported
    pub supports_hole_punch: bool,
    /// Whether TURN is required
    pub requires_turn: bool,
    /// Active port mappings
    pub port_mappings: Vec<PortMappingInfo>,
    /// Discovery timestamp
    pub discovered_at: std::time::Instant,
}

impl NatInfo {
    /// Check if we can connect directly to peers.
    pub fn can_connect_direct(&self) -> bool {
        self.nat_type.supports_direct_p2p() && !self.port_mappings.is_empty()
    }

    /// Get the best external address for sharing with peers.
    pub fn external_addr(&self) -> SocketAddr {
        self.public_addr
    }
}

/// Information about an active port mapping.
#[derive(Debug, Clone)]
pub struct PortMappingInfo {
    /// Protocol
    pub protocol: PortMappingProtocol,
    /// Local port
    pub local_port: u16,
    /// External port
    pub external_port: u16,
    /// Description
    pub description: String,
    /// When it expires
    pub expires_at: Option<std::time::Instant>,
}

/// NAT traversal manager.
#[derive(Debug)]
pub struct NatTraversalManager {
    config: NatConfig,
    stun_client: Option<StunClient>,
    turn_client: Option<TurnClient>,
    upnp_client: Option<UpnpClient>,
    hole_punch: HolePunch,
    nat_info: Arc<RwLock<Option<NatInfo>>>,
    port_mappings: Arc<RwLock<HashMap<String, PortMappingInfo>>>,
}

impl NatTraversalManager {
    /// Create a new NAT traversal manager (synchronous).
    pub fn new(config: NatConfig) -> NatResult<Self> {
        let stun_client = if config.stun_enabled {
            Some(StunClient::new()?)
        } else {
            None
        };

        // TURN client must be created asynchronously, so we use None for now
        let turn_client = None;

        let upnp_client = if config.upnp_enabled {
            Some(UpnpClient::new()?)
        } else {
            None
        };

        Ok(Self {
            config,
            stun_client,
            turn_client,
            upnp_client,
            hole_punch: HolePunch::new(None),
            nat_info: Arc::new(RwLock::new(None)),
            port_mappings: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Initialize the TURN client (must be called async).
    pub async fn init_turn_client(&mut self) -> NatResult<()> {
        if self.config.turn_enabled && self.config.turn_server.is_some() {
            self.turn_client = Some(
                TurnClient::new(self.config.turn_server.as_ref().unwrap()).await?
            );
        }
        Ok(())
    }

    /// Discover NAT type and public address.
    pub async fn discover(&mut self) -> NatResult<NatInfo> {
        let mut info = NatInfo {
            nat_type: NatType::Unknown,
            local_addr: "0.0.0.0:0".parse().unwrap(),
            public_addr: "0.0.0.0:0".parse().unwrap(),
            supports_hole_punch: false,
            requires_turn: false,
            port_mappings: Vec::new(),
            discovered_at: std::time::Instant::now(),
        };

        // Get local address from STUN client
        if let Some(ref mut client) = self.stun_client {
            info.local_addr = client.local_addr();

            // Detect NAT type
            if self.config.detect_nat_type {
                let (nat_type, response) = client.detect_nat_type(&self.config.stun_servers).await?;
                info.nat_type = nat_type;
                info.public_addr = response.mapped_address;
            } else {
                // Just get public address
                match timeout(
                    Duration::from_millis(self.config.stun_timeout_ms),
                    client.binding_request(&self.config.stun_servers[0])
                ).await {
                    Ok(Ok(response)) => {
                        info.public_addr = response.mapped_address;
                        info.nat_type = NatType::Unknown;
                    }
                    _ => {}
                }
            }
        }

        info.supports_hole_punch = info.nat_type.supports_hole_punching();
        info.requires_turn = info.nat_type.requires_turn();

        // Try UPnP port forwarding
        if let Some(ref upnp) = self.upnp_client {
            if let Ok(devices) = upnp.discover().await {
                if let Some(device) = devices.first() {
                    // Try to add port mappings
                    for port in self.get_preferred_ports() {
                        let mapping = PortMappingInfo {
                            protocol: PortMappingProtocol::Tcp,
                            local_port: port,
                            external_port: port,
                            description: "ADNet Node".to_string(),
                            expires_at: None,
                        };

                        if upnp.add_port_mapping(
                            device,
                            port,
                            port,
                            PortMappingProtocol::Tcp,
                            "ADNet",
                            self.config.upnp_lease_seconds,
                        ).await.is_ok() {
                            let mut mappings = self.port_mappings.write().await;
                            mappings.insert(format!("tcp-{}", port), mapping.clone());
                            info.port_mappings.push(mapping);
                        }
                    }
                }
            }
        }

        // Store the discovered info
        let mut nat_info = self.nat_info.write().await;
        *nat_info = Some(info.clone());

        Ok(info)
    }

    /// Get preferred local ports for binding.
    fn get_preferred_ports(&self) -> Vec<u16> {
        let mut ports = Vec::new();
        let start = self.config.local_port_start;
        let end = self.config.local_port_end;

        for port in (start..end).step_by(100).take(10) {
            ports.push(port);
        }

        ports
    }

    /// Punch a hole to a peer.
    pub async fn punch_hole(
        &self,
        peer_id: &str,
        local_socket: &tokio::net::UdpSocket,
        rendezvous_addr: SocketAddr,
    ) -> NatResult<crate::hole_punch::HolePunchResult> {
        self.hole_punch
            .punch_udp(peer_id, local_socket, rendezvous_addr, None)
            .await
    }

    /// Get a TURN relay allocation.
    pub async fn get_turn_allocation(&self) -> NatResult<RelayAllocation> {
        let client = self.turn_client.as_ref()
            .ok_or_else(|| NatError::NoTraversalMethod)?;

        client.allocate().await
    }

    /// Get the discovered NAT info.
    pub async fn get_nat_info(&self) -> Option<NatInfo> {
        let info = self.nat_info.read().await;
        info.clone()
    }

    /// Check if direct connection is possible.
    pub async fn can_connect_directly(&self) -> bool {
        let info = self.nat_info.read().await;
        if let Some(ref info) = *info {
            !info.requires_turn && !info.port_mappings.is_empty()
        } else {
            false
        }
    }

    /// Determine the best connection method to a peer.
    pub async fn get_connection_method(&self) -> ConnectionMethod {
        let info = self.nat_info.read().await;

        let info = match &*info {
            Some(i) => i,
            None => return ConnectionMethod::Discover,
        };

        if info.nat_type == NatType::OpenInternet {
            ConnectionMethod::Direct
        } else if info.requires_turn {
            ConnectionMethod::TurnRelay
        } else if info.supports_hole_punch {
            ConnectionMethod::HolePunch
        } else {
            ConnectionMethod::Discover
        }
    }

    /// Clean up resources.
    pub async fn shutdown(&mut self) {
        // Remove all UPnP port mappings
        if let Some(ref upnp) = self.upnp_client {
            if let Ok(devices) = upnp.discover().await {
                if let Some(device) = devices.first() {
                    let mappings = self.port_mappings.read().await;
                    for (key, mapping) in mappings.iter() {
                        let _ = upnp.remove_port_mapping(
                            device,
                            mapping.external_port,
                            mapping.protocol,
                        ).await;
                        tracing::info!("Removed UPnP mapping: {}", key);
                    }
                }
            }
        }

        // Close TURN allocation
        if let Some(turn) = self.turn_client.as_ref() {
            turn.close().await;
        }
    }
}

/// Connection method to a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMethod {
    /// Need to discover NAT first
    Discover,
    /// Direct connection possible
    Direct,
    /// Use UDP hole punching
    HolePunch,
    /// Must use TURN relay
    TurnRelay,
    /// Use existing relay
    Relay,
}

impl std::fmt::Display for ConnectionMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionMethod::Discover => write!(f, "discover"),
            ConnectionMethod::Direct => write!(f, "direct"),
            ConnectionMethod::HolePunch => write!(f, "hole_punch"),
            ConnectionMethod::TurnRelay => write!(f, "turn_relay"),
            ConnectionMethod::Relay => write!(f, "relay"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nat_traversal_manager_creation() {
        let config = NatConfig::default();
        let manager = NatTraversalManager::new(config);
        assert!(manager.is_ok());
    }

    #[test]
    fn test_connection_method_display() {
        assert_eq!(ConnectionMethod::Direct.to_string(), "direct");
        assert_eq!(ConnectionMethod::HolePunch.to_string(), "hole_punch");
        assert_eq!(ConnectionMethod::TurnRelay.to_string(), "turn_relay");
    }

    #[tokio::test]
    async fn test_nat_info_direct_connection() {
        let info = NatInfo {
            nat_type: NatType::FullCone,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.1:5000".parse().unwrap(),
            supports_hole_punch: true,
            requires_turn: false,
            port_mappings: vec![
                PortMappingInfo {
                    protocol: PortMappingProtocol::Tcp,
                    local_port: 5000,
                    external_port: 5000,
                    description: "ADNet".to_string(),
                    expires_at: None,
                }
            ],
            discovered_at: std::time::Instant::now(),
        };

        assert!(info.can_connect_direct());
    }

    #[tokio::test]
    async fn test_nat_info_requires_turn() {
        let info = NatInfo {
            nat_type: NatType::Symmetric,
            local_addr: "192.168.1.100:5000".parse().unwrap(),
            public_addr: "203.0.113.1:12345".parse().unwrap(),
            supports_hole_punch: false,
            requires_turn: true,
            port_mappings: vec![],
            discovered_at: std::time::Instant::now(),
        };

        assert!(!info.can_connect_direct());
        assert!(info.requires_turn);
    }
}
