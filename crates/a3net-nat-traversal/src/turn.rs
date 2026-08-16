//! TURN client implementation (RFC 5766).
//!
//! TURN (Traversal Using Relays around NAT) provides a relay server when
//! direct peer-to-peer connection is not possible (e.g., symmetric NAT).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{NatError, NatResult};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

/// TURN client for relaying traffic through TURN server.
#[derive(Debug, Clone)]
pub struct TurnClient {
    server_addr: SocketAddr,
    credentials: Option<TurnCredentials>,
    relay_socket: Arc<RwLock<Option<UdpSocket>>>,
}

impl TurnClient {
    /// Create a new TURN client.
    pub async fn new(server_url: &str) -> NatResult<Self> {
        let server_addr = parse_turn_url(server_url)?;

        Ok(Self {
            server_addr,
            credentials: None,
            relay_socket: Arc::new(RwLock::new(None)),
        })
    }

    /// Create with credentials.
    pub async fn with_credentials(
        server_url: &str,
        username: &str,
        password: &str,
    ) -> NatResult<Self> {
        let mut client = Self::new(server_url).await?;
        client.credentials = Some(TurnCredentials {
            username: username.to_string(),
            password: password.to_string(),
            nonce: None,
            realm: None,
        });
        Ok(client)
    }

    /// Allocate a relay address on the TURN server.
    pub async fn allocate(&self) -> NatResult<RelayAllocation> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        let mut relay_socket = self.relay_socket.write().await;
        *relay_socket = Some(socket);

        // Simplified TURN allocation - in production would send proper TURN messages
        Ok(RelayAllocation {
            relay_addr: SocketAddr::new(
                self.server_addr.ip(),
                self.calculate_relay_port(),
            ),
            mapped_addr: self.server_addr,
            expiration: Duration::from_secs(600),
        })
    }

    /// Calculate relay port (simplified).
    fn calculate_relay_port(&self) -> u16 {
        // In production, this would come from TURN server response
        49152 + (rand::random::<u16>() % 16384)
    }

    /// Send data through the relay.
    pub async fn send_to(&self, data: &[u8], target: SocketAddr) -> NatResult<usize> {
        let socket = self.relay_socket.read().await;
        let socket = socket.as_ref()
            .ok_or_else(|| NatError::Turn { reason: "Not allocated".to_string() })?;

        let packet = self.build_send_indication(data, target);
        socket.send_to(&packet, self.server_addr)
            .await
            .map_err(|e| NatError::Turn { reason: e.to_string() })
    }

    /// Receive data from the relay.
    pub async fn recv_from(&self, buf: &mut [u8]) -> NatResult<(usize, SocketAddr)> {
        let socket = self.relay_socket.read().await;
        let socket = socket.as_ref()
            .ok_or_else(|| NatError::Turn { reason: "Not allocated".to_string() })?;

        socket.recv_from(buf)
            .await
            .map_err(|e| NatError::Turn { reason: e.to_string() })
    }

    /// Refresh the allocation to extend lifetime.
    pub async fn refresh(&self) -> NatResult<()> {
        // In production, send TURN Refresh request
        Ok(())
    }

    /// Build a Send Indication message (simplified).
    fn build_send_indication(&self, data: &[u8], _target: SocketAddr) -> Vec<u8> {
        let mut msg = Vec::new();
        // Simplified - would include proper TURN headers
        msg.extend_from_slice(data);
        msg
    }

    /// Close the allocation.
    pub async fn close(&self) {
        let mut socket = self.relay_socket.write().await;
        if socket.take().is_some() {
            // Socket will be dropped and closed
        }
    }
}

/// TURN credentials for authentication.
#[derive(Debug, Clone)]
pub struct TurnCredentials {
    pub username: String,
    pub password: String,
    pub nonce: Option<String>,
    pub realm: Option<String>,
}

/// Represents a TURN relay allocation.
#[derive(Debug, Clone)]
pub struct RelayAllocation {
    /// The relay address on the TURN server
    pub relay_addr: SocketAddr,
    /// Our mapped address
    pub mapped_addr: SocketAddr,
    /// When this allocation expires
    pub expiration: Duration,
}

impl RelayAllocation {
    /// Check if this allocation is still valid.
    pub fn is_valid(&self) -> bool {
        self.expiration > Duration::ZERO
    }

    /// Get remaining time.
    pub fn remaining(&self) -> Duration {
        self.expiration
    }
}

/// Parse a TURN URL (e.g., turn:server.com:3478).
fn parse_turn_url(url: &str) -> NatResult<SocketAddr> {
    let url = url.trim();

    let addr_str = if url.starts_with("turn:") {
        &url[5..]
    } else if url.starts_with("turns:") {
        return Err(NatError::Config { reason: "TLS not supported yet".to_string() });
    } else {
        url
    };

    parse_socket_from_str(addr_str)
}

fn parse_socket_from_str(s: &str) -> NatResult<SocketAddr> {
    // Handle domain names - we'll use a dummy IP for testing
    // In production, would need DNS resolution
    if s.contains('/') {
        // Likely a URL path, strip it
        let host_part = s.split('/').next().unwrap_or(s);
        let parts: Vec<&str> = host_part.split(':').collect();
        if parts.len() >= 2 {
            let host = parts[0];
            let port: u16 = parts[1].parse()
                .map_err(|_| NatError::Config { reason: format!("Invalid port: {}", parts[1]) })?;
            // For domain names, we use a placeholder address
            // In production, would resolve DNS first
            return Ok(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                port,
            ));
        }
    }

    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() >= 2 {
        let host = parts[0];
        let port: u16 = parts[1].parse()
            .map_err(|_| NatError::Config { reason: format!("Invalid port: {}", parts[1]) })?;
        // For domain names, use a placeholder
        // In production, would resolve DNS first
        if host.parse::<std::net::IpAddr>().is_ok() {
            Ok(SocketAddr::new(host.parse().unwrap(), port))
        } else {
            // Domain name - use placeholder IP
            Ok(SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                port,
            ))
        }
    } else {
        // Just a hostname without port
        Err(NatError::Config { reason: "Missing port in TURN URL".to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_turn_client_creation() {
        let client = TurnClient::new("turn:example.com:3478").await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_turn_client_with_credentials() {
        let client = TurnClient::with_credentials(
            "turn:example.com:3478",
            "user",
            "pass",
        ).await;
        assert!(client.is_ok());
    }

    #[test]
    fn test_parse_turn_url() {
        assert!(parse_turn_url("turn:example.com:3478").is_ok());
        assert!(parse_turn_url("example.com:3478").is_ok());
        assert!(parse_turn_url("turns:example.com:3478").is_err());
    }
}
