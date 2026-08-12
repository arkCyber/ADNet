//! NAT Hole Punching implementation.
//!
//! Enables direct P2P connections between nodes behind NAT by coordinating
//! simultaneous connection attempts. This is used when direct QUIC connections
//! fail due to NAT restrictions.
//!
//! ## How it works
//!
//! 1. Both peers learn each other's external addresses via STUN
//! 2. Both peers send UDP packets to each other's external addresses simultaneously
//! 3. This "punches holes" in both NATs, allowing subsequent TCP/QUIC traffic
//!
//! ## Limitations
//!
//! - Symmetric NATs cannot be hole-punched
//! - Requires both peers to be online simultaneously
//! - Best effort - may not work in all network configurations

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{interval, Instant};
use tracing::{debug, info, warn};

use crate::stun::{NatType, StunClient, StunConfig};

/// Magic header for hole punching packets.
///
/// Security: This is not cryptographic - it's just a protocol discriminator.
/// The actual security comes from the transport layer (QUIC+mTLS).
const MAGIC_HEADER: [u8; 4] = [0x41, 0x44, 0x4E, 0x45]; // "ADNE"

/// Hole punching session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolePunchState {
    /// Initial state - waiting for peers.
    Waiting,
    /// Sending punch packets.
    Punching,
    /// Hole punched successfully.
    Success,
    /// Hole punching failed.
    Failed,
    /// Timeout waiting for peer.
    Timeout,
}

impl Default for HolePunchState {
    fn default() -> Self {
        HolePunchState::Waiting
    }
}

impl HolePunchState {
    /// Check if this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Success | Self::Failed | Self::Timeout)
    }
}

/// Hole punching session for a single peer.
#[derive(Debug)]
pub struct HolePunchSession {
    /// Remote peer address (external).
    peer_addr: SocketAddr,
    /// Our external address (from STUN).
    _our_external: SocketAddr,
    /// Session state.
    state: HolePunchState,
    /// When the session should timeout.
    deadline: Instant,
}

impl HolePunchSession {
    fn new(peer_addr: SocketAddr, our_external: SocketAddr, timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            peer_addr,
            _our_external: our_external,
            state: HolePunchState::Waiting,
            deadline: now + timeout,
        }
    }

    /// Update session state.
    pub fn update_state(&mut self, state: HolePunchState) {
        debug!("Hole punch session {} -> {:?}", self.peer_addr, state);
        self.state = state;
    }

    /// Check if session has expired.
    pub fn is_expired(&self) -> bool {
        Instant::now() > self.deadline
    }

    /// Get current state.
    pub fn state(&self) -> HolePunchState {
        self.state
    }

    /// Get peer address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }
}

/// Hole punching coordinator.
///
/// Manages multiple hole punching sessions and coordinates the punching process.
pub struct HolePunchCoordinator {
    /// Sessions indexed by peer address.
    sessions: Arc<RwLock<HashMap<SocketAddr, HolePunchSession>>>,
    /// Local UDP socket for punching.
    punch_socket: Option<Arc<UdpSocket>>,
    /// Our external address (discovered via STUN).
    our_external: Arc<RwLock<Option<SocketAddr>>>,
    /// Default timeout for hole punching attempts.
    default_timeout: Duration,
}

impl HolePunchCoordinator {
    /// Create a new coordinator.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            punch_socket: None,
            our_external: Arc::new(RwLock::new(None)),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Create a coordinator with a pre-bound UDP socket.
    pub fn with_socket(socket: UdpSocket) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            punch_socket: Some(Arc::new(socket)),
            our_external: Arc::new(RwLock::new(None)),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Initialize the coordinator with STUN discovery.
    ///
    /// This binds a UDP socket and discovers our external address.
    pub async fn initialize(&mut self, stun_server: SocketAddr) -> anyhow::Result<()> {
        // Bind UDP socket if not already set
        if self.punch_socket.is_none() {
            let socket = UdpSocket::bind("0.0.0.0:0").await?;
            self.punch_socket = Some(Arc::new(socket));
        }

        // Discover our external address
        let socket = self.punch_socket.as_ref().unwrap();
        let client = StunClient::new(StunConfig {
            server: stun_server,
            timeout: Duration::from_secs(3),
            retries: 2,
        });

        match client.detect(socket).await {
            Ok(response) => {
                info!(
                    "Our external address: {}, NAT type: {}",
                    response.mapped_address,
                    response.nat_type.description()
                );
                let mut ext = self.our_external.write().await;
                *ext = Some(response.mapped_address);
            }
            Err(e) => {
                warn!("STUN discovery failed: {}. Continuing without external address.", e);
            }
        }

        Ok(())
    }

