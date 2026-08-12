// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Network conditions for simulation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Network condition parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkCondition {
    /// Latency configuration.
    pub latency: Option<Latency>,
    /// Packet loss configuration.
    pub packet_loss: Option<PacketLoss>,
    /// Bandwidth throttling.
    pub bandwidth: Option<Bandwidth>,
    /// Packet corruption rate.
    pub corruption_rate: f64,
    /// Network partition configuration.
    pub partition: Option<Partition>,
    /// Reordering probability.
    pub reordering_rate: f64,
}

impl Default for NetworkCondition {
    fn default() -> Self {
        Self {
            latency: None,
            packet_loss: None,
            bandwidth: None,
            corruption_rate: 0.0,
            partition: None,
            reordering_rate: 0.0,
        }
    }
}

/// Latency configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Latency {
    /// Base latency in milliseconds.
    pub base_ms: u64,
    /// Jitter (standard deviation) in milliseconds.
    pub jitter_ms: u64,
    /// Minimum latency in milliseconds.
    pub min_ms: u64,
    /// Maximum latency in milliseconds.
    pub max_ms: u64,
}

impl Latency {
    /// Create a new latency configuration.
    pub fn new(base_ms: u64) -> Self {
        Self {
            base_ms,
            jitter_ms: base_ms / 10, // 10% jitter by default
            min_ms: 0,
            max_ms: base_ms * 3,
        }
    }

    /// Add jitter to the base latency.
    pub fn with_jitter(mut self, jitter_ms: u64) -> Self {
        self.jitter_ms = jitter_ms;
        self
    }

    /// Get the actual latency with random jitter.
    pub fn actual_latency(&self) -> Duration {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        // Use wrapping subtraction to handle u64 -> i64 conversion
        let jitter_range = self.jitter_ms as i64;
        let jitter: i64 = rng.gen_range(-jitter_range..=jitter_range);
        let latency_ms = (self.base_ms as i64).saturating_add(jitter);
        let latency_ms = latency_ms.clamp(self.min_ms as i64, self.max_ms as i64);

        Duration::from_millis(latency_ms as u64)
    }
}

/// Packet loss configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketLoss {
    /// Loss rate as a probability (0.0 to 1.0).
    pub rate: f64,
    /// Burst loss configuration.
    pub burst: Option<BurstLoss>,
}

impl PacketLoss {
    /// Create a new packet loss configuration.
    pub fn new(rate: f64) -> Self {
        Self {
            rate: rate.clamp(0.0, 1.0),
            burst: None,
        }
    }

    /// Add burst loss characteristics.
    pub fn with_burst(mut self, avg_burst_length: u32) -> Self {
        self.burst = Some(BurstLoss {
            avg_burst_length,
            current_burst: 0,
            in_burst: false,
        });
        self
    }

    /// Determine if a packet should be dropped.
    pub fn should_drop(&mut self) -> bool {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        if let Some(ref mut burst) = self.burst {
            if burst.in_burst {
                burst.current_burst -= 1;
                if burst.current_burst == 0 {
                    burst.in_burst = false;
                }
                return true;
            }

            // Check if we should start a new burst
            if rng.gen_bool(self.rate * 0.3) {
                burst.in_burst = true;
                burst.current_burst = rng.gen_range(1..burst.avg_burst_length * 2);
                return true;
            }
        }

        rng.gen_bool(self.rate)
    }
}

/// Burst loss configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurstLoss {
    /// Average burst length in packets.
    pub avg_burst_length: u32,
    /// Current position in burst.
    pub current_burst: u32,
    /// Whether we're currently in a burst.
    pub in_burst: bool,
}

/// Bandwidth throttling configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bandwidth {
    /// Upload rate in bytes per second.
    pub upload_bps: u64,
    /// Download rate in bytes per second.
    pub download_bps: u64,
    /// Burst allowance in bytes.
    pub burst_bytes: u64,
}

impl Bandwidth {
    /// Create a new bandwidth configuration.
    pub fn new(upload_mbps: f64, download_mbps: f64) -> Self {
        Self {
            upload_bps: (upload_mbps * 1_000_000.0) as u64,
            download_bps: (download_mbps * 1_000_000.0) as u64,
            burst_bytes: 64 * 1024, // 64 KB burst by default
        }
    }

    /// Check if upload is allowed given token bucket.
    pub fn can_upload(&self, tokens: u64, _packet_size: u64) -> bool {
        tokens >= _packet_size.min(1024) // At least 1KB
    }

    /// Check if download is allowed.
    pub fn can_download(&self, tokens: u64, _packet_size: u64) -> bool {
        tokens >= _packet_size.min(1024)
    }
}

/// Network partition configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Partition {
    /// Probability of partition starting.
    pub start_probability: f64,
    /// Expected partition duration.
    pub duration_secs: u64,
    /// Whether partition is currently active.
    pub active: bool,
    /// Time when partition started (seconds since epoch).
    pub started_at_secs: Option<u64>,
}

impl Partition {
    /// Create a new partition configuration.
    pub fn new(duration: Duration) -> Self {
        Self {
            start_probability: 0.01, // 1% chance per check
            duration_secs: duration.as_secs(),
            active: false,
            started_at_secs: None,
        }
    }

    /// Update partition state.
    pub fn update(&mut self) {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        if self.active {
            if let Some(started) = self.started_at_secs {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if now.saturating_sub(started) >= self.duration_secs {
                    self.active = false;
                    self.started_at_secs = None;
                }
            }
        } else if rng.gen_bool(self.start_probability) {
            self.active = true;
            self.started_at_secs = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            );
        }
    }

    /// Check if we're currently partitioned.
    pub fn is_partitioned(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latency_calculation() {
        let latency = Latency::new(100).with_jitter(20);
        for _ in 0..100 {
            let actual = latency.actual_latency();
            assert!(actual.as_millis() >= 80);
            assert!(actual.as_millis() <= 300); // base * 3
        }
    }

    #[test]
    fn test_packet_loss() {
        let mut loss = PacketLoss::new(0.5);
        let drops: usize = (0..1000).filter(|_| loss.should_drop()).count();
        // Should be roughly 50%
        assert!(drops > 400 && drops < 600);
    }
}
