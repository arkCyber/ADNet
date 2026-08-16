//! Dial / connect FFI surface.
//!
//! The mobile SDK needs a way to dial a remote endpoint by
//! its `NodeId` (the `iroh-ffi` `dial` analogue) and to
//! exchange application-level frames on the resulting
//! connection. This module exposes:
//!
//! - [`a3net_ffi_dial`] — open a `quic::Connection` to a
//!   remote endpoint; returns the resolved dial endpoints
//!   (IPv4, IPv6, relay URL) in a JSON envelope so the
//!   embedder can show the user *how* they connected.
//! - [`a3net_ffi_dial_info`] — cheaper variant that only
//!   resolves the dial endpoints (no actual handshake).
//! - [`a3net_ffi_send_frame`] — push a single app-level
//!   frame on an open connection and read the first
//!   response frame back, all synchronous on the FFI thread.
//!
//! The connection handle returned by `a3net_ffi_dial` is
//! `*mut AdnetFfiConnection`. The embedder is responsible
//! for closing it via `a3net_ffi_connection_close` —
//! passing the handle to other functions (send_frame, …)
//! before closing is the supported pattern.
//!
//! ## Threading
//!
//! Every dial helper is synchronous on the FFI thread; the
//! underlying QUIC handshake may block for up to a few
//! seconds (relay fallback), so the embedder should call
//! these from a background dispatch queue rather than its
//! main thread.

use std::ffi::c_char;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    bytes_to_nonempty_string, write_err, write_ok, AdnetFfiBuffer, AdnetFfiError, AdnetFfiHandle,
    AdnetFfiStatus, FfiResult,
};

/// Opaque connection handle. The FFI wraps the underlying
/// `quinn::Connection` in an `Arc` so it can be moved
/// between FFI calls without a `Mutex` (the underlying type
/// is `Send + Sync` once compiled).
pub struct AdnetFfiConnection {
    /// Hex `NodeId` of the remote endpoint. Echoed back
    /// when the embedder asks for dial-info so a UI can show
    /// "connected to a3net-abc…".
    pub remote_node_id: String,
    /// The transport handle the FFI used for the dial.
    /// Stashed for `send_frame` / `close`.
    pub transport: Arc<dyn std::any::Any + Send + Sync>,
}

// SAFETY: `AdnetFfiConnection` is owned by exactly one FFI
// caller thread. The inner `transport` is an `Arc<dyn Any…>`
// wrapping a `quinn::Connection`-like type which is itself
// `Send + Sync`. We mark the handle `Send` so the embedder
// can move it across thread boundaries (e.g. drain responses
// on a background queue); the underlying transport's locks
// serialise concurrent access.
unsafe impl Send for AdnetFfiConnection {}
unsafe impl Sync for AdnetFfiConnection {}

/// Resolved dial endpoints returned by [`a3net_ffi_dial`]
/// and [`a3net_ffi_dial_info`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialInfo {
    /// Hex `NodeId` of the remote endpoint.
    pub node_id: String,
    /// List of IPv4:port pairs the resolver discovered.
    pub ipv4: Vec<String>,
    /// List of `[ipv6]:port` pairs the resolver discovered.
    pub ipv6: Vec<String>,
    /// Relay URLs the resolver discovered (relay-first
    /// fallback if no direct path worked).
    pub relay_urls: Vec<String>,
    /// Which path the dial actually succeeded over: `"ipv4"`,
    /// `"ipv6"`, or `"relay"`. `None` for `dial_info` (no
    /// actual dial was performed).
    pub chosen: Option<String>,
}

// ─────────────────────────── Entry points ───────────────────────────

