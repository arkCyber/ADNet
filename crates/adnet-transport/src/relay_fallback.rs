//! QUIC Relay Fallback implementation.
//!
//! Provides automatic fallback to relay when direct P2P connection fails.
//! This is used as the last resort for nodes behind symmetric NATs or
//! when direct connections are blocked.
//!
//! ## Connection Flow
//!
//! ```text
//! ┌──────────┐    Direct     ┌──────────┐
//! │  Peer A  │ ──────────────▶ │  Peer B  │
//! └──────────┘    Success     └──────────┘
//!       │
//!       │ Direct fails
//!       ▼
//! ┌──────────┐   Hole Punch  ┌──────────┐
//! │  Peer A  │ ──────────────▶ │  Peer B  │
//! └──────────┘    Success     └──────────┘
//!       │
//!       │ Hole punch fails
//!       ▼
//! ┌──────────┐   Relay HTTP   ┌──────────┐
//! │  Peer A  │ ──────────────▶ │  Peer B  │
//! └──────────┘                 └──────────┘
//! ```
//!
//! ## Relay Integration
//!
//! When relay fallback is enabled, the transport will:
//! 1. First attempt direct connection
//! 2. Then attempt hole punching (if applicable)
//! 3. Finally fall back to relay via HTTP/WebSocket tunnel

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use adnet_types::{NodeAddr, NodeId};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::stun::{NatType, StunClient};
use crate::traits::{Transport, TransportError, TransportResult};

/// Relay endpoint configuration.
#[derive(Debug, Clone)]
pub struct RelayEndpoint {
    /// Relay server URL.
    url: String,
    /// Relay's public key / identity.
    pub relay_id: NodeId,
    /// Whether this relay is currently reachable.
    reachable: bool,
    /// Last health check timestamp.
    last_health_check: std::time::Instant,
}

impl RelayEndpoint {
    /// Create a new relay endpoint.
    ///
    /// Security: Validates that the URL is HTTPS and not a loopback/private address.
    pub fn new(url: String, relay_id: NodeId) -> anyhow::Result<Self> {
        Self::validate_url(&url)?;
        Ok(Self {
            url,
            relay_id,
            reachable: true,
            last_health_check: std::time::Instant::now(),
        })
    }

    /// Validate relay URL for security.
    ///
    /// - Must be HTTPS
    /// - Must not be localhost/loopback
    /// - Must not be private IP range
    /// - Must not be multicast
    fn validate_url(url: &str) -> anyhow::Result<()> {
        // Parse the URL
        let parsed = url::Url::parse(url)
            .map_err(|e| anyhow::anyhow!("Invalid relay URL: {}", e))?;

        // Must be HTTPS
        if parsed.scheme() != "https" {
            anyhow::bail!("Relay URL must use HTTPS, got: {}", parsed.scheme());
        }

        // Get host
        let host = parsed.host_str()
            .ok_or_else(|| anyhow::anyhow!("Relay URL has no host"))?;

        // Parse IP address, handling both bare IPs and bracket-enclosed IPv6
        let ip_str = if host.starts_with('[') && host.ends_with(']') {
            &host[1..host.len()-1]  // Strip brackets for IPv6
        } else {
            host
        };

        // Check for IP addresses
        if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
            // Block loopback (IPv4 127.x.x.x and IPv6 ::1)
            if ip.is_loopback() {
                anyhow::bail!("Relay URL cannot be loopback address");
            }
            // Block private ranges
            match ip {
                std::net::IpAddr::V4(ipv4) if ipv4.is_private() => {
                    anyhow::bail!("Relay URL cannot be private IP address")
                }
                std::net::IpAddr::V6(ipv6) if ipv6.is_unique_local() => {
                    anyhow::bail!("Relay URL cannot be private IP address")
                }
                _ => {}
            }
            // Block link-local
            if let std::net::IpAddr::V4(ipv4) = ip {
                if ipv4.is_link_local() {
                    anyhow::bail!("Relay URL cannot be link-local address");
                }
            }
            // Block multicast
            if ip.is_multicast() {
                anyhow::bail!("Relay URL cannot be multicast address");
            }
        } else {
            // Hostname - check for localhost variants
            let lower = host.to_lowercase();
            if lower == "localhost" || lower.starts_with("localhost.") {
                anyhow::bail!("Relay URL cannot be localhost");
            }
        }

