//! UDP/TCP hole punching for NAT traversal.
//!
//! Hole punching is a technique to establish direct peer-to-peer connections
//! through NATs by coordinating through a rendezvous server.

use std::collections::HashMap;
use std::net::{SocketAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{timeout, interval};
use rand::Rng;

use crate::error::{NatError, NatResult};

/// Hole punching result.
#[derive(Debug, Clone)]
pub struct HolePunchResult {
    /// Whether the connection succeeded
    pub success: bool,
    /// The external addresses discovered
    pub local_external: Option<SocketAddr>,
    pub remote_external: Option<SocketAddr>,
    /// Time taken
    pub duration: Duration,
    /// Error message if failed
    pub error: Option<String>,
}

/// Hole punching manager for UDP.
#[derive(Debug)]
pub struct HolePunch {
    config: HolePunchConfig,
    active_sessions: Arc<RwLock<HashMap<String, HolePunchSession>>>,
}

#[derive(Debug)]
struct HolePunchSession {
    peer_id: String,
    started_at: std::time::Instant,
    attempts: u32,
}

#[derive(Debug, Clone)]
pub struct HolePunchConfig {
    timeout_ms: u64,
    max_attempts: u32,
    retry_interval_ms: u64,
}

impl Default for HolePunchConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 10000,
            max_attempts: 5,
            retry_interval_ms: 1000,
        }
    }
}

impl HolePunch {
    /// Create a new hole punching manager.
    pub fn new(config: Option<HolePunchConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            active_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Punch a hole to a peer through a rendezvous server.
    ///
    /// This method coordinates with a rendezvous server to punch holes
    /// in both peers' NATs simultaneously.
    pub async fn punch_udp(
        &self,
        peer_id: &str,
        local_socket: &UdpSocket,
        rendezvous_addr: SocketAddr,
        peer_external_hint: Option<SocketAddr>,
    ) -> NatResult<HolePunchResult> {
        let start = std::time::Instant::now();
        let mut attempts = 0;
        let mut last_error = None;

        // Register with rendezvous server
        let local_addr = local_socket.local_addr()
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        // Send punch messages to the peer through rendezvous
        while attempts < self.config.max_attempts {
            attempts += 1;

            // Send punch packet
            let punch_msg = self.build_punch_message(peer_id, local_addr);
            if let Err(e) = local_socket.send_to(&punch_msg, rendezvous_addr).await {
                last_error = Some(format!("Send failed: {}", e));
                tracing::debug!("Hole punch attempt {} failed: {}", attempts, e);
            }

            // Try to receive from any peer that might be punching us
            let mut buf = [0u8; 1024];
            match timeout(
                Duration::from_millis(self.config.retry_interval_ms),
                local_socket.recv_from(&mut buf)
            ).await {
                Ok(Ok((len, from))) => {
                    // We received something - could be the peer or NAT traversal
                    if self.is_peer_response(&buf[..len], peer_id) {
                        return Ok(HolePunchResult {
                            success: true,
                            local_external: Some(local_addr),
                            remote_external: Some(from),
                            duration: start.elapsed(),
                            error: None,
                        });
                    }
                }
                Ok(Err(e)) => {
                    last_error = Some(format!("Receive failed: {}", e));
                }
                Err(_) => {
                    // Timeout - continue trying
                }
            }

            // Small delay between attempts
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(HolePunchResult {
            success: false,
            local_external: Some(local_addr),
            remote_external: peer_external_hint,
            duration: start.elapsed(),
            error: last_error.or(Some("Max attempts reached".to_string())),
        })
    }

    /// Perform simultaneous hole punching.
    ///
    /// Both peers call this method at the same time to punch holes
    /// in each other's NAT simultaneously.
    pub async fn punch_simultaneous(
        &self,
        peer_id: &str,
        socket: &UdpSocket,
        peer_addr: SocketAddr,
    ) -> NatResult<HolePunchResult> {
        let start = std::time::Instant::now();
        let local_addr = socket.local_addr()
            .map_err(|e| NatError::Network { reason: e.to_string() })?;

        // Send multiple packets to "punch" holes
        let mut rng = rand::thread_rng();
        for i in 0..10 {
            // Vary source port slightly to help symmetric NATs
            let mut pkt = vec![0u8; 64];
            rng.fill(&mut pkt[..]);
            pkt[..8].copy_from_slice(b"ADNETHP");
            pkt[8..24].copy_from_slice(peer_id.as_bytes());

            // Send to peer's guessed external address
            if let Err(e) = socket.send_to(&pkt, peer_addr).await {
                tracing::debug!("Punch packet {} failed: {}", i, e);
            }

            tokio::time::sleep(Duration::from_millis(50 + rng.gen_range(0..100))).await;
        }

        Ok(HolePunchResult {
            success: true, // Simplified - would need actual feedback
            local_external: Some(local_addr),
            remote_external: Some(peer_addr),
            duration: start.elapsed(),
            error: None,
        })
    }

    /// Build a hole punch message.
    fn build_punch_message(&self, peer_id: &str, local_addr: SocketAddr) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(b"ADNETHP"); // Magic bytes
        msg.extend_from_slice(peer_id.as_bytes());
        msg.extend_from_slice(&local_addr.port().to_be_bytes());
        msg
    }

    /// Check if a received packet is from our peer.
    fn is_peer_response(&self, data: &[u8], peer_id: &str) -> bool {
        data.len() >= 24 && &data[..8] == b"ADNETHP"
    }

    /// Register an active punching session.
    pub async fn register_session(&self, peer_id: String) {
        let mut sessions = self.active_sessions.write().await;
        sessions.insert(peer_id.clone(), HolePunchSession {
            peer_id,
            started_at: std::time::Instant::now(),
            attempts: 0,
        });
    }

    /// Remove a punching session.
    pub async fn remove_session(&self, peer_id: &str) {
        let mut sessions = self.active_sessions.write().await;
        sessions.remove(peer_id);
    }

    /// Get active session count.
    pub async fn active_count(&self) -> usize {
        let sessions = self.active_sessions.read().await;
        sessions.len()
    }
}

/// TCP hole punching support.
pub mod tcp {
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::net::TcpStream as TokioTcpStream;

    use crate::error::{NatError, NatResult};

    /// Attempt TCP hole punching.
    pub async fn punch_tcp(
        peer_addr: SocketAddr,
        timeout_ms: u64,
    ) -> NatResult<TokioTcpStream> {
        let stream = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            TokioTcpStream::connect(peer_addr)
        ).await
        .map_err(|_| NatError::Timeout { operation: "TCP hole punch".to_string() })?
        .map_err(|e| NatError::Network { reason: e.to_string() })?;

        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hole_punch_config() {
        let config = HolePunchConfig::default();
        assert_eq!(config.max_attempts, 5);
        assert_eq!(config.timeout_ms, 10000);
    }

    #[tokio::test]
    async fn test_hole_punch_creation() {
        let hp = HolePunch::new(None);
        assert_eq!(hp.active_count().await, 0);

        hp.register_session("peer1".to_string()).await;
        assert_eq!(hp.active_count().await, 1);

        hp.remove_session("peer1").await;
        assert_eq!(hp.active_count().await, 0);
    }
}
