//! Bandwidth metering for exit-node traffic.
//!
//! This module tracks bandwidth usage for gateway-side traffic
//! (when we are offering exit services) and client-side traffic
//! (when we are using a gateway).
//!
//! ## Usage Tracking
//!
//! - **Gateway side**: Track traffic forwarded for each client peer.
//! - **Client side**: Track traffic sent through the active gateway.
//!
//! ## Rate Limiting
//!
//! Token bucket algorithm for smooth rate limiting with configurable
//! burst capacity.

use std::sync::Arc;
use std::time::{Duration, Instant};

use a3net_types::NodeId;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Bandwidth usage statistics for a single client or globally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BandwidthStats {
    /// Total bytes uploaded (exit traffic sent to Internet).
    pub bytes_sent: u64,
    /// Total bytes downloaded (traffic received from Internet).
    pub bytes_received: u64,
    /// Number of packets processed.
    pub packets_sent: u64,
    pub packets_received: u64,
    /// Timestamp when counting started.
    pub since: DateTime<Utc>,
}

impl BandwidthStats {
    /// Add traffic counts to this stats object.
    pub fn add_traffic(&mut self, sent_bytes: u64, received_bytes: u64, packets: u64) {
        self.bytes_sent += sent_bytes;
        self.bytes_received += received_bytes;
        self.packets_sent += packets;
        self.packets_received += packets;
    }

    /// Total bytes transferred.
    pub fn total_bytes(&self) -> u64 {
        self.bytes_sent + self.bytes_received
    }

    /// Create a copy with only bytes (for privacy-sensitive contexts).
    pub fn bytes_only(&self) -> Self {
        Self {
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            packets_sent: 0,
            packets_received: 0,
            since: self.since,
        }
    }
}

/// Per-client bandwidth meter.
#[derive(Debug, Clone)]
pub struct ClientMeter {
    inner: Arc<ClientMeterInner>,
}

#[derive(Debug)]
struct ClientMeterInner {
    client_id: NodeId,
    stats: RwLock<BandwidthStats>,
    rate_limit: RwLock<Option<RateLimitConfig>>,
    token_bucket: RwLock<TokenBucket>,
}

impl ClientMeter {
    /// Create a new meter for a client.
    pub fn new(client_id: NodeId) -> Self {
        Self {
            inner: Arc::new(ClientMeterInner {
                client_id,
                stats: RwLock::new(BandwidthStats {
                    since: Utc::now(),
                    ..Default::default()
                }),
                rate_limit: RwLock::new(None),
                token_bucket: RwLock::new(TokenBucket::new(0, 0)),
            }),
        }
    }

    /// Get the client ID.
    pub fn client_id(&self) -> &NodeId {
        &self.inner.client_id
    }

    /// Record traffic for this client.
    pub fn record_traffic(&self, sent_bytes: u64, received_bytes: u64, packets: u64) {
        self.inner.stats.write().add_traffic(sent_bytes, received_bytes, packets);
    }

    /// Get current statistics.
    pub fn stats(&self) -> BandwidthStats {
        self.inner.stats.read().clone()
    }

    /// Check if client is within rate limits. Returns Ok if allowed.
    pub fn check_rate_limit(&self, bytes: u64) -> RateLimitResult {
        let limit = self.inner.rate_limit.read();
        match &*limit {
            Some(cfg) => {
                let mut bucket = self.inner.token_bucket.write();
                bucket.consume(bytes, cfg)
            }
            None => RateLimitResult::Allowed,
        }
    }

    /// Set rate limit for this client.
    pub fn set_rate_limit(&self, config: Option<RateLimitConfig>) {
        *self.inner.rate_limit.write() = config.clone();
        if let Some(cfg) = config {
            *self.inner.token_bucket.write() = TokenBucket::new(cfg.bytes_per_second, cfg.burst_bytes);
        }
    }

    /// Reset statistics.
    pub fn reset(&self) {
        let mut stats = self.inner.stats.write();
        *stats = BandwidthStats {
            since: Utc::now(),
            ..Default::default()
        };
    }
}

/// Rate limit configuration for a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum bytes per second.
    pub bytes_per_second: u64,
    /// Burst capacity in bytes.
    pub burst_bytes: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            bytes_per_second: 10 * 1024 * 1024, // 10 MB/s default
            burst_bytes: 5 * 1024 * 1024,      // 5 MB burst
        }
    }
}

/// Result of a rate limit check.
#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitResult {
    /// Request is allowed.
    Allowed,
    /// Request would exceed rate limit.
    Exceeded {
        /// How long to wait before retrying (in seconds).
        wait_seconds: f64,
    },
}