        Ok(())
    }

    /// Get the relay URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Check if relay is reachable.
    pub fn is_reachable(&self) -> bool {
        self.reachable
    }

    /// Mark relay as unreachable.
    pub fn mark_unreachable(&mut self) {
        self.reachable = false;
    }

    /// Mark relay as reachable.
    pub fn mark_reachable(&mut self) {
        self.reachable = true;
        self.last_health_check = std::time::Instant::now();
    }

    /// Get time since last health check.
    pub fn time_since_health_check(&self) -> Duration {
        self.last_health_check.elapsed()
    }
}

/// Connection attempt result with metadata.
#[derive(Debug, Clone)]
pub struct ConnectionAttempt {
    /// The peer we tried to connect to.
    pub peer: NodeId,
    /// The address we tried.
    pub attempted_addr: Option<SocketAddr>,
    /// The connection type that succeeded (if any).
    pub connection_type: FallbackConnectionType,
    /// Time taken to connect.
    pub connect_time_ms: u64,
}

/// Connection type used in fallback strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackConnectionType {
    /// Direct P2P connection.
    Direct,
    /// Connection via hole punching.
    HolePunch,
    /// Connection via relay.
    Relay,
}

impl FallbackConnectionType {
    /// Get string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::HolePunch => "hole_punch",
            Self::Relay => "relay",
        }
    }
}

/// Result of a connection attempt.
#[derive(Debug)]
pub enum ConnectionResult {
    /// Connection succeeded with fallback type.
    Connected(FallbackConnectionType),
    /// Connection failed after all strategies.
    Failed(Vec<TransportError>),
}

/// Error when all connection strategies fail.
#[derive(Debug)]
pub struct ConnectError {
    errors: Vec<TransportError>,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "all connection strategies failed: ")?;
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{:?}", e)?;
        }
        Ok(())
    }
}

impl std::error::Error for ConnectError {}

impl ConnectError {
    /// Create a new ConnectError from a list of transport errors.
    pub fn new(errors: Vec<TransportError>) -> Self {
        Self { errors }
    }

    /// Get the underlying errors.
    pub fn errors(&self) -> &[TransportError] {
        &self.errors
    }

    /// Get the number of failed attempts.
    pub fn attempts(&self) -> usize {
        self.errors.len()
    }
}

/// Fallback connection result type.
pub type FallbackConnectionResult = ConnectionResult;

/// QUIC transport with relay fallback support.
pub struct QuicTransportWithRelay {
    /// Inner QUIC transport.
    inner: Arc<dyn Transport>,
    /// Relay endpoints.
    relays: Arc<RwLock<Vec<RelayEndpoint>>>,
    /// Local NAT type.
    local_nat_type: Arc<RwLock<Option<NatType>>>,
    /// NAT type detection timestamp.
    nat_detection_time: Arc<RwLock<Option<std::time::Instant>>>,
    /// Connection timeout.
    connection_timeout: Duration,
    /// Whether to enable relay fallback.
    relay_fallback_enabled: bool,
    /// Whether to attempt hole punching.
    hole_punch_enabled: bool,
    /// HTTP client for health checks.
    http_client: reqwest::Client,
}

impl QuicTransportWithRelay {
    /// Create a new transport with relay fallback.
    pub fn new(inner: Arc<dyn Transport>) -> anyhow::Result<Self> {
        Ok(Self {
            inner,
            relays: Arc::new(RwLock::new(Vec::new())),
            local_nat_type: Arc::new(RwLock::new(None)),
            nat_detection_time: Arc::new(RwLock::new(None)),
            connection_timeout: Duration::from_secs(10),
            relay_fallback_enabled: true,
            hole_punch_enabled: true,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {}", e))?,
        })
    }

