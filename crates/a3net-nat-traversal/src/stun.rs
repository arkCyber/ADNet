//! STUN client implementation (RFC 5389).
//!
//! STUN (Session Traversal Utilities for NAT) is used to:
//! - Discover the public IP address behind a NAT
//! - Determine the type of NAT (full cone, restricted, symmetric, etc.)
//! - Keep NAT bindings alive

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::config::{NatType, StunServer};
use crate::error::{NatError, NatResult};

/// STUN message types.
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_BINDING_ERROR: u16 = 0x0111;

/// STUN attribute types.
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_ATTR_SOURCE_ADDRESS: u16 = 0x0004;
const STUN_ATTR_CHANGED_ADDRESS: u16 = 0x0005;

/// Magic cookie for STUN.
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// STUN client for NAT type detection.
#[derive(Debug)]
pub struct StunClient {
    local_addr: SocketAddr,
}

impl StunClient {
    /// Create a new STUN client.
    pub fn new() -> NatResult<Self> {
        let local_addr = "0.0.0.0:0".parse().unwrap();
        Ok(Self { local_addr })
    }

    /// Get the local address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Perform a STUN binding request.
    pub async fn binding_request(&mut self, server: &StunServer) -> NatResult<StunResponse> {
        // Create a new socket for each request
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        socket.set_nonblocking(false).ok();
        self.local_addr = socket.local_addr().unwrap_or(self.local_addr);

        // Build STUN binding request
        let mut transaction_id = [0u8; 12];
        getrandom::getrandom(&mut transaction_id)
            .map_err(|e| NatError::Stun { reason: e.to_string() })?;

        let mut msg = Vec::with_capacity(32);
        msg.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(&transaction_id);

        // Send request
        socket.send_to(&msg, server.addr)
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        // Receive response
        let mut buf = [0u8; 1024];
        let (bytes_read, from) = socket.recv_from(&mut buf)
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        // Parse response
        self.parse_response(&buf[..bytes_read], from)
    }

    /// Parse STUN response.
    fn parse_response(&self, data: &[u8], from: SocketAddr) -> NatResult<StunResponse> {
        if data.len() < 20 {
            return Err(NatError::Stun { reason: "Response too short".to_string() });
        }

        let msg_type = u16::from_be_bytes([data[0], data[1]]);
        if msg_type != STUN_BINDING_RESPONSE {
            return Err(NatError::Stun { reason: format!("Unexpected response type: {:x}", msg_type) });
        }

        let mapped_addr = self.find_xor_mapped_address(data)?;
        let source_addr = self.find_attribute(data, STUN_ATTR_SOURCE_ADDRESS)?;
        let changed_addr = self.find_attribute(data, STUN_ATTR_CHANGED_ADDRESS)?;

        Ok(StunResponse {
            server: from,
            mapped_address: mapped_addr,
            source_address: source_addr,
            changed_address: changed_addr,
        })
    }

    /// Find XOR-mapped address in response.
    fn find_xor_mapped_address(&self, data: &[u8]) -> NatResult<SocketAddr> {
        self.find_attribute(data, STUN_ATTR_XOR_MAPPED_ADDRESS)
    }

    /// Find an address attribute.
    fn find_attribute(&self, data: &[u8], attr_type: u16) -> NatResult<SocketAddr> {
        let mut offset = 20;

        while offset + 4 < data.len() {
            let type_ = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 4;

            if type_ == attr_type && length >= 8 {
                let family = data[offset];
                if family == 0x01 {
                    let port = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);

                    if attr_type == STUN_ATTR_XOR_MAPPED_ADDRESS {
                        let xor_port = port ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
                        let xor_addr = u32::from_be_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]);
                        let addr = xor_addr ^ STUN_MAGIC_COOKIE;
                        return Ok(SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::from(addr)), xor_port));
                    } else {
                        let port = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
                        let addr = u32::from_be_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]);
                        return Ok(SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::from(addr)), port));
                    }
                }
            }

            offset += length;
            offset = (offset + 3) & !3;
        }

        Err(NatError::Stun { reason: "Address attribute not found".to_string() })
    }

    /// Detect NAT type using multiple STUN servers.
    pub async fn detect_nat_type(&mut self, servers: &[StunServer]) -> NatResult<(NatType, StunResponse)> {
        if servers.is_empty() {
            return Err(NatError::NatTypeDetection { reason: "No STUN servers provided".to_string() });
        }

        // Step 1: Basic binding request to get mapped address
        let primary = self.binding_request(&servers[0]).await?;

        // Step 2: If local and mapped addresses differ, we're behind NAT
        let local_ip = match self.local_addr.ip() {
            std::net::IpAddr::V4(ip) => ip,
            _ => return Ok((NatType::OpenInternet, primary)),
        };

        let mapped_ip = match primary.mapped_address.ip() {
            std::net::IpAddr::V4(ip) => ip,
            _ => return Ok((NatType::OpenInternet, primary)),
        };

        if local_ip == mapped_ip {
            return Ok((NatType::OpenInternet, primary));
        }

        // Step 3: Test for symmetric NAT (different port per destination)
        if servers.len() > 1 {
            let secondary = self.binding_request(&servers[1]).await?;

            if primary.mapped_address.port() != secondary.mapped_address.port() {
                return Ok((NatType::Symmetric, primary));
            }

            if primary.mapped_address.ip() != secondary.mapped_address.ip() {
                return Ok((NatType::Symmetric, primary));
            }
        }

        // Step 4: Default to port-restricted cone NAT
        Ok((NatType::PortRestrictedCone, primary))
    }
}

impl Default for StunClient {
    fn default() -> Self {
        Self::new().expect("Failed to create STUN client")
    }
}

/// STUN binding response.
#[derive(Debug, Clone)]
pub struct StunResponse {
    /// STUN server that responded
    pub server: SocketAddr,
    /// Our mapped address (public IP:port)
    pub mapped_address: SocketAddr,
    /// Address on STUN server where response came from
    pub source_address: SocketAddr,
    /// Alternative address where we could send requests
    pub changed_address: SocketAddr,
}

impl StunResponse {
    /// Check if we're behind a NAT.
    pub fn is_behind_nat(&self, local: SocketAddr) -> bool {
        self.mapped_address.ip() != local.ip() || self.mapped_address.port() != local.port()
    }

    /// Get the public IP address.
    pub fn public_ip(&self) -> std::net::IpAddr {
        self.mapped_address.ip()
    }

    /// Get the public port.
    pub fn public_port(&self) -> u16 {
        self.mapped_address.port()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nat_type_descriptions() {
        assert_eq!(NatType::OpenInternet.supports_direct_p2p(), true);
        assert_eq!(NatType::Symmetric.supports_direct_p2p(), false);
        assert_eq!(NatType::Symmetric.requires_turn(), true);
        assert_eq!(NatType::OpenInternet.requires_turn(), false);
    }

    #[test]
    fn test_stun_response() {
        let response = StunResponse {
            server: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            mapped_address: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 12345),
            source_address: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 3478),
            changed_address: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 3478),
        };

        let local = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 54321);
        assert!(response.is_behind_nat(local));

        let public_local = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 12345);
        assert!(!response.is_behind_nat(public_local));
    }
}