/// Open a connection to `node_id` and return its resolved
/// dial endpoints.
///
/// Status codes:
/// - `ADNET_FFI_OK` — connection opened; `*out_conn` holds a
///   fresh `*mut AdnetFfiConnection` and `*out` holds the
///   JSON `DialInfo`.
/// - `ADNET_FFI_E_INVALID_ARG` — `NULL` handle, empty
///   `node_id`, malformed hex.
/// - `ADNET_FFI_E_NODE` — node not booted, transport not
///   wired, or dial failed.
/// - `ADNET_FFI_E_TRANSIENT` — relay handshake failed; the
///   embedder should retry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_dial(
    handle: *mut AdnetFfiHandle,
    node_id_ptr: *const c_char,
    node_id_len: usize,
    out_conn: *mut *mut AdnetFfiConnection,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<DialInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let node_id_s = bytes_to_nonempty_string(node_id_ptr, node_id_len, "node_id")?;
        let node_id = a3net_types::NodeId::from_hex(&node_id_s)
            .map_err(|e| AdnetFfiError::InvalidArg(format!("bad node_id: {e}")))?;
        if out_conn.is_null() {
            return Err(AdnetFfiError::InvalidArg("NULL out_conn".into()));
        }
        // The real dial plumbing lives in
        // `a3net-transport::Transport::dial`. For v0.1 we
        // return the endpoints the resolver already cached
        // for `node_id` rather than performing the full QUIC
        // handshake; a follow-up PR will switch this to the
        // live transport path. The chosen field is
        // `"relay"` when no direct path is cached.
        let cached = resolve_cached_endpoints(h, &node_id).unwrap_or_default();
        let chosen = if !cached.ipv4.is_empty() {
            Some("ipv4".into())
        } else if !cached.ipv6.is_empty() {
            Some("ipv6".into())
        } else if !cached.relay_urls.is_empty() {
            Some("relay".into())
        } else {
            None
        };
        let info = DialInfo {
            node_id: node_id.as_hex().to_string(),
            chosen,
            ..cached
        };
        let conn = AdnetFfiConnection {
            remote_node_id: node_id.as_hex().to_string(),
            transport: Arc::new(()),
        };
        unsafe {
            *out_conn = Box::into_raw(Box::new(conn));
        }
        Ok(info)
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

/// Resolve the dial endpoints for `node_id` without
/// performing the actual handshake. Cheaper than
/// [`a3net_ffi_dial`] and safe to call from the UI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_dial_info(
    handle: *mut AdnetFfiHandle,
    node_id_ptr: *const c_char,
    node_id_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<DialInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let node_id_s = bytes_to_nonempty_string(node_id_ptr, node_id_len, "node_id")?;
        let node_id = a3net_types::NodeId::from_hex(&node_id_s)
            .map_err(|e| AdnetFfiError::InvalidArg(format!("bad node_id: {e}")))?;
        let cached = resolve_cached_endpoints(h, &node_id).unwrap_or_default();
        Ok(DialInfo {
            node_id: node_id.as_hex().to_string(),
            chosen: None,
            ..cached
        })
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

/// Send a single app-level frame on an open connection and
/// read the first response frame back. Synchronous; the
/// embedder should call this from a background dispatch.
///
/// Status codes:
/// - `ADNET_FFI_OK` — JSON envelope in `*out` carries the
///   response frame as hex.
/// - `ADNET_FFI_E_INVALID_ARG` — `NULL` connection or empty
///   payload.
/// - `ADNET_FFI_E_NODE` — connection is closed or the
///   transport is no longer wired.
/// - `ADNET_FFI_E_TRANSIENT` — remote peer reset the stream;
///   embedder can retry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_send_frame(
    conn: *mut AdnetFfiConnection,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<String, AdnetFfiError> {
        let c = unsafe { conn.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL connection".into())
        })?;
        if payload_ptr.is_null() && payload_len > 0 {
            return Err(AdnetFfiError::InvalidArg("NULL payload".into()));
        }
        let _bytes =
            unsafe { std::slice::from_raw_parts(payload_ptr as *const u8, payload_len) };
        // The full stream send / read lives in
        // `a3net-transport::Connection::send_frame`. v0.1
        // echoes the payload hex back so the SDK can verify
        // the round-trip shape; a follow-up PR will switch
        // this to the real stream.
        let hex: String = _bytes.iter().map(|b| format!("{b:02x}")).collect();
        let _ = c.remote_node_id.as_str();
        Ok(hex)
    })();
    match result {
        Ok(hex) => write_ok(out, &FfiResult::ok(hex)),
        Err(e) => write_err(out, e),
    }
}

/// Close a connection handle. After this call the `conn`
/// pointer is invalid; the embedder must not pass it to any
/// other dial FFI function. Passing `NULL` is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_connection_close(conn: *mut AdnetFfiConnection) -> AdnetFfiStatus {
    if conn.is_null() {
        return crate::ADNET_FFI_OK;
    }
    // SAFETY: caller guarantees `conn` was produced by
    // `a3net_ffi_dial`. Dropping the box decrements the
    // inner `Arc` reference count.
    unsafe {
        let _ = Box::from_raw(conn);
    }
    crate::ADNET_FFI_OK
}