    /// Get our discovered external address.
    pub async fn our_external_addr(&self) -> Option<SocketAddr> {
        self.our_external.read().await.clone()
    }

    /// Start a hole punching session with a peer.
    ///
    /// Returns a channel to receive updates about the session.
    pub async fn start_session(
        &self,
        peer_external: SocketAddr,
        peer_internal: Option<SocketAddr>,
        state_rx: mpsc::Receiver<HolePunchState>,
    ) -> anyhow::Result<HolePunchHandle> {
        let our_external = self.our_external.read().await;
        let our_addr = our_external.ok_or_else(|| {
            anyhow::anyhow!("external address not discovered. Call initialize() first.")
        })?;

        // Validate peer address
        if peer_external.ip().is_loopback() {
            return Err(anyhow::anyhow!("cannot hole punch to loopback address"));
        }
        if peer_external.ip().is_unspecified() {
            return Err(anyhow::anyhow!("peer address is unspecified"));
        }

        // Log internal address if provided (for debugging)
        if let Some(internal) = peer_internal {
            debug!("Hole punch target: external={}, internal={}", peer_external, internal);
        }

        let session = HolePunchSession::new(peer_external, our_addr, self.default_timeout);

        // Store the session
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(peer_external, session);
        }

        // Start the punching task
        let socket = self.punch_socket.clone();
        let sessions = Arc::clone(&self.sessions);
        let peer_addr = peer_external;
        let timeout = self.default_timeout;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let result = Self::run_punching_loop(
                socket,
                peer_addr,
                our_addr,
                timeout,
                state_rx,
            )
            .await;

            // Update session state
            let mut sessions = sessions.write().await;
            if let Some(session) = sessions.get_mut(&peer_addr) {
                match result {
                    Ok(()) => session.update_state(HolePunchState::Success),
                    Err(_) => session.update_state(HolePunchState::Failed),
                }
            }

            let _ = done_tx.send(result);
        });

        Ok(HolePunchHandle {
            peer_addr,
            done_rx,
        })
    }

    /// The main hole punching loop.
    async fn run_punching_loop(
        socket: Option<Arc<UdpSocket>>,
        peer_external: SocketAddr,
        _our_external: SocketAddr,
        timeout: Duration,
        mut state_rx: mpsc::Receiver<HolePunchState>,
    ) -> anyhow::Result<()> {
        let socket = socket.ok_or_else(|| anyhow::anyhow!("UDP socket not initialized"))?;

        // Create a punch packet with magic header
        let punch_msg = Self::build_punch_packet();
        let mut ticker = interval(Duration::from_millis(200));

        info!("Starting hole punch to {}", peer_external);

        // Send punch packets periodically
        loop {
            tokio::select! {
                // Send punch packet
                _ = ticker.tick() => {
                    if let Err(e) = socket.send_to(&punch_msg, peer_external).await {
                        debug!("Punch packet send failed: {}", e);
                        // Continue trying - NAT may be filtering
                    } else {
                        debug!("Sent punch packet to {}", peer_external);
                    }
                }

                // Listen for incoming punch from peer
                result = Self::receive_punch(&socket) => {
                    match result {
                        Ok(from) if from == peer_external => {
                            info!("Received punch from peer {}", peer_external);
                            return Ok(());
                        }
                        Ok(from) => {
                            debug!("Received punch from unexpected source {}", from);
                        }
                        Err(e) => {
                            debug!("Punch receive error: {}", e);
                        }
                    }
                }

                // Check for state updates
                state = state_rx.recv() => {
                    match state {
                        Some(HolePunchState::Success) => return Ok(()),
                        Some(HolePunchState::Failed) | Some(HolePunchState::Timeout) => {
                            return Err(anyhow::anyhow!("Session failed"));
                        }
                        Some(state) => {
                            debug!("State update: {:?}", state);
                        }
                        None => break,
                    }
                }

                // Timeout
                _ = tokio::time::sleep(timeout) => {
                    warn!("Hole punch timed out for {}", peer_external);
                    return Err(anyhow::anyhow!("Hole punch timeout"));
                }
            }
        }

        Err(anyhow::anyhow!("State channel closed"))
    }

    /// Build a hole punch packet.
    ///
    /// Format: MAGIC_HEADER (4 bytes) + random nonce (8 bytes)
    fn build_punch_packet() -> Vec<u8> {
        use rand::RngCore;
        let mut packet = Vec::with_capacity(12);
        packet.extend_from_slice(&MAGIC_HEADER);
        let mut nonce = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut nonce);
        packet.extend_from_slice(&nonce);
        packet
    }

    /// Receive a punch packet from any source.
    async fn receive_punch(socket: &UdpSocket) -> anyhow::Result<SocketAddr> {
        let mut buf = [0u8; 64];
        // Use tokio's timeout instead of set_read_timeout for non-blocking receive
        match tokio::time::timeout(Duration::from_millis(100), socket.recv_from(&mut buf)).await {
            Ok(Ok((len, from))) if len >= 4 => {
                // Check magic header
                if buf[..4] == MAGIC_HEADER {
                    Ok(from)
                } else {
                    Err(anyhow::anyhow!("Not a punch packet"))
                }
            }
            Ok(Ok(_)) => Err(anyhow::anyhow!("Packet too short")),
            Ok(Err(e)) => Err(anyhow::anyhow!("recv_from: {}", e)),
            Err(_) => Err(anyhow::anyhow!("timeout")),
        }
    }

    /// Get the session state for a peer.
    pub async fn session_state(&self, peer: &SocketAddr) -> Option<HolePunchState> {
        let sessions = self.sessions.read().await;
        sessions.get(peer).map(|s| s.state())
    }

    /// Get session info for a peer.
    pub async fn session_info(&self, peer: &SocketAddr) -> Option<HolePunchSessionInfo> {
        let sessions = self.sessions.read().await;
        sessions.get(peer).map(|s| HolePunchSessionInfo {
            peer_addr: s.peer_addr(),
            state: s.state(),
            is_expired: s.is_expired(),
        })
    }

    /// Clean up expired sessions.
    pub async fn cleanup_expired(&self) {
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, session| !session.is_expired());
    }

    /// Get number of active sessions.
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }
}

