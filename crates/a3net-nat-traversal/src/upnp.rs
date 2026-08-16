//! UPnP IGD (Internet Gateway Device) client implementation.
//!
//! UPnP allows automatic port forwarding on routers that support it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::config::{PortMappingProtocol, NatConfig};
use crate::error::{NatError, NatResult};

/// UPnP IGD client for port forwarding.
#[derive(Debug, Clone)]
pub struct UpnpClient {
    multicast_addr: SocketAddr,
    search_timeout: Duration,
}

impl UpnpClient {
    /// Create a new UPnP client.
    pub fn new() -> NatResult<Self> {
        Ok(Self {
            multicast_addr: SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)),
                1900,
            ),
            search_timeout: Duration::from_secs(5),
        })
    }

    /// Discover UPnP IGD devices on the network.
    pub async fn discover(&self) -> NatResult<Vec<UpnpDevice>> {
        use tokio::net::UdpSocket;

        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| NatError::Upnp { reason: e.to_string() })?;

        // Send M-SEARCH multicast
        let search_msg = b"M-SEARCH * HTTP/1.1\r\n\
            HOST: 239.255.255.250:1900\r\n\
            MAN: \"ssdp:discover\"\r\n\
            MX: 3\r\n\
            ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";

        socket.send_to(search_msg, self.multicast_addr)
            .await
            .map_err(|e| NatError::Upnp { reason: e.to_string() })?;

        // Collect responses
        let mut devices = Vec::new();
        let mut buf = [0u8; 4096];

        // Use tokio timeout for the receive loop
        let deadline = std::time::Instant::now() + self.search_timeout;
        while std::time::Instant::now() < deadline {
            let remaining = deadline - std::time::Instant::now();
            let read_future = socket.recv_from(&mut buf);

            match tokio::time::timeout(remaining, read_future).await {
                Ok(Ok((len, addr))) => {
                    let response = String::from_utf8_lossy(&buf[..len]);
                    if let Some(device) = self.parse_device_response(&response, addr) {
                        devices.push(device);
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("UPnP discovery error: {}", e);
                    break;
                }
                Err(_) => {
                    // Timeout - stop waiting
                    break;
                }
            }
        }

        if devices.is_empty() {
            return Err(NatError::Upnp { reason: "No UPnP IGD devices found".to_string() });
        }

        Ok(devices)
    }

    /// Parse a device response from M-SEARCH.
    fn parse_device_response(&self, response: &str, addr: std::net::SocketAddr) -> Option<UpnpDevice> {
        let mut location = None;
        let mut usn = None;

        for line in response.lines() {
            if line.to_uppercase().starts_with("LOCATION:") {
                location = line.strip_prefix("LOCATION:").map(|s| s.trim().to_string());
            } else if line.to_uppercase().starts_with("USN:") {
                usn = line.strip_prefix("USN:").map(|s| s.trim().to_string());
            }
        }

        location.map(|loc| UpnpDevice {
            addr,
            location: loc,
            usn,
            services: Vec::new(),
        })
    }

    /// Get the WAN IP from a device.
    pub async fn get_wan_ip(&self, _device: &UpnpDevice) -> NatResult<IpAddr> {
        // In production, would parse device description and call GetExternalIPAddress
        Ok(Ipv4Addr::new(0, 0, 0, 0).into())
    }

    /// Add a port mapping.
    pub async fn add_port_mapping(
        &self,
        device: &UpnpDevice,
        internal_port: u16,
        external_port: u16,
        protocol: PortMappingProtocol,
        _description: &str,
        lease_seconds: u32,
    ) -> NatResult<()> {
        // In production, would send SOAP request to AddPortMapping
        tracing::info!(
            "UPnP: Would add port mapping {}:{}/{:?} -> {} (lease: {}s)",
            device.location,
            external_port,
            protocol,
            internal_port,
            lease_seconds
        );
        Ok(())
    }

    /// Remove a port mapping.
    pub async fn remove_port_mapping(
        &self,
        device: &UpnpDevice,
        external_port: u16,
        protocol: PortMappingProtocol,
    ) -> NatResult<()> {
        tracing::info!(
            "UPnP: Would remove port mapping {}:{}/{:?}",
            device.location,
            external_port,
            protocol
        );
        Ok(())
    }

    /// Get all current port mappings.
    pub async fn get_port_mappings(
        &self,
        _device: &UpnpDevice,
    ) -> NatResult<Vec<PortMapping>> {
        Ok(Vec::new())
    }
}

impl Default for UpnpClient {
    fn default() -> Self {
        Self::new().expect("Failed to create UPnP client")
    }
}

/// Represents a discovered UPnP device.
#[derive(Debug, Clone)]
pub struct UpnpDevice {
    /// Address where the device responded
    pub addr: SocketAddr,
    /// Device description URL
    pub location: String,
    /// Unique Service Name
    pub usn: Option<String>,
    /// Available services
    pub services: Vec<String>,
}

impl UpnpDevice {
    /// Check if this device has WAN IP service.
    pub fn has_wan_ip_service(&self) -> bool {
        !self.services.is_empty()
    }
}

/// Represents a port mapping on the UPnP device.
#[derive(Debug, Clone)]
pub struct PortMapping {
    /// External port
    pub external_port: u16,
    /// Protocol (TCP or UDP)
    pub protocol: PortMappingProtocol,
    /// Internal client IP
    pub internal_client: Ipv4Addr,
    /// Internal port
    pub internal_port: u16,
    /// Mapping description
    pub description: String,
    /// Lease duration (0 = permanent)
    pub lease_duration: u32,
    /// Enabled
    pub enabled: bool,
}

impl PortMapping {
    /// Create a new port mapping.
    pub fn new(
        external_port: u16,
        protocol: PortMappingProtocol,
        internal_port: u16,
        description: &str,
    ) -> Self {
        Self {
            external_port,
            protocol,
            internal_client: Ipv4Addr::new(127, 0, 0, 1),
            internal_port,
            description: description.to_string(),
            lease_duration: 3600,
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_mapping_creation() {
        let mapping = PortMapping::new(
            8080,
            PortMappingProtocol::Tcp,
            3000,
            "A3Net Node",
        );

        assert_eq!(mapping.external_port, 8080);
        assert_eq!(mapping.internal_port, 3000);
        assert_eq!(mapping.description, "A3Net Node");
    }

    #[test]
    fn test_upnp_client_creation() {
        let client = UpnpClient::new();
        assert!(client.is_ok());
    }
}
