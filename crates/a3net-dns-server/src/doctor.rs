//! `a3net-dns-server doctor` — quick diagnostic helper for the
//! self-hosted DNS server binary.
//!
//! Mirrors `iroh doctor` at a smaller surface so operators can
//! smoke-test a freshly-installed DNS server before pointing
//! real traffic at it:
//!
//! - `report`  — bind / zone / state file / upstream reachable.
//! - `upstream` — probe the configured pkarr relay URL.
//! - `state`   — describe the on-disk journal (path, count,
//!   oldest record TTL).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::DnsServerConfig;
use crate::http::HttpApi;

/// Aggregated snapshot for `a3net-dns-server doctor report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub bind: String,
    pub zone: String,
    pub upstream: Option<String>,
    pub upstream_reachable: Option<bool>,
    pub upstream_latency_ms: Option<u64>,
    pub state_file: Option<String>,
    pub zone_record_count: usize,
}

/// Print the human/JSON report and exit. `bind`/`zone`/`upstream`
/// come from the parsed config; `state_file` and `zone_record_count`
/// come from the `HttpApi` already constructed by `main`.
pub fn run_report(cfg: &DnsServerConfig, api: &HttpApi, json: bool) -> Result<(), String> {
    let (state_file, count) = describe_state(api);
    let (reachable, latency) = match cfg.pkarr_relay.as_deref() {
        Some(url) => match futures::executor::block_on(probe_upstream(url)) {
            Ok(ms) => (Some(true), Some(ms)),
            Err(_) => (Some(false), None),
        },
        None => (None, None),
    };
    let report = DoctorReport {
        bind: cfg.bind.to_string(),
        zone: cfg.zone.clone(),
        upstream: cfg.pkarr_relay.clone(),
        upstream_reachable: reachable,
        upstream_latency_ms: latency,
        state_file: state_file.map(|p| p.to_string_lossy().into_owned()),
        zone_record_count: count,
    };
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => return Err(format!("json encode: {e}")),
        }
    } else {
        println!("DNS server doctor report:");
        println!("  bind           : {}", report.bind);
        println!("  zone           : {}", report.zone);
        println!("  upstream       : {:?}", report.upstream);
        println!(
            "  upstream OK    : {}",
            report
                .upstream_reachable
                .map(|b| b.to_string())
                .unwrap_or_else(|| "n/a".into())
        );
        if let Some(ms) = report.upstream_latency_ms {
            println!("  upstream RTT   : {ms} ms");
        }
        println!(
            "  state file     : {}",
            report
                .state_file
                .as_deref()
                .unwrap_or("(in-memory only)")
        );
        println!("  zone records   : {}", report.zone_record_count);
    }
    Ok(())
}

/// Standalone helper for `doctor upstream <url>` — probes the
/// configured pkarr relay's `/pkarr/<z32>` endpoint and returns
/// the round-trip latency in milliseconds.
pub async fn probe_upstream(url: &str) -> Result<u64, String> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("build reqwest client: {e}")),
    };
    let probe_url = format!("{}/health", url.trim_end_matches('/'));
    let started = std::time::Instant::now();
    let resp = match client.get(&probe_url).send().await {
        Ok(r) => r,
        Err(e) => return Err(format!("GET {probe_url}: {e}")),
    };
    let status = resp.status().as_u16();
    if status >= 500 {
        return Err(format!("upstream returned {status}"));
    }
    Ok(started.elapsed().as_millis() as u64)
}

fn describe_state(api: &HttpApi) -> (Option<PathBuf>, usize) {
    // HttpApi doesn't expose the raw state path; we recompute
    // the count via `list()` and we recover the path from the
    // `state_path()` accessor when available.
    let count = api.list().len();
    let path = api.state_path();
    (path, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serializes_as_json() {
        let r = DoctorReport {
            bind: "0.0.0.0:53".into(),
            zone: "a3net.test".into(),
            upstream: None,
            upstream_reachable: None,
            upstream_latency_ms: None,
            state_file: Some("/var/lib/dns.json".into()),
            zone_record_count: 7,
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: DoctorReport = serde_json::from_str(&j).unwrap();
        assert_eq!(back.zone, "a3net.test");
        assert_eq!(back.zone_record_count, 7);
    }

    #[tokio::test]
    async fn probe_upstream_rejects_invalid_url() {
        let err = probe_upstream("not a url").await.unwrap_err();
        // reqwest surfaces URL-parse errors as either a
        // "relative URL" rejection (when the input lacks a
        // scheme) or a generic builder error. Both are
        // acceptable as long as we get *some* failure path.
        assert!(
            err.contains("relative URL")
                || err.contains("URL")
                || err.contains("builder error")
                || err.contains("invalid"),
            "got: {err}"
        );
    }
}