impl Default for HolePunchCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Information about a hole punching session.
#[derive(Debug, Clone)]
pub struct HolePunchSessionInfo {
    /// Peer address.
    pub peer_addr: SocketAddr,
    /// Current state.
    pub state: HolePunchState,
    /// Whether the session has expired.
    pub is_expired: bool,
}

/// Handle to an active hole punching session.
pub struct HolePunchHandle {
    peer_addr: SocketAddr,
    done_rx: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
}

impl HolePunchHandle {
    /// Wait for the session to complete.
    pub async fn wait(self) -> anyhow::Result<()> {
        self.done_rx.await??;
        Ok(())
    }

    /// Get the peer's address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Try to get the result without waiting.
    pub fn try_wait(&mut self) -> Option<anyhow::Result<()>> {
        match self.done_rx.try_recv() {
            Ok(result) => Some(result),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                Some(Err(anyhow::anyhow!("Channel closed")))
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
        }
    }
}

/// Connection strategy based on NAT type.
#[derive(Debug, Clone)]
pub struct ConnectionStrategy {
    /// Whether to attempt direct connection first.
    pub try_direct_first: bool,
    /// Whether to attempt hole punching.
    pub try_hole_punch: bool,
    /// Whether to fall back to relay.
    pub try_relay: bool,
    /// Timeout for each connection attempt.
    pub attempt_timeout: Duration,
}

impl Default for ConnectionStrategy {
    fn default() -> Self {
        Self {
            try_direct_first: true,
            try_hole_punch: true,
            try_relay: true,
            attempt_timeout: Duration::from_secs(10),
        }
    }
}

impl ConnectionStrategy {
    /// Create a strategy optimized for the given NAT type.
    pub fn for_nat_type(nat_type: NatType) -> Self {
        match nat_type {
            NatType::OpenInternet | NatType::FullCone => Self {
                try_direct_first: true,
                try_hole_punch: false,
                try_relay: false,
                attempt_timeout: Duration::from_secs(5),
            },
            NatType::AddressRestricted | NatType::PortRestricted => Self {
                try_direct_first: true,
                try_hole_punch: true,
                try_relay: true,
                attempt_timeout: Duration::from_secs(15),
            },
            NatType::SymmetricNat => Self {
                try_direct_first: false,
                try_hole_punch: false,
                try_relay: true,
                attempt_timeout: Duration::from_secs(10),
            },
            NatType::Unknown => Self {
                try_direct_first: true,
                try_hole_punch: true,
                try_relay: true,
                attempt_timeout: Duration::from_secs(20),
            },
        }
    }

    /// Returns true if we should try relay.
    pub fn should_try_relay(&self) -> bool {
        self.try_relay
    }

    /// Returns true if we should try hole punching.
    pub fn should_try_hole_punch(&self) -> bool {
        self.try_hole_punch
    }

    /// Returns true if we should try direct connection.
    pub fn should_try_direct(&self) -> bool {
        self.try_direct_first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_header_length() {
        assert_eq!(MAGIC_HEADER.len(), 4);
        assert_eq!(MAGIC_HEADER, [0x41, 0x44, 0x4E, 0x45]);
    }

    #[test]
    fn connection_strategy_for_nat_types() {
        let open = ConnectionStrategy::for_nat_type(NatType::OpenInternet);
        assert!(open.try_direct_first);
        assert!(!open.try_hole_punch);
        assert!(!open.try_relay);

        let symmetric = ConnectionStrategy::for_nat_type(NatType::SymmetricNat);
        assert!(!symmetric.try_direct_first);
        assert!(!symmetric.try_hole_punch);
        assert!(symmetric.try_relay);

        let restricted = ConnectionStrategy::for_nat_type(NatType::PortRestricted);
        assert!(restricted.try_direct_first);
        assert!(restricted.try_hole_punch);
        assert!(restricted.try_relay);
    }

    #[tokio::test]
    async fn coordinator_initialization() {
        let mut coordinator = HolePunchCoordinator::new();
        // Without a valid STUN server, this will fail but that's ok for the test
        let _ = coordinator
            .initialize(SocketAddr::new("127.0.0.1".parse().unwrap(), 3478))
            .await;
    }

    #[test]
    fn hole_punch_state_terminal() {
        assert!(HolePunchState::Success.is_terminal());
        assert!(HolePunchState::Failed.is_terminal());
        assert!(HolePunchState::Timeout.is_terminal());
        assert!(!HolePunchState::Waiting.is_terminal());
        assert!(!HolePunchState::Punching.is_terminal());
    }

    #[test]
    fn connection_strategy_default() {
        let strategy = ConnectionStrategy::default();
        assert!(strategy.try_direct_first);
        assert!(strategy.try_hole_punch);
        assert!(strategy.try_relay);
        assert_eq!(strategy.attempt_timeout, Duration::from_secs(10));
    }

    #[test]
    fn hole_punch_state_debug() {
        assert_eq!(format!("{:?}", HolePunchState::Waiting), "Waiting");
        assert_eq!(format!("{:?}", HolePunchState::Punching), "Punching");
        assert_eq!(format!("{:?}", HolePunchState::Success), "Success");
        assert_eq!(format!("{:?}", HolePunchState::Failed), "Failed");
        assert_eq!(format!("{:?}", HolePunchState::Timeout), "Timeout");
    }

    #[test]
    fn hole_punch_state_eq() {
        assert_eq!(HolePunchState::Waiting, HolePunchState::Waiting);
        assert_ne!(HolePunchState::Waiting, HolePunchState::Punching);
        assert_eq!(HolePunchState::Success, HolePunchState::Success);
    }

    #[test]
    fn hole_punch_state_clone() {
        let state = HolePunchState::Waiting;
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn hole_punch_state_default() {
        let state = HolePunchState::default();
        assert_eq!(state, HolePunchState::Waiting);
    }

    #[test]
    fn hole_punch_state_send() {
        fn assert_send<T: Send>(_: T) {}
        assert_send(HolePunchState::Waiting);
        assert_send(HolePunchState::Success);
    }

    #[test]
    fn hole_punch_state_sync() {
        fn assert_sync<T: Sync>(_: T) {}
        assert_sync(HolePunchState::Waiting);
        assert_sync(HolePunchState::Success);
    }

    #[test]
    fn build_punch_packet_length() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let packet = runtime.block_on(async {
            // Just verify the magic header is correct
            HolePunchCoordinator::build_punch_packet()
        });
        assert_eq!(packet.len(), 12); // 4 byte magic + 8 byte nonce
        assert_eq!(&packet[..4], &MAGIC_HEADER);
    }

    #[test]
    fn build_punch_packet_uniqueness() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut packets = std::collections::HashSet::new();
        for _ in 0..100 {
            let packet = runtime.block_on(async {
                HolePunchCoordinator::build_punch_packet()
            });
            // Nonce should be different each time
            assert!(packets.insert(packet.clone()), "Packets should be unique");
        }
    }

    #[test]
    fn session_info_debug() {
        let info = HolePunchSessionInfo {
            peer_addr: "192.168.1.1:9000".parse().unwrap(),
            state: HolePunchState::Punching,
            is_expired: false,
        };
        let debug_str = format!("{:?}", info);
        assert!(debug_str.contains("192.168.1.1"));
        assert!(debug_str.contains("Punching"));
    }

    #[test]
    fn connection_strategy_debug() {
        let strategy = ConnectionStrategy::for_nat_type(NatType::FullCone);
        let debug_str = format!("{:?}", strategy);
        assert!(debug_str.contains("ConnectionStrategy"));
        assert!(debug_str.contains("try_direct_first"));
    }
}