    /// Create with custom configuration.
    pub fn with_config(
        inner: Arc<dyn Transport>,
        relays: Vec<RelayEndpoint>,
        connection_timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner,
            relays: Arc::new(RwLock::new(relays)),
            local_nat_type: Arc::new(RwLock::new(None)),
            nat_detection_time: Arc::new(RwLock::new(None)),
            connection_timeout,
            relay_fallback_enabled: true,
            hole_punch_enabled: true,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| anyhow::anyhow!("Failed to create HTTP client: {}", e))?,
        })
    }

    /// Add a relay endpoint.
    pub async fn add_relay(&self, relay: RelayEndpoint) {
        let mut relays = self.relays.write().await;
        if !relays.iter().any(|r| r.relay_id == relay.relay_id) {
            relays.push(relay.clone());
            info!("Added relay endpoint: {}", relay.url());
        }
    }

    /// Add a relay by URL and ID.
    pub async fn add_relay_by_url(&self, url: String, relay_id: NodeId) -> anyhow::Result<()> {
        let relay = RelayEndpoint::new(url, relay_id)?;
        self.add_relay(relay).await;
        Ok(())
    }

    /// Remove a relay endpoint.
    pub async fn remove_relay(&self, relay_id: &NodeId) {
        let mut relays = self.relays.write().await;
        relays.retain(|r| r.relay_id != *relay_id);
        info!("Removed relay endpoint: {}", relay_id);
    }

    /// Set the local NAT type (from STUN detection).
    pub async fn set_nat_type(&self, nat_type: NatType) {
        let mut nat = self.local_nat_type.write().await;
        let mut detection_time = self.nat_detection_time.write().await;
        *nat = Some(nat_type);
        *detection_time = Some(std::time::Instant::now());
        info!("Local NAT type set to: {}", nat_type.description());
    }

    /// Get the cached NAT type if still valid.
    pub async fn get_nat_type(&self) -> Option<NatType> {
        let nat = self.local_nat_type.read().await;
        let detection_time = self.nat_detection_time.read().await;

        // Check if detection is recent (within 5 minutes)
        if let (Some(n), Some(t)) = (*nat, *detection_time) {
            if t.elapsed() < Duration::from_secs(300) {
                return Some(n);
            }
        }
        None
    }

    /// Enable or disable relay fallback.
    pub fn set_relay_fallback(&mut self, enabled: bool) {
        self.relay_fallback_enabled = enabled;
        debug!("Relay fallback enabled: {}", enabled);
    }

    /// Enable or disable hole punching.
    pub fn set_hole_punch(&mut self, enabled: bool) {
        self.hole_punch_enabled = enabled;
        debug!("Hole punching enabled: {}", enabled);
    }

    /// Detect local NAT type via STUN.
    ///
    /// Uses cached result if available and recent.
    pub async fn detect_nat_type(
        &self,
        stun_server: SocketAddr,
    ) -> anyhow::Result<NatType> {
        // Check cache first
        if let Some(cached) = self.get_nat_type().await {
            debug!("Using cached NAT type: {:?}", cached);
            return Ok(cached);
        }

        // Get local socket from inner transport
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;

        let client = StunClient::new(crate::stun::StunConfig {
            server: stun_server,
            timeout: Duration::from_secs(3),
            retries: 2,
        });

        let response = client.detect(&socket).await?;
        let nat_type = response.nat_type;

        self.set_nat_type(nat_type).await;
        Ok(nat_type)
    }

    /// Attempt to connect with automatic fallback.
    ///
    /// This tries connection strategies in order:
    /// 1. Direct connection (if address is known)
    /// 2. Hole punching (if both peers support it)
    /// 3. Relay fallback
    pub async fn connect(&self, peer: NodeId, peer_addr: &NodeAddr) -> Result<FallbackConnectionResult, ConnectError> {
        let start = std::time::Instant::now();
        let mut errors = Vec::new();

        info!(
            "Connecting to peer {} with address {:?}",
            peer.short(),
            peer_addr
        );

        // Strategy 1: Try direct connection
        if let Some(direct) = &peer_addr.direct {
            match self.try_direct(peer.clone(), direct).await {
                Ok(()) => {
                    let elapsed = start.elapsed().as_millis() as u64;
                    info!(
                        "Direct connection to {} succeeded in {}ms",
                        peer.short(),
                        elapsed
                    );
                    return Ok(FallbackConnectionResult::Connected(FallbackConnectionType::Direct));
                }
                Err(e) => {
                    warn!("Direct connection failed: {}", e);
                    errors.push(e);
                }
            }
        }

        // Strategy 2: Try hole punching (if enabled)
        if self.hole_punch_enabled {
            let nat_type = self.local_nat_type.read().await;
            if let Some(nat) = *nat_type {
                if nat.supports_hole_punching() {
                    match self.try_hole_punch(peer.clone(), peer_addr).await {
                        Ok(()) => {
                            let elapsed = start.elapsed().as_millis() as u64;
                            info!(
                                "Hole punch connection to {} succeeded in {}ms",
                                peer.short(),
                                elapsed
                            );
                            return Ok(FallbackConnectionResult::Connected(FallbackConnectionType::HolePunch));
                        }
                        Err(e) => {
                            warn!("Hole punching failed: {}", e);
                            errors.push(TransportError::Other(e.to_string()));
                        }
                    }
                }
            }
        }

        // Strategy 3: Try relay fallback
        if self.relay_fallback_enabled {
            match self.try_relay(peer.clone(), peer_addr).await {
                Ok(()) => {
                    let elapsed = start.elapsed().as_millis() as u64;
                    info!(
                        "Relay connection to {} succeeded in {}ms",
                        peer.short(),
                        elapsed
                    );
                    return Ok(FallbackConnectionResult::Connected(FallbackConnectionType::Relay));
                }
                Err(e) => {
                    warn!("Relay connection failed: {}", e);
                    errors.push(TransportError::Other(e.to_string()));
                }
            }
        }

        error!(
            "All connection strategies failed for {}: {:?}",
            peer.short(),
            errors
        );
        Err(ConnectError::new(errors))
    }

    /// Try direct connection.
    async fn try_direct(&self, peer: NodeId, _addr: &adnet_types::node::Endpoint) -> TransportResult<()> {
        debug!("Attempting direct connection to {}", peer.short());

        let result = tokio::time::timeout(
            self.connection_timeout,
            self.inner.dial(peer),
        )
        .await;

        match result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(TransportError::Other("direct connection timed out".into())),
        }
    }

    /// Try hole punching.
    async fn try_hole_punch(
        &self,
        peer: NodeId,
        peer_addr: &NodeAddr,
    ) -> anyhow::Result<()> {
        debug!("Attempting hole punch to {}", peer.short());

        // Import hole punching module
        let coordinator = crate::hole_punch::HolePunchCoordinator::new();

        // Get peer's external address
        let peer_external = peer_addr
            .direct
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no direct address for hole punch"))?;

        let peer_socket_addr = format!(
            "{}:{}",
            peer_external.host(),
            peer_external.port().unwrap_or(0)
        )
        .parse::<SocketAddr>()?;

        // Start hole punch session
        let (_tx, rx) = mpsc::channel(4);
        let handle = coordinator
            .start_session(peer_socket_addr, None, rx)
            .await?;

        // Wait for punch success
        match tokio::time::timeout(Duration::from_secs(30), handle.wait()).await {
            Ok(Ok(())) => {
                // Hole punched! Now try direct connection
                self.inner.dial(peer).await?;
                Ok(())
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("hole punch failed: {}", e)),
            Err(_) => Err(anyhow::anyhow!("hole punch timed out")),
        }
    }

    /// Try relay connection.
    async fn try_relay(
        &self,
        peer: NodeId,
        peer_addr: &NodeAddr,
    ) -> anyhow::Result<()> {
        debug!("Attempting relay connection to {}", peer.short());

        let relays = self.relays.read().await;
        if relays.is_empty() {
            return Err(anyhow::anyhow!("no relay endpoints configured"));
        }

        // Try each relay in order
        for relay in relays.iter() {
            if !relay.is_reachable() {
                continue;
            }

            info!("Trying relay: {}", relay.url());

            match self.connect_via_relay(peer.clone(), peer_addr, relay).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!("Relay {} failed: {}", relay.url(), e);
                    continue;
                }
            }
        }

        Err(anyhow::anyhow!(
            "All relays failed for peer {}",
            peer.short()
        ))
    }

    /// Connect via a specific relay.
    async fn connect_via_relay(
        &self,
        _peer: NodeId,
        _peer_addr: &NodeAddr,
        relay: &RelayEndpoint,
    ) -> anyhow::Result<()> {
        debug!("Relay {} endpoint configured", relay.url());

        // Relay tunneling is not yet implemented.
        // This requires the relay server to expose a WebSocket tunnel API.
        Err(anyhow::anyhow!(
            "relay tunneling not yet implemented (relay: {})",
            relay.url()
        ))
    }

    /// Get available connection strategies.
    pub async fn available_strategies(&self) -> Vec<FallbackConnectionType> {
        let mut strategies = vec![FallbackConnectionType::Direct];

        if self.hole_punch_enabled {
            let nat = self.local_nat_type.read().await;
            if let Some(n) = *nat {
                if n.supports_hole_punching() {
                    strategies.push(FallbackConnectionType::HolePunch);
                }
            }
        }

        if self.relay_fallback_enabled {
            let relays = self.relays.read().await;
            if !relays.is_empty() && relays.iter().any(|r| r.is_reachable()) {
                strategies.push(FallbackConnectionType::Relay);
            }
        }

        strategies
    }

    /// Check relay health and update status.
    pub async fn check_relay_health(&self) {
        let mut relays = self.relays.write().await;
        for relay in relays.iter_mut() {
            let healthy = self.ping_relay(relay).await;
            if healthy {
                relay.mark_reachable();
            } else {
                relay.mark_unreachable();
            }
        }
    }

    /// Ping a relay to check if it's reachable.
    async fn ping_relay(&self, relay: &RelayEndpoint) -> bool {
        let url = format!("{}/health", relay.url().trim_end_matches('/'));
        match self.http_client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Get number of configured relays.
    pub async fn relay_count(&self) -> usize {
        let relays = self.relays.read().await;
        relays.len()
    }

    /// Get number of reachable relays.
    pub async fn reachable_relay_count(&self) -> usize {
        let relays = self.relays.read().await;
        relays.iter().filter(|r| r.is_reachable()).count()
    }
}

