//! WebRTC transport configuration.

use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default STUN servers. The full list is the same one the IETF draft
/// documents in section 7; we keep it as a `const` so it's trivially auditable.
pub const DEFAULT_STUN_SERVERS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
];

/// Default connection-establishment timeout. The browser side typically
/// converges in <5s when both peers are on the public Internet; the timeout
/// above that is mostly there for cellular networks where the initial
/// candidate gathering can take 10–20s.
pub const DEFAULT_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for the WebRTC transport.
///
/// This struct is intentionally **cheap to construct** when the `webrtc`
/// feature is off, so downstream code (CLI, FFI, config wizard) can hold
/// one in memory without paying the cold-build cost of `webrtc-rs`.
///
/// # Example
///
/// ```toml
/// [webrtc]
/// stun = ["stun:stun.l.google.com:19302"]
/// turn = [{ url = "turn:turn.example.com:3478", username = "u", credential = "c" }]
/// establish_timeout_ms = 30_000
/// max_datagram_bytes = 16_384
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebRtcConfig {
    /// STUN servers. Browsers and `webrtc-rs` use these to discover
    /// reflexive candidates. Empty list means "no STUN" — only works on
    /// the same LAN.
    #[serde(default = "WebRtcConfig::default_stun")]
    pub stun: Vec<String>,

    /// TURN servers. Use these when STUN alone is insufficient (symmetric
    /// NAT). Each entry has a URL and (optional) credentials.
    #[serde(default)]
    pub turn: Vec<TurnServer>,

    /// Maximum time we wait for ICE to reach the `Connected` state before
    /// giving up. Default 30s.
    #[serde(default = "WebRtcConfig::default_establish_timeout_ms")]
    pub establish_timeout_ms: u64,

    /// Maximum payload size of a single WebRTC DataChannel message. SCTP
    /// guarantees delivery up to ~16 KiB; larger values cause browser-side
    /// fragmentation and may stall on lossy links. We default to the safe
    /// 16 KiB.
    #[serde(default = "WebRtcConfig::default_max_datagram_bytes")]
    pub max_datagram_bytes: usize,

    /// Optional bind address for the local ICE UDP socket. Defaults to
    /// `0.0.0.0:0` (let the OS pick).
    #[serde(default)]
    pub bind: Option<SocketAddr>,

    /// Whether to enable the unordered+unreliable DataChannel variant for
    /// gossip fan-out. Default: true. Loss is acceptable for gossip but
    /// reduces latency.
    #[serde(default = "WebRtcConfig::default_true")]
    pub enable_unreliable_dc: bool,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            stun: Self::default_stun(),
            turn: Vec::new(),
            establish_timeout_ms: Self::default_establish_timeout_ms(),
            max_datagram_bytes: Self::default_max_datagram_bytes(),
            bind: None,
            enable_unreliable_dc: Self::default_true(),
        }
    }
}

impl WebRtcConfig {
    fn default_stun() -> Vec<String> {
        DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect()
    }

    const fn default_establish_timeout_ms() -> u64 {
        30_000
    }

    const fn default_max_datagram_bytes() -> usize {
        16 * 1024
    }

    const fn default_true() -> bool {
        true
    }

    /// Returns the ICE timeout as a `Duration`.
    pub fn establish_timeout(&self) -> Duration {
        Duration::from_millis(self.establish_timeout_ms)
    }
}

/// TURN server configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TurnServer {
    pub url: String,
    pub username: Option<String>,
    pub credential: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = WebRtcConfig::default();
        assert!(!cfg.stun.is_empty(), "STUN servers should be on by default");
        assert_eq!(cfg.max_datagram_bytes, 16 * 1024);
        assert_eq!(cfg.establish_timeout_ms, 30_000);
        assert!(cfg.enable_unreliable_dc);
    }

    #[test]
    fn roundtrip_json() {
        let cfg = WebRtcConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: WebRtcConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, back);
    }

    #[test]
    fn deserialize_minimal() {
        let json = "{}";
        let cfg: WebRtcConfig = serde_json::from_str(json).expect("deserialize minimal");
        assert!(!cfg.stun.is_empty());
        assert_eq!(cfg.max_datagram_bytes, 16 * 1024);
    }
}
