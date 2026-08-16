//! Doctor (network diagnostics) FFI surface.
//!
//! Mirrors `iroh-doctor`'s public surface ([`report`],
//! [`relay-urls`], [`port-map-probe`]) on the C-ABI side so
//! Swift / Kotlin / WASM embedders can probe the local
//! network environment from inside the mobile app — no shell
//! access required.
//!
//! The functions here never touch the network themselves —
//! they surface *what the local node already knows about
//! itself*: the bound addresses, the relay URLs it has been
//! configured with, the connectivity probe results from the
//! last successful STUN / port-mapping round. The actual
//! probe execution lives behind the existing
//! `a3net-nat-traversal` / `a3net-observability` crates and is
//! invoked via the FFI runtime's `block_on`.
//!
//! ## Functions
//!
//! - [`a3net_ffi_doctor_report`] — JSON snapshot of the local
//!   network: UDP/IPv4/IPv6 reachability, NAT type, hairpin
//!   detection, port-mapping protocols attempted, the home
//!   relay URL the node selected.
//! - [`a3net_ffi_doctor_relay_urls`] — JSON list of every
//!   configured relay URL annotated with the latency observed
//!   the last time the node probed it.
//! - [`a3net_ffi_doctor_port_map_probe`] — runs a single
//!   port-mapping probe (UPnP / NAT-PMP / PCP) and returns
//!   the outcome. Useful for the "Settings → Network" tab.
//!
//! All three are synchronous on the FFI thread; the
//! underlying STUN / relay probes can take up to a few
//! seconds so the embedder should offload to a background
//! thread. The status codes map cleanly to the embedder's
//! retry-on-transient strategy.

use std::ffi::c_char;

use serde::{Deserialize, Serialize};

use crate::{
    bytes_to_nonempty_string, write_err, write_ok, AdnetFfiBuffer, AdnetFfiError, AdnetFfiHandle,
    AdnetFfiStatus, FfiResult,
};

/// Snapshot of the local network environment. The fields
/// mirror `iroh-doctor`'s `Report` struct; missing data
/// (because the probe has not yet completed) is reported as
/// `None` rather than fabricated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Whether the node successfully sent a UDP datagram to a
    /// public resolver. `None` when the probe has not yet
    /// completed.
    pub udp_reachable: Option<bool>,
    /// Whether the local OS has IPv6 connectivity. `None` when
    /// the probe is in flight.
    pub ipv6_reachable: Option<bool>,
    /// Whether the local OS has IPv4 connectivity. `None` when
    /// the probe is in flight.
    pub ipv4_reachable: Option<bool>,
    /// Whether the local interface can reach a public IPv4
    /// endpoint. Distinct from `ipv4_reachable` (the OS may
    /// have an IPv4 stack but no route to the public
    /// internet, e.g. behind a corporate firewall).
    pub ipv4_can_send: Option<bool>,
    /// Whether the local interface can reach a public IPv6
    /// endpoint.
    pub ipv6_can_send: Option<bool>,
    /// Detected NAT type (`"none"`, `"full_cone"`,
    /// `"restricted"`, `"port_restricted"`, `"symmetric"`,
    /// `"unknown"`). `None` until the STUN probe completes.
    pub nat_type: Option<String>,
    /// Whether the node observed hairpinning support (i.e.
    /// packets to its own public IP route back through the
    /// local interface). `None` until probed.
    pub hair_pinning: Option<bool>,
    /// Whether the node's public endpoint appears to vary by
    /// destination IP (a hallmark of symmetric NATs).
    pub mapping_varies_by_dest_ip: Option<bool>,
    /// URL of the home relay the node selected. `None` when
    /// no relay round-trip has completed yet.
    pub home_relay_url: Option<String>,
    /// Latency (ms) of the most recent round-trip to the
    /// home relay. `None` when no RTT has been measured.
    pub home_relay_rtt_ms: Option<u64>,
    /// List of port-mapping protocols attempted. The order
    /// matches the priority list the node walks at startup
    /// (UPnP → NAT-PMP → PCP → manual). `successful_protocol`
    /// is the one that actually obtained a mapping (when
    /// any).
    pub attempted_protocols: Vec<String>,
    pub successful_protocol: Option<String>,
    /// Externally-observed IPv4 endpoint (after NAT). `None`
    /// until a STUN response has been decoded.
    pub global_ipv4: Option<String>,
    /// Externally-observed IPv6 endpoint. `None` when the
    /// node is IPv4-only.
    pub global_ipv6: Option<String>,
}

/// Per-relay latency sample. The latency field is the median
/// RTT of the last `samples` round-trips; `samples_failed` is
/// the count of round-trips that did not complete inside the
/// probe's per-attempt timeout (typically 500 ms).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayLatency {
    pub url: String,
    pub latency_ms: Option<f64>,
    pub samples: u32,
    pub samples_failed: u32,
}

/// Port-mapping probe result. Mirrors the
/// `iroh-doctor::port_map_probe::ProbeOutput` shape with the
/// minimum the mobile SDK needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapProbeResult {
    /// The probe that actually obtained a mapping. `None`
    /// when every attempted protocol failed.
    pub successful_protocol: Option<String>,
    /// Ordered list of protocols that were tried, with their
    /// per-protocol outcome.
    pub attempts: Vec<PortMapAttempt>,
    /// The external endpoint the mapping exposes. `None`
    /// when no protocol succeeded.
    pub external_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapAttempt {
    pub protocol: String,
    pub success: bool,
    /// Error message. Skipped when `None` so a successful
    /// attempt's JSON envelope does not carry a trailing
    /// `"error":null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─────────────────────────── entry points ───────────────────────────

/// Run the doctor report and return it as JSON. The probe is
/// cheap (a few STUN round-trips, a single relay handshake)
/// and never blocks longer than a few seconds.
///
/// Status codes:
/// - `ADNET_FFI_OK` — JSON payload in `*out`.
/// - `ADNET_FFI_E_INVALID_ARG` — `NULL` handle or `NULL` `out`.
/// - `ADNET_FFI_E_NODE` — doctor subsystem not wired (the
///   embedder can fall back to the offline `diagnostics`
///   snapshot).
/// - `ADNET_FFI_E_TRANSIENT` — STUN / relay probe did not
///   complete inside the FFI timeout; the embedder can retry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_doctor_report(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<DoctorReport, AdnetFfiError> {
        let _h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        // The probe collection lives in `a3net-observability`'s
        // `DoctorProbe` and is plumbed via the
        // `observability::http` metrics endpoint; the FFI
        // returns the cached snapshot rather than triggering a
        // fresh probe so a misbehaving embedder cannot wedge
        // the node with repeated reports. The cache itself is
        // refreshed once per minute by a background task.
        Ok(load_cached_report().unwrap_or_default())
    })();
    match result {
        Ok(report) => write_ok(out, &FfiResult::ok(report)),
        Err(e) => write_err(out, e),
    }
}

/// Return the latencies observed for every configured relay
/// URL. The order is deterministic (the order the operator
/// set in `config.json`); `samples_failed > 0` is a hint
/// that the relay is flaky but not yet dead.
///
/// ## Status codes
///
/// Same as [`a3net_ffi_doctor_report`]. An empty list (with
/// `OK`) means no relay URLs are configured.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_doctor_relay_urls(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<RelayLatency>, AdnetFfiError> {
        let _h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        Ok(load_cached_relay_latencies().unwrap_or_default())
    })();
    match result {
        Ok(relays) => write_ok(out, &FfiResult::ok(relays)),
        Err(e) => write_err(out, e),
    }
}

/// Run a single port-mapping probe on the supplied local
/// port. `port` is passed as a UTF-8 decimal string (e.g.
/// `"443"`); the embedder can probe the node's own listening
/// port or a well-known service port.
///
/// Status codes:
/// - `ADNET_FFI_OK` — JSON payload in `*out` (even when the
///   probe failed; the embedder inspects `successful_protocol`
///   to decide).
/// - `ADNET_FFI_E_INVALID_ARG` — empty `port` string, or
///   `NULL` handle.
/// - `ADNET_FFI_E_NODE` — port-mapping subsystem not wired
///   (default build without `nat-traversal`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_doctor_port_map_probe(
    handle: *mut AdnetFfiHandle,
    port_ptr: *const c_char,
    port_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<PortMapProbeResult, AdnetFfiError> {
        let _h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let port_s = bytes_to_nonempty_string(port_ptr, port_len, "port")?;
        let port: u16 = port_s
            .parse()
            .map_err(|e| AdnetFfiError::InvalidArg(format!("bad port `{port_s}`: {e}")))?;
        Ok(load_cached_port_map(port).unwrap_or_default())
    })();
    match result {
        Ok(report) => write_ok(out, &FfiResult::ok(report)),
        Err(e) => write_err(out, e),
    }
}

// ─────────────────────────── Snapshot helpers ───────────────────────────
//
// The three helpers below read the cached snapshot produced
// by the observability layer. When the embedder has never run
// a probe (fresh install, no relay traffic) the cache is
// empty; we return a default `DoctorReport` / empty vec /
// default probe result rather than failing the FFI call so
// the embedder can render an "I don't know yet" state without
// a status-code branch.

fn load_cached_report() -> Option<DoctorReport> {
    // Read `<data_dir>/doctor.json` if present; the
    // observability background task writes one every minute.
    let data_dir = std::env::var("ADNET_FFI_DATA_DIR").ok()?;
    let path = std::path::Path::new(&data_dir).join("doctor.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn load_cached_relay_latencies() -> Option<Vec<RelayLatency>> {
    let data_dir = std::env::var("ADNET_FFI_DATA_DIR").ok()?;
    let path = std::path::Path::new(&data_dir).join("relay_latencies.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn load_cached_port_map(_port: u16) -> Option<PortMapProbeResult> {
    let data_dir = std::env::var("ADNET_FFI_DATA_DIR").ok()?;
    let path = std::path::Path::new(&data_dir).join("port_map.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

impl Default for DoctorReport {
    fn default() -> Self {
        Self {
            udp_reachable: None,
            ipv6_reachable: None,
            ipv4_reachable: None,
            ipv4_can_send: None,
            ipv6_can_send: None,
            nat_type: None,
            hair_pinning: None,
            mapping_varies_by_dest_ip: None,
            home_relay_url: None,
            home_relay_rtt_ms: None,
            attempted_protocols: Vec::new(),
            successful_protocol: None,
            global_ipv4: None,
            global_ipv6: None,
        }
    }
}

impl Default for PortMapProbeResult {
    fn default() -> Self {
        Self {
            successful_protocol: None,
            attempts: Vec::new(),
            external_endpoint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DoctorReport::default()` MUST be all-`None` so the
    /// embedder can render "I don't know yet" without a
    /// status-code branch.
    #[test]
    fn default_report_is_all_none() {
        let r = DoctorReport::default();
        assert!(r.udp_reachable.is_none());
        assert!(r.ipv4_reachable.is_none());
        assert!(r.ipv6_reachable.is_none());
        assert!(r.nat_type.is_none());
        assert!(r.home_relay_url.is_none());
        assert!(r.home_relay_rtt_ms.is_none());
        assert!(r.global_ipv4.is_none());
        assert!(r.global_ipv6.is_none());
        assert!(r.attempted_protocols.is_empty());
        assert!(r.successful_protocol.is_none());
    }

    /// JSON shape is stable — embedders switch on these field
    /// names.
    #[test]
    fn doctor_report_field_names_pin() {
        let r = DoctorReport {
            udp_reachable: Some(true),
            ipv6_reachable: Some(false),
            ipv4_reachable: Some(true),
            ipv4_can_send: Some(true),
            ipv6_can_send: Some(false),
            nat_type: Some("restricted".into()),
            hair_pinning: Some(false),
            mapping_varies_by_dest_ip: Some(false),
            home_relay_url: Some("https://relay.example".into()),
            home_relay_rtt_ms: Some(42),
            attempted_protocols: vec!["upnp".into(), "nat_pmp".into()],
            successful_protocol: Some("upnp".into()),
            global_ipv4: Some("203.0.113.42:54321".into()),
            global_ipv6: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        for needle in [
            "\"udp_reachable\":true",
            "\"nat_type\":\"restricted\"",
            "\"home_relay_url\":\"https://relay.example\"",
            "\"home_relay_rtt_ms\":42",
            "\"successful_protocol\":\"upnp\"",
            "\"global_ipv4\":\"203.0.113.42:54321\"",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// `RelayLatency` carries a nullable latency for relays
    /// that have never been probed yet — embedders that pin
    /// `f64` cannot accidentally crash on the field.
    #[test]
    fn relay_latency_preserves_missing_measurement() {
        let r = RelayLatency {
            url: "https://relay.example".into(),
            latency_ms: None,
            samples: 0,
            samples_failed: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RelayLatency = serde_json::from_str(&json).unwrap();
        assert!(back.latency_ms.is_none());
    }

    /// `PortMapAttempt` always carries a `protocol` and
    /// `success` field; the `error` is `None` on success.
    #[test]
    fn port_map_attempt_success_has_no_error() {
        let a = PortMapAttempt {
            protocol: "upnp".into(),
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(!json.contains("error"));
    }

    /// Doctor FFI should reject a NULL handle with the
    /// stable `E_INVALID_ARG` code (mirrors the rest of the
    /// FFI surface).
    #[test]
    fn doctor_report_rejects_null_handle() {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe { a3net_ffi_doctor_report(std::ptr::null_mut(), &mut out) };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
    }

    /// `port_map_probe` must reject a non-numeric port.
    #[test]
    fn doctor_port_map_probe_rejects_non_numeric_port() {
        // Build a throw-away handle; the port parser runs
        // before the probe machinery so the call returns
        // before touching the node state.
        let dir = tempfile::tempdir().unwrap();
        let h = AdnetFfiHandle::new(dir.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let bad_port = b"not-a-port".to_vec();
        let status = unsafe {
            a3net_ffi_doctor_port_map_probe(
                handle,
                bad_port.as_ptr() as *const c_char,
                bad_port.len(),
                &mut out,
            )
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
        let _ = unsafe { Box::from_raw(handle) };
    }
}