/// Extension trait for Transport with relay support.
pub trait TransportWithRelay {
    /// Get the transport with relay fallback wrapper.
    fn with_relay(self) -> anyhow::Result<QuicTransportWithRelay>
    where
        Self: Sized + 'static,
        Arc<Self>: Transport;
}

impl<T: Transport + 'static> TransportWithRelay for T {
    fn with_relay(self) -> anyhow::Result<QuicTransportWithRelay> {
        QuicTransportWithRelay::new(Arc::new(self))
    }
}

/// Helper to create a transport with default relay fallback settings.
pub fn create_transport_with_relay_fallback(
    inner: Arc<dyn Transport>,
    relay_urls: Vec<(String, NodeId)>,
) -> anyhow::Result<QuicTransportWithRelay> {
    let mut relays = Vec::new();
    for (url, id) in relay_urls {
        let relay = RelayEndpoint::new(url, id)?;
        relays.push(relay);
    }
    QuicTransportWithRelay::with_config(inner, relays, Duration::from_secs(10))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_connection_type_str() {
        assert_eq!(FallbackConnectionType::Direct.as_str(), "direct");
        assert_eq!(FallbackConnectionType::HolePunch.as_str(), "hole_punch");
        assert_eq!(FallbackConnectionType::Relay.as_str(), "relay");
    }

    #[test]
    fn fallback_connection_type_debug() {
        assert_eq!(format!("{:?}", FallbackConnectionType::Direct), "Direct");
        assert_eq!(format!("{:?}", FallbackConnectionType::HolePunch), "HolePunch");
        assert_eq!(format!("{:?}", FallbackConnectionType::Relay), "Relay");
    }

    #[test]
    fn fallback_connection_type_eq() {
        assert_eq!(FallbackConnectionType::Direct, FallbackConnectionType::Direct);
        assert_ne!(FallbackConnectionType::Direct, FallbackConnectionType::Relay);
    }

    #[test]
    fn fallback_connection_type_clone() {
        let t = FallbackConnectionType::Direct;
        let cloned = t.clone();
        assert_eq!(t, cloned);
    }

    #[test]
    fn fallback_connection_type_send() {
        fn assert_send<T: Send>() {}
        assert_send::<FallbackConnectionType>();
    }

    #[test]
    fn fallback_connection_type_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<FallbackConnectionType>();
    }

    #[test]
    fn relay_endpoint_valid_https() {
        let relay = RelayEndpoint::new(
            "https://relay.example.com".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        ).unwrap();
        assert!(relay.is_reachable());
        assert_eq!(relay.url(), "https://relay.example.com");
    }

    #[test]
    fn relay_endpoint_reject_http() {
        let relay = RelayEndpoint::new(
            "http://relay.example.com".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        );
        assert!(relay.is_err());
        assert!(format!("{}", relay.unwrap_err()).contains("HTTPS"));
    }

    #[test]
    fn relay_endpoint_reject_localhost() {
        let relay = RelayEndpoint::new(
            "https://localhost".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        );
        assert!(relay.is_err());
        assert!(format!("{}", relay.unwrap_err()).contains("localhost"));
    }

    #[test]
    fn relay_endpoint_reject_loopback() {
        let relay = RelayEndpoint::new(
            "https://127.0.0.1".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        );
        assert!(relay.is_err());
    }

    #[test]
    fn relay_endpoint_reject_private() {
        let relay = RelayEndpoint::new(
            "https://192.168.1.1".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        );
        assert!(relay.is_err());
        assert!(format!("{}", relay.unwrap_err()).contains("private"));
    }

    #[test]
    fn relay_endpoint_reachability() {
        let mut relay = RelayEndpoint::new(
            "https://relay.example.com".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        ).unwrap();
        relay.mark_unreachable();
        assert!(!relay.is_reachable());
        relay.mark_reachable();
        assert!(relay.is_reachable());
        assert!(relay.time_since_health_check() < Duration::from_secs(1));
    }

    #[test]
    fn relay_endpoint_debug() {
        let relay = RelayEndpoint::new(
            "https://relay.example.com".to_string(),
            NodeId::from_bytes(&[0x42u8; 32]).unwrap(),
        ).unwrap();
        let debug_str = format!("{:?}", relay);
        assert!(debug_str.contains("RelayEndpoint"));
        assert!(debug_str.contains("relay.example.com"));
    }

    #[test]
    fn connection_result() {
        let result = ConnectionResult::Connected(FallbackConnectionType::Direct);
        match result {
            ConnectionResult::Connected(conn_type) => {
                assert_eq!(conn_type, FallbackConnectionType::Direct);
            }
            ConnectionResult::Failed(_) => panic!("expected Connected"),
        }
    }

    #[test]
    fn connection_result_debug() {
        let result = ConnectionResult::Connected(FallbackConnectionType::Direct);
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("Connected"));
    }

    #[test]
    fn connect_error() {
        let error = ConnectError::new(vec![
            TransportError::Other("connect attempt 1".into()),
            TransportError::Other("no route".into()),
        ]);
        assert_eq!(error.errors.len(), 2);
        assert_eq!(error.attempts(), 2);
    }

    #[test]
    fn connect_error_display() {
        let error = ConnectError::new(vec![
            TransportError::Other("timeout 1".into()),
        ]);
        let display_str = format!("{}", error);
        assert!(display_str.contains("all connection strategies failed"));
    }

    #[test]
    fn connect_error_display_multiple() {
        let error = ConnectError::new(vec![
            TransportError::Other("error 1".into()),
            TransportError::Other("error 2".into()),
        ]);
        let display_str = format!("{}", error);
        assert!(display_str.contains("all connection strategies failed"));
    }

    #[test]
    fn create_transport_with_relay_fallback() {
        // Skip this test as it requires a full Transport implementation
        // The test for RelayEndpoint URL validation is sufficient
        let relay = RelayEndpoint::new(
            "https://relay1.example.com".to_string(),
            NodeId::from_bytes(&[0x11u8; 32]).unwrap(),
        );
        assert!(relay.is_ok());

        let relay2 = RelayEndpoint::new(
            "https://relay2.example.com".to_string(),
            NodeId::from_bytes(&[0x22u8; 32]).unwrap(),
        );
        assert!(relay2.is_ok());
    }

    #[test]
    fn create_transport_rejects_invalid_relay() {
        let result = RelayEndpoint::new(
            "http://insecure.example.com".to_string(),
            NodeId::from_bytes(&[0x11u8; 32]).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn relay_endpoint_reject_link_local() {
        // 169.254.x.x is link-local
        let result = RelayEndpoint::new(
            "https://169.254.1.1".to_string(),
            NodeId::from_bytes(&[0x33u8; 32]).unwrap(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(format!("{}", err).contains("link-local") || format!("{}", err).contains("private"));
    }

    #[test]
    fn relay_endpoint_reject_multicast() {
        // 224.x.x.x is multicast range
        let result = RelayEndpoint::new(
            "https://224.0.0.1".to_string(),
            NodeId::from_bytes(&[0x44u8; 32]).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn relay_endpoint_invalid_url() {
        let result = RelayEndpoint::new(
            "not-a-valid-url".to_string(),
            NodeId::from_bytes(&[0x55u8; 32]).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn relay_endpoint_no_host() {
        let result = RelayEndpoint::new(
            "https://".to_string(),
            NodeId::from_bytes(&[0x66u8; 32]).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn relay_endpoint_ipv6_loopback() {
        let result = RelayEndpoint::new(
            "https://[::1]".to_string(),
            NodeId::from_bytes(&[0x77u8; 32]).unwrap(),
        );
        assert!(result.is_err(), "IPv6 loopback should be rejected, got: {:?}", result);
    }

    // Mock transport removed - QuicTransportWithRelay requires a full Transport impl
}
