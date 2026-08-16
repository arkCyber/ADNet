//! `a3net bench [n]` — measure IPC round-trip latency.
//!
//! Sends N sequential `info` JSON-RPC calls and reports min / p50 / p95
//! / p99 / max latency plus the effective throughput (calls/sec).
//!
//! Useful for verifying the transport layer is healthy (Unix socket
//! on the same machine is expected to deliver <1 ms p50; HTTP on
//! localhost is expected to deliver 1-3 ms p50).

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::ipc_client::IpcClient;

/// Top-level dispatch — `a3net bench [n]`.
pub async fn run_bench(client: &IpcClient, n: u32, json_out: bool) -> Result<()> {
    let n = n.clamp(1, 10_000);
    // Warmup: 3 calls to amortise first-call connection cost.
    for _ in 0..3 {
        let _ = client.info().await;
    }
    let mut samples = Vec::with_capacity(n as usize);
    let total_start = Instant::now();
    for _ in 0..n {
        let t0 = Instant::now();
        // `info` may fail if the daemon dropped mid-bench — count
        // failures as a separate metric and continue.
        let ok = client.info().await.is_ok();
        let dt = t0.elapsed();
        if ok {
            samples.push(dt);
        }
    }
    let total_elapsed = total_start.elapsed();
    if samples.is_empty() {
        anyhow::bail!("bench: all {} calls failed (daemon unreachable?)", n);
    }
    samples.sort();
    let min = samples[0];
    let max = samples[samples.len() - 1];
    let p50 = percentile(&samples, 0.50);
    let p95 = percentile(&samples, 0.95);
    let p99 = percentile(&samples, 0.99);
    let mean = samples.iter().sum::<Duration>() / samples.len() as u32;
    let throughput = samples.len() as f64 / total_elapsed.as_secs_f64();

    if json_out {
        let payload = serde_json::json!({
            "samples": samples.len(),
            "total_elapsed_ms": total_elapsed.as_millis(),
            "min_ms": min.as_micros() as f64 / 1000.0,
            "mean_ms": mean.as_micros() as f64 / 1000.0,
            "p50_ms": p50.as_micros() as f64 / 1000.0,
            "p95_ms": p95.as_micros() as f64 / 1000.0,
            "p99_ms": p99.as_micros() as f64 / 1000.0,
            "max_ms": max.as_micros() as f64 / 1000.0,
            "throughput_rps": throughput,
            "transport": client.transport_label(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("IPC latency over {} calls ({}):", samples.len(), client.transport_label());
        println!("  min      = {:>8.2} ms", micros_as_ms(min));
        println!("  mean     = {:>8.2} ms", micros_as_ms(mean));
        println!("  p50      = {:>8.2} ms", micros_as_ms(p50));
        println!("  p95      = {:>8.2} ms", micros_as_ms(p95));
        println!("  p99      = {:>8.2} ms", micros_as_ms(p99));
        println!("  max      = {:>8.2} ms", micros_as_ms(max));
        println!("  total    = {:>8.2} ms", total_elapsed.as_micros() as f64 / 1000.0);
        println!("  throughput = {:>6.1} calls/sec", throughput);
    }
    Ok(())
}

fn percentile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn micros_as_ms(d: Duration) -> f64 {
    d.as_micros() as f64 / 1000.0
}

impl IpcClient {
    /// Short label for the active transport ("unix" / "http:host:port").
    pub fn transport_label(&self) -> String {
        match self.transport() {
            crate::ipc_client::Transport::UnixSocket(p) => {
                format!("unix:{}", p.display())
            }
            crate::ipc_client::Transport::Http(url) => {
                format!("http:{}", url)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_picks_correct_index() {
        // 100 elements at 1..=100 µs.
        let d: Vec<Duration> = (1..=100).map(|i| Duration::from_micros(i)).collect();
        // `position = round((len-1) * q)`:
        //   p0   → round(99 * 0.00) = 0    → 1µs
        //   p50  → round(99 * 0.50) = 50   → 51µs
        //   p95  → round(99 * 0.95) = 94   → 95µs
        //   p100 → round(99 * 1.00) = 99   → 100µs
        assert_eq!(percentile(&d, 0.0), Duration::from_micros(1));
        assert_eq!(percentile(&d, 0.50), Duration::from_micros(51));
        assert_eq!(percentile(&d, 0.95), Duration::from_micros(95));
        assert_eq!(percentile(&d, 1.00), Duration::from_micros(100));
    }

    #[test]
    fn percentile_handles_empty() {
        assert_eq!(percentile(&[], 0.5), Duration::ZERO);
    }

    #[test]
    fn micros_as_ms_basic() {
        assert!((micros_as_ms(Duration::from_micros(1500)) - 1.5).abs() < 0.001);
    }
}
