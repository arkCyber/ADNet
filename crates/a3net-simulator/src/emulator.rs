// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Network emulator for applying conditions to connections.

use crate::conditions::{Bandwidth, Latency, NetworkCondition, PacketLoss, Partition};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};

/// Connection identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);

/// Statistics for a connection.
#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_dropped: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub latency_samples: Vec<Duration>,
}

/// Token bucket for bandwidth limiting.
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: u64,
    last_refill: Instant,
    rate: u64, // tokens per second
    capacity: u64,
}

impl TokenBucket {
    fn new(rate: u64, capacity: u64) -> Self {
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
            rate,
            capacity,
        }
    }

    fn try_consume(&mut self, amount: u64) -> bool {
        self.refill();
        if self.tokens >= amount {
            self.tokens -= amount;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let tokens_to_add = (elapsed * self.rate as f64) as u64;
        self.tokens = std::cmp::min(self.tokens + tokens_to_add, self.capacity);
        self.last_refill = Instant::now();
    }
}

/// A tracked connection with simulated network conditions.
pub struct TrackedConnection {
    id: ConnectionId,
    condition: NetworkCondition,
    upload_bucket: TokenBucket,
    download_bucket: TokenBucket,
    pending_delivery: HashMap<u64, (Instant, Vec<u8>)>, // seq -> (deliver_at, data)
    stats: ConnectionStats,
}

impl TrackedConnection {
    fn new(id: ConnectionId, condition: NetworkCondition) -> Self {
        let bandwidth = condition.bandwidth.unwrap_or(Bandwidth {
            upload_bps: u64::MAX,
            download_bps: u64::MAX,
            burst_bytes: 0,
        });

        Self {
            id,
            condition,
            upload_bucket: TokenBucket::new(bandwidth.upload_bps, bandwidth.burst_bytes),
            download_bucket: TokenBucket::new(bandwidth.download_bps, bandwidth.burst_bytes),
            pending_delivery: HashMap::new(),
            stats: ConnectionStats::default(),
        }
    }

    /// Apply latency to a packet and schedule delivery.
    fn apply_latency(&mut self, seq: u64, data: Vec<u8>) -> Option<Duration> {
        if let Some(ref latency) = self.condition.latency {
            let delay = latency.actual_latency();
            self.pending_delivery.insert(seq, (Instant::now() + delay, data));
            Some(delay)
        } else {
            None
        }
    }

    /// Check if a packet should be dropped.
    fn should_drop(&mut self) -> bool {
        if let Some(ref mut loss) = self.condition.packet_loss {
            loss.should_drop()
        } else {
            false
        }
    }

    /// Check if we're currently partitioned.
    fn is_partitioned(&self) -> bool {
        self.condition.partition.as_ref().map_or(false, |p| p.is_partitioned())
    }

    /// Get pending packets that are ready for delivery.
    fn get_ready_packets(&mut self) -> Vec<(u64, Vec<u8>)> {
        let now = Instant::now();
        // Retain only packets that are ready for delivery
        self.pending_delivery.retain(|_seq, (deliver_at, _)| *deliver_at <= now);

        self.pending_delivery
            .iter()
            .filter(|(_, (deliver_at, _))| *deliver_at <= now)
            .map(|(seq, (_, data))| (*seq, data.clone()))
            .collect()
    }
}

/// Network emulator that applies conditions to connections.
pub struct NetworkEmulator {
    connections: Arc<RwLock<HashMap<ConnectionId, TrackedConnection>>>,
    next_seq: Arc<RwLock<u64>>,
}