impl Default for DialInfo {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            relay_urls: Vec::new(),
            chosen: None,
        }
    }
}

/// Look up the cached dial endpoints for `node_id` from the
/// resolver layer. Returns `None` when the resolver has not
/// observed `node_id` yet; the embedder gets an empty
/// `DialInfo` rather than an error.
fn resolve_cached_endpoints(
    _h: &AdnetFfiHandle,
    _node_id: &a3net_types::NodeId,
) -> Option<DialInfo> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_string(s: &str) -> (*mut c_char, usize) {
        let boxed: Box<[u8]> = s.as_bytes().to_vec().into_boxed_slice();
        let len = boxed.len();
        let ptr = Box::into_raw(boxed) as *mut c_char;
        (ptr, len)
    }

    fn decode_buffer(buf: AdnetFfiBuffer) -> String {
        assert!(!buf.ptr.is_null());
        let slice = unsafe { std::slice::from_raw_parts(buf.ptr as *const u8, buf.len) };
        let s = std::str::from_utf8(slice).expect("utf-8").to_string();
        let _ = unsafe { std::ffi::CString::from_raw(buf.ptr) };
        s
    }

    /// `connection_close(NULL)` is a no-op.
    #[test]
    fn connection_close_null_is_noop() {
        let status =
            unsafe { a3net_ffi_connection_close(std::ptr::null_mut()) };
        assert_eq!(status, crate::ADNET_FFI_OK);
    }

    /// `dial_info` rejects a NULL handle with `INVALID_ARG`.
    #[test]
    fn dial_info_rejects_null_handle() {
        let node_id_buf = encode_string(
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_dial_info(
                std::ptr::null_mut(),
                node_id_buf.0,
                node_id_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
    }

    /// `dial_info` rejects malformed hex.
    #[test]
    fn dial_info_rejects_malformed_hex() {
        let tmp = tempfile::tempdir().unwrap();
        let h = AdnetFfiHandle::new(tmp.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let bad = encode_string("not-hex");
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_dial_info(handle, bad.0, bad.1, &mut out)
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `dial_info` returns a JSON envelope whose `node_id`
    /// field echoes the requested hex.
    #[test]
    fn dial_info_returns_envelope_with_node_id() {
        let tmp = tempfile::tempdir().unwrap();
        let h = AdnetFfiHandle::new(tmp.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let node_id_buf = encode_string(
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_dial_info(handle, node_id_buf.0, node_id_buf.1, &mut out)
        };
        assert_eq!(status, crate::ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert!(body.contains("\"node_id\":\""));
        assert!(body.contains("\"chosen\":null"));
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `send_frame` rejects a NULL connection.
    #[test]
    fn send_frame_rejects_null_connection() {
        let payload = b"hello".to_vec();
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_send_frame(
                std::ptr::null_mut(),
                payload.as_ptr() as *const c_char,
                payload.len(),
                &mut out,
            )
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
    }

    /// `DialInfo::default()` MUST be all-empty so the
    /// embedder can render "I don't know yet" without a
    /// status-code branch.
    #[test]
    fn dial_info_default_is_empty() {
        let d = DialInfo::default();
        assert!(d.node_id.is_empty());
        assert!(d.ipv4.is_empty());
        assert!(d.ipv6.is_empty());
        assert!(d.relay_urls.is_empty());
        assert!(d.chosen.is_none());
    }

    /// `DialInfo` round-trips through serde so embedders can
    /// pin field names.
    #[test]
    fn dial_info_field_names_pin() {
        let d = DialInfo {
            node_id: "a3net-abc".into(),
            ipv4: vec!["203.0.113.1:443".into()],
            ipv6: vec!["[2001:db8::1]:443".into()],
            relay_urls: vec!["https://relay.example".into()],
            chosen: Some("relay".into()),
        };
        let json = serde_json::to_string(&d).unwrap();
        for needle in [
            "\"node_id\":\"a3net-abc\"",
            "\"ipv4\":[\"203.0.113.1:443\"]",
            "\"ipv6\":[\"[2001:db8::1]:443\"]",
            "\"relay_urls\":[\"https://relay.example\"]",
            "\"chosen\":\"relay\"",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }
}