/// Token bucket for rate limiting.
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(bytes_per_second: u64, burst_bytes: u64) -> Self {
        Self {
            tokens: burst_bytes as f64,
            capacity: burst_bytes as f64,
            refill_rate: bytes_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = Instant::now();
    }

    fn consume(&mut self, bytes: u64, config: &RateLimitConfig) -> RateLimitResult {
        self.refill();
        let needed = bytes as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            RateLimitResult::Allowed
        } else {
            let wait = (needed - self.tokens) / self.refill_rate;
            RateLimitResult::Exceeded { wait_seconds: wait }
        }
    }
}

/// Global bandwidth meter for the exit node.
#[derive(Debug, Clone)]
pub struct ExitNodeMeter {
    inner: Arc<ExitNodeMeterInner>,
}

#[derive(Debug)]
struct ExitNodeMeterInner {
    global_stats: RwLock<BandwidthStats>,
    client_meters: RwLock<std::collections::HashMap<NodeId, ClientMeter>>,
    global_limit: RwLock<Option<GlobalBandwidthLimit>>,
}

impl ExitNodeMeter {
    /// Create a new exit node meter.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ExitNodeMeterInner {
                global_stats: RwLock::new(BandwidthStats {
                    since: Utc::now(),
                    ..Default::default()
                }),
                client_meters: RwLock::new(std::collections::HashMap::new()),
                global_limit: RwLock::new(None),
            }),
        }
    }

    /// Record traffic for a specific client.
    pub fn record_client_traffic(
        &self,
        client_id: &NodeId,
        sent_bytes: u64,
        received_bytes: u64,
        packets: u64,
    ) {
        self.inner.global_stats.write().add_traffic(sent_bytes, received_bytes, packets);

        let mut meters = self.inner.client_meters.write();
        let meter = meters.entry(client_id.clone()).or_insert_with(|| ClientMeter::new(client_id.clone()));
        meter.record_traffic(sent_bytes, received_bytes, packets);
    }

    /// Record traffic without client tracking (for aggregated reporting).
    pub fn record_traffic(&self, sent_bytes: u64, received_bytes: u64, packets: u64) {
        self.inner.global_stats.write().add_traffic(sent_bytes, received_bytes, packets);
    }

    /// Get global statistics.
    pub fn global_stats(&self) -> BandwidthStats {
        self.inner.global_stats.read().clone()
    }

    /// Get statistics for a specific client.
    pub fn client_stats(&self, client_id: &NodeId) -> Option<BandwidthStats> {
        self.inner.client_meters.read().get(client_id).map(|m| m.stats())
    }

    /// List all tracked clients.
    pub fn tracked_clients(&self) -> Vec<NodeId> {
        self.inner.client_meters.read().keys().cloned().collect()
    }

    /// Get statistics for all clients.
    pub fn all_client_stats(&self) -> Vec<(NodeId, BandwidthStats)> {
        self.inner.client_meters.read()
            .iter()
            .map(|(id, m)| (id.clone(), m.stats()))
            .collect()
    }

    /// Set global bandwidth limit.
    pub fn set_global_limit(&self, limit: Option<GlobalBandwidthLimit>) {
        *self.inner.global_limit.write() = limit;
    }

    /// Get global bandwidth limit.
    pub fn global_limit(&self) -> Option<GlobalBandwidthLimit> {
        self.inner.global_limit.read().clone()
    }

    /// Check global bandwidth limit. Returns Ok if allowed.
    pub fn check_global_limit(&self, bytes: u64) -> RateLimitResult {
        let limit = self.inner.global_limit.read();
        match &*limit {
            Some(cfg) => {
                let stats = self.inner.global_stats.read();
                if cfg.direction == TrafficDirection::Upload || cfg.direction == TrafficDirection::Both {
                    let remaining = cfg.max_bytes.saturating_sub(stats.bytes_sent + bytes);
                    if remaining < bytes {
                        let wait = bytes as f64 / cfg.bytes_per_second as f64;
                        return RateLimitResult::Exceeded { wait_seconds: wait };
                    }
                }
                if cfg.direction == TrafficDirection::Download || cfg.direction == TrafficDirection::Both {
                    let remaining = cfg.max_bytes.saturating_sub(stats.bytes_received + bytes);
                    if remaining < bytes {
                        let wait = bytes as f64 / cfg.bytes_per_second as f64;
                        return RateLimitResult::Exceeded { wait_seconds: wait };
                    }
                }
                RateLimitResult::Allowed
            }
            None => RateLimitResult::Allowed,
        }
    }

    /// Reset all statistics.
    pub fn reset(&self) {
        *self.inner.global_stats.write() = BandwidthStats {
            since: Utc::now(),
            ..Default::default()
        };
        self.inner.client_meters.write().clear();
    }

    /// Get meter for a specific client, creating one if needed.
    pub fn get_or_create_meter(&self, client_id: NodeId) -> ClientMeter {
        self.inner.client_meters.write()
            .entry(client_id.clone())
            .or_insert_with(|| ClientMeter::new(client_id))
            .clone()
    }
}