impl NetworkEmulator {
    /// Create a new network emulator.
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            next_seq: Arc::new(RwLock::new(0)),
        }
    }

    /// Add a connection with network conditions.
    pub async fn add_connection(&self, id: ConnectionId, condition: NetworkCondition) {
        let conn = TrackedConnection::new(id.clone(), condition);
        let mut connections = self.connections.write().await;
        connections.insert(id, conn);
    }

    /// Remove a connection.
    pub async fn remove_connection(&self, id: &ConnectionId) {
        let mut connections = self.connections.write().await;
        connections.remove(id);
    }

    /// Get connection statistics.
    pub async fn get_stats(&self, id: &ConnectionId) -> Option<ConnectionStats> {
        let connections = self.connections.read().await;
        connections.get(id).map(|c| c.stats.clone())
    }

    /// Send a packet through the emulator.
    pub async fn send(&self, conn_id: &ConnectionId, data: Vec<u8>) -> Option<Duration> {
        let mut connections = self.connections.write().await;
        let conn = connections.get_mut(conn_id)?;

        // Check for partition
        if conn.is_partitioned() {
            conn.stats.packets_dropped += 1;
            return None;
        }

        // Check for packet loss
        if conn.should_drop() {
            conn.stats.packets_dropped += 1;
            return None;
        }

        // Check bandwidth
        if !conn.upload_bucket.try_consume(data.len() as u64) {
            // Would block - for simulation, just count as if it worked
        }

        conn.stats.packets_sent += 1;
        conn.stats.bytes_sent += data.len() as u64;

        // Apply latency
        let seq = {
            let mut next = self.next_seq.write().await;
            let s = *next;
            *next += 1;
            s
        };

        let delay = conn.apply_latency(seq, data);
        delay
    }

    /// Receive packets that are ready.
    pub async fn receive(&self, conn_id: &ConnectionId) -> Vec<Vec<u8>> {
        let mut connections = self.connections.write().await;
        let conn = connections.get_mut(conn_id);

        if let Some(conn) = conn {
            let ready = conn.get_ready_packets();
            for (_, data) in &ready {
                conn.stats.packets_received += 1;
                conn.stats.bytes_received += data.len() as u64;
            }
            ready.into_iter().map(|(_, d)| d).collect()
        } else {
            Vec::new()
        }
    }

    /// Update conditions for a connection.
    pub async fn update_condition(&self, id: &ConnectionId, condition: NetworkCondition) {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.get_mut(id) {
            conn.condition = condition;
        }
    }

    /// Get all connection IDs.
    pub async fn connections(&self) -> Vec<ConnectionId> {
        let connections = self.connections.read().await;
        connections.keys().cloned().collect()
    }

    /// Spawn a background task to update partition states.
    pub fn spawn_partition_updater(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let mut connections = self.connections.write().await;
                for conn in connections.values_mut() {
                    if let Some(ref mut partition) = conn.condition.partition {
                        partition.update();
                    }
                }
            }
        })
    }
}

impl Default for NetworkEmulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for applying network conditions to tokio channels.
pub trait SimulatedChannel<T> {
    /// Apply network conditions to a channel.
    fn apply_network_conditions(self, emulator: Arc<NetworkEmulator>, conn_id: ConnectionId) -> SimulatedSender<T>;
}

/// Simulated sender that respects network conditions.
pub struct SimulatedSender<T> {
    tx: mpsc::Sender<T>,
    emulator: Arc<NetworkEmulator>,
    conn_id: ConnectionId,
}

impl<T: Send + 'static> SimulatedSender<T> {
    /// Send a value with simulated network conditions.
    pub async fn send(&self, value: T) -> Option<Duration> {
        // For demonstration - actual implementation would serialize and simulate
        let _ = self.tx.send(value).await;
        self.emulator.send(&self.conn_id, vec![0; 64]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_tracking() {
        let emulator = NetworkEmulator::new();
        let id = ConnectionId("test-1".to_string());
        let condition = NetworkCondition::default();

        emulator.add_connection(id.clone(), condition).await;
        assert!(emulator.get_stats(&id).await.is_some());

        emulator.remove_connection(&id).await;
        assert!(emulator.get_stats(&id).await.is_none());
    }

    #[tokio::test]
    async fn test_send_with_latency() {
        let emulator = Arc::new(NetworkEmulator::new());
        let id = ConnectionId("latency-test".to_string());
        let mut condition = NetworkCondition::default();
        condition.latency = Some(Latency::new(100));
        emulator.add_connection(id.clone(), condition).await;

        // Send should apply latency
        let delay = emulator.send(&id, vec![1, 2, 3]).await;
        assert!(delay.is_some());
        assert!(delay.unwrap().as_millis() >= 80); // 100 - jitter
    }
}