impl Default for ExitNodeMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Traffic direction for rate limiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TrafficDirection {
    #[default]
    Both,
    Upload,
    Download,
}

/// Global bandwidth limit configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalBandwidthLimit {
    /// Maximum total bytes allowed.
    pub max_bytes: u64,
    /// Direction this limit applies to.
    pub direction: TrafficDirection,
    /// Rate at which the limit refills (bytes per second).
    pub bytes_per_second: u64,
}

/// Snapshot of bandwidth usage for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandwidthSnapshot {
    pub global: BandwidthStats,
    pub clients: Vec<(String, BandwidthStats)>,
    pub global_limit: Option<GlobalBandwidthLimit>,
    pub tracked_client_count: usize,
}

impl ExitNodeMeter {
    /// Take a full snapshot for reporting.
    pub fn snapshot(&self) -> BandwidthSnapshot {
        let global = self.global_stats();
        let clients = self.all_client_stats()
            .into_iter()
            .map(|(id, stats)| (id.short().to_string(), stats))
            .collect();
        BandwidthSnapshot {
            global,
            clients,
            global_limit: self.global_limit(),
            tracked_client_count: self.tracked_clients().len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_stats_add_traffic() {
        let mut stats = BandwidthStats::default();
        stats.add_traffic(100, 200, 5);
        assert_eq!(stats.bytes_sent, 100);
        assert_eq!(stats.bytes_received, 200);
        assert_eq!(stats.total_bytes(), 300);
    }

    #[test]
    fn client_meter_records_traffic() {
        let client = NodeId::random();
        let meter = ClientMeter::new(client.clone());

        meter.record_traffic(1024, 2048, 10);
        let stats = meter.stats();
        assert_eq!(stats.bytes_sent, 1024);
        assert_eq!(stats.bytes_received, 2048);
    }

    #[test]
    fn client_meter_rate_limit_allowed() {
        let client = NodeId::random();
        let meter = ClientMeter::new(client.clone());
        meter.set_rate_limit(Some(RateLimitConfig {
            bytes_per_second: 1024 * 1024,
            burst_bytes: 1024,
        }));

        // First request should be allowed
        assert_eq!(meter.check_rate_limit(512), RateLimitResult::Allowed);
    }

    #[test]
    fn exit_node_meter_records_traffic() {
        let meter = ExitNodeMeter::new();
        let client = NodeId::random();

        meter.record_client_traffic(&client, 1000, 2000, 5);
        assert_eq!(meter.global_stats().bytes_sent, 1000);
        assert_eq!(meter.global_stats().bytes_received, 2000);
        assert_eq!(meter.client_stats(&client).unwrap().bytes_sent, 1000);
    }

    #[test]
    fn exit_node_meter_tracked_clients() {
        let meter = ExitNodeMeter::new();
        let client1 = NodeId::random();
        let client2 = NodeId::random();

        meter.record_client_traffic(&client1, 100, 0, 1);
        meter.record_client_traffic(&client2, 200, 0, 1);

        let clients = meter.tracked_clients();
        assert_eq!(clients.len(), 2);
        assert!(clients.contains(&client1));
        assert!(clients.contains(&client2));
    }

    #[test]
    fn token_bucket_refill() {
        let mut bucket = TokenBucket::new(1000, 500);
        assert_eq!(bucket.tokens, 500.0);

        // Consume some tokens
        bucket.consume(200, &RateLimitConfig::default());
        // After consumption, tokens should be less than or equal to initial - consumed
        assert!(bucket.tokens <= 300.0);
    }

    #[test]
    fn bandwidth_snapshot_includes_all_fields() {
        let meter = ExitNodeMeter::new();
        let client = NodeId::random();
        meter.record_client_traffic(&client, 1000, 2000, 10);

        let snap = meter.snapshot();
        assert_eq!(snap.global.bytes_sent, 1000);
        assert_eq!(snap.tracked_client_count, 1);
    }
}
