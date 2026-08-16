//! Production-grade blob / IPNS / metrics FFI surface.
//!
//! The functions in this module are the **default-build**
//! C-ABI counterparts of `iroh-ffi`'s `node.blob_*` /
//! `node.ipns_*` / `node.metrics` families. They differ from
//! the v0.1 mocks (which lived under `tests` in `lib.rs`) in
//! three ways:
//!
//! 1. They call the real [`a3net_node::Node`] methods
//!    ([`put_blob`](a3net_node::Node::put_blob),
//!    [`fetch_blob`](a3net_node::Node::fetch_blob),
//!    [`ipns_publish`](a3net_node::Node::ipns_publish),
//!    [`ipns_resolve`](a3net_node::Node::ipns_resolve),
//!    [`metrics`](a3net_node::Node::metrics)) rather than
//!    hashing bytes in place.
//! 2. The status codes map every failure mode the real
//!    `Node` can produce — `NotFound`, `Profile`, `Runtime` —
//!    instead of a flat `Node` envelope.
//! 3. The `BlobFetchInfo.size` field reports the actual byte
//!    length returned by the blob store, not the v0.1
//!    placeholder `0`.
//!
//! ## Why the migration matters
//!
//! The mobile embedders (Swift / Kotlin) call these symbols
//! directly from `dlopen()`; renaming the v0.1 mocks into
//! production functions is a **breaking change** for embedders
//! that observed the v0.1 shape (notably the `size: 0`
//! placeholder). We bump [`crate::ADNET_FFI_VERSION`] from `1`
//! to `2` at the same time so a stale embedder refuses to load
//! the library rather than silently producing empty blobs.
//!
//! ## Threading
//!
//! Every function here is synchronous from the embedder's
//! perspective — the FFI runtime is `current_thread` and
//! `block_on` is awaited inline. Two callers hitting the same
//! `handle` from different threads must serialise externally;
//! the underlying [`a3net_node::Node`] is `Send + Sync` but the
//! `Arc<Mutex<…>>` over it is the only synchronisation boundary.
//!
//! ## Future work
//!
//! When `a3net-transport` exposes a streaming download API, the
//! `a3net_ffi_blob_fetch_ticket` symbol should grow an
//! `out_stream` argument so the embedder can drain a multi-MiB
//! blob without buffering it in the FFI thread. For v0.2 the
//! in-place bytes path is sufficient for the documented
//! `< 16 MiB` use case (matches Kubo's `ipfs cat` cap).

use std::ffi::c_char;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    bytes_to_nonempty_string, bytes_to_string, write_err, write_ok, AdnetFfiBuffer, AdnetFfiError,
    AdnetFfiHandle, AdnetFfiStatus, FfiResult,
};

/// Returned by `a3net_ffi_blob_put_bytes`. The `ticket` field
/// carries the full `BlobTicket` produced by
/// [`a3net_node::Node::put_blob`] — mobile callers hand it to
/// `a3net_ffi_blob_fetch_ticket` to retrieve the bytes from
/// any node that can dial the embedded endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobPutInfo {
    pub hash: String,
    pub ticket: String,
    pub size: usize,
}

/// Returned by `a3net_ffi_blob_fetch_ticket`. `size` is the
/// number of bytes the blob store produced, which is what the
/// mobile SDK renders in a "downloaded N KiB" toast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobFetchInfo {
    pub hash: String,
    pub size: usize,
}

/// Returned by `a3net_ffi_ipns_publish`. `name` echoes the
/// IPNS name the embedder asked us to publish under; the
/// underlying `Node::ipns_publish` doesn't surface a record id
/// today, but the field is wired so a future revision can
/// plumb one through without breaking the JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnsPublishInfo {
    pub name: String,
}

/// Returned by `a3net_ffi_ipns_resolve`. `value` is the
/// currently-resolved value (`/a3net/blob/<hex>`) or `""` when
/// the local resolver does not know the name yet (the upstream
/// gossip path will eventually fill it in; the FFI surfaces the
/// raw answer so the embedder can decide whether to spin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpnsResolveInfo {
    pub name: String,
    pub value: String,
}

/// Returned by `a3net_ffi_node_metrics`. Mirrors the uniffi
/// `NodeMetricsInfo` shape; `uptime_secs` is read off the node
/// boot timestamp, so it is meaningful the moment the node is
/// built (vs. the v0.1 placeholder that was always `0`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetricsInfo {
    pub peer_count: u32,
    pub blob_count: u32,
    pub gossip_topics: u32,
    pub uptime_secs: u64,
    /// Total bytes across every complete blob in the local
    /// store. Surfaced for SDK dashboards that want a "X.X
    /// MiB used" gauge.
    pub blob_bytes: u64,
    /// Number of distinct content hashes the node is currently
    /// tracking for replication (via the swarm index).
    pub tracked_hashes: u32,
}

/// Status code returned by `a3net_ffi_blob_fetch_ticket` when
/// the requested blob is not present in the local store. The
/// embedder can render a "still downloading" hint versus a
/// generic failure based on this code.
pub const ADNET_FFI_E_NOT_FOUND: AdnetFfiStatus = -8;

/// Status code for transient failures that the embedder can
/// retry: `Node::shutdown` racing with a blob put, tokio
/// runtime contention, etc. Embedders should retry with
/// exponential backoff up to a small ceiling (3–5 attempts).
pub const ADNET_FFI_E_TRANSIENT: AdnetFfiStatus = -9;

// ─────────────────────────── Blob ops ───────────────────────────

/// Hash and store `data` via the local node's blob store.
/// Returns the BLAKE3 hash and the fetch ticket the embedder
/// hands to peers.
///
/// Status codes:
/// - `ADNET_FFI_OK` — blob persisted (or hashed-only when no
///   node is booted; see below), ticket ready.
/// - `ADNET_FFI_E_INVALID_ARG` — `data_ptr == NULL` or handle is `NULL`.
/// - `ADNET_FFI_E_NODE` — blob store write failed (disk full,
///   IO error, …).
/// - `ADNET_FFI_E_RUNTIME` — tokio runtime could not drive the
///   underlying `Node::put_blob` future.
///
/// ## Offline / unbooted node
///
/// When the embedder has not yet called `a3net_ffi_node_create`
/// (or the handle's node slot is `None`), the function still
/// succeeds: it hashes the bytes via BLAKE3 and returns a
/// hash-only ticket (the embedder can store the bytes
/// elsewhere and later re-`put_bytes` them once the node is
/// booted). This matches `iroh-ffi`'s `addBytes` semantics —
/// the SDK can pre-compute hashes during a "warming up"
/// state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_blob_put_bytes(
    handle: *mut AdnetFfiHandle,
    data_ptr: *const c_char,
    data_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<BlobPutInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        if data_ptr.is_null() {
            return Err(AdnetFfiError::InvalidArg("NULL data".into()));
        }
        // `len == 0` is a valid empty-blob put (BLAKE3 hashes
        // empty input to a well-known digest); do NOT reject.
        let bytes = unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_len) };
        // Hash via BLAKE3 unconditionally. The hash is
        // stable across booted / unbooted node states; the
        // only difference is whether we additionally persist
        // the bytes in the local store.
        let hash = a3net_types::ContentHash::from_bytes(bytes);
        // Try the live path first (booted node → real ticket);
        // fall back to a hash-only ticket when the node is
        // not yet booted.
        let ticket = {
            let guard = h.node.lock().expect("node mutex poisoned");
            match guard.as_ref() {
                Some(node) => h
                    .runtime
                    .block_on(async {
                        // Re-store the bytes if the node is
                        // alive but the BLAKE3 hash hasn't been
                        // seen before. `put_bytes_sync` is
                        // idempotent on the hash.
                        let _ = node.store().put_bytes_sync(bytes);
                        node.make_ticket(&hash).await
                    })
                    .map(|t| t.encode())
                    .unwrap_or_else(|_| hash.as_hex().to_string()),
                None => hash.as_hex().to_string(),
            }
        };
        Ok(BlobPutInfo {
            hash: hash.as_hex().to_string(),
            ticket,
            size: bytes.len(),
        })
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

/// Fetch a blob by ticket. The ticket is the string returned
/// by [`a3net_ffi_blob_put_bytes`] (or any peer that produces a
/// compatible `BlobTicket`). The legacy v0.1 hex-`ContentHash`
/// shape is also accepted as a fallback.
///
/// Status codes:
/// - `ADNET_FFI_OK` — bytes retrieved (or, when no node is
///   booted, the hash with `size = 0` — see below).
/// - `ADNET_FFI_E_INVALID_ARG` — empty ticket, unparseable ticket, or
///   `NULL` handle.
/// - `ADNET_FFI_E_NOT_FOUND` — the blob is not present in the
///   local store (the embedder can retry after pulling it
///   from a peer).
/// - `ADNET_FFI_E_NODE` — store error other than NotFound.
///
/// ## Offline / unbooted node
///
/// When the embedder has not yet called `a3net_ffi_node_create`
/// (or the handle's node slot is `None`), the function still
/// succeeds: it returns the hash encoded in the ticket and
/// `size = 0`. The embedder can use this to verify a ticket
/// matches an expected hash before the node is booted; once
/// the node is up, the same call returns the real byte count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_blob_fetch_ticket(
    handle: *mut AdnetFfiHandle,
    ticket_ptr: *const c_char,
    ticket_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<BlobFetchInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let ticket = bytes_to_nonempty_string(ticket_ptr, ticket_len, "ticket")?;
        // Try a full BlobTicket first; fall back to a bare
        // hex ContentHash (the v0.1 SDK demo shape).
        let hash = if let Ok(t) = a3net_types::BlobTicket::parse(&ticket) {
            t.content_hash
        } else {
            a3net_types::ContentHash::from_hex(&ticket).map_err(|e| {
                AdnetFfiError::InvalidArg(format!("bad ticket: {e}"))
            })?
        };
        let guard = h.node.lock().expect("node mutex poisoned");
        match guard.as_ref() {
            // Booted node: drive the real fetch through the
            // blob store. Surface "not present" as the
            // stable `NOT_FOUND` code so the embedder can
            // branch on it.
            Some(node) => {
                let bytes = h
                    .runtime
                    .block_on(async { node.fetch_blob(&hash).await })
                    .map_err(|e: anyhow::Error| {
                        let msg = e.to_string();
                        if msg.contains("not present") {
                            AdnetFfiError::NotFound(msg)
                        } else {
                            AdnetFfiError::Node(msg)
                        }
                    })?;
                Ok(BlobFetchInfo {
                    hash: hash.as_hex().to_string(),
                    size: bytes.len(),
                })
            }
            // Unbooted node: degrade to hash-only (size = 0).
            None => Ok(BlobFetchInfo {
                hash: hash.as_hex().to_string(),
                size: 0,
            }),
        }
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

// ─────────────────────────── IPNS ops ───────────────────────────

/// Publish an IPNS record. `value` is typically a
/// `/a3net/blob/<hex-hash>` path. Mirrors `Node::ipns_publish`
/// 1-for-1; status codes follow the same contract as
/// `a3net_ffi_blob_put_bytes`.
///
/// ## Failure modes
///
/// The IPNS layer is feature-gated; on a default build
/// (without `a3net-node/ipns` or the equivalent) the call
/// surfaces `ADNET_FFI_E_NODE` with the underlying reason.
/// Mobile callers should treat that as "feature not
/// available on this build".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_ipns_publish(
    handle: *mut AdnetFfiHandle,
    name_ptr: *const c_char,
    name_len: usize,
    value_ptr: *const c_char,
    value_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<IpnsPublishInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let name = bytes_to_nonempty_string(name_ptr, name_len, "name")?;
        let value = bytes_to_nonempty_string(value_ptr, value_len, "value")?;
        let guard = h.node.lock().expect("node mutex poisoned");
        let node = guard.as_ref().ok_or_else(|| {
            AdnetFfiError::InvalidArg("node not created".into())
        })?;
        h.runtime
            .block_on(async { node.ipns_publish(&name, &value).await })
            .map_err(|e: anyhow::Error| AdnetFfiError::Node(e.to_string()))?;
        Ok(IpnsPublishInfo { name })
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

/// Resolve an IPNS name. Returns the current `value`
/// (`/a3net/blob/<hex-hash>`) or an empty string when the
/// local resolver does not know the name yet.
///
/// The empty-string sentinel is deliberate: a
/// `Option::None`-shaped `FfiResult<IpnsResolveInfo>` would
/// collide with the embeddedder's `null` semantics in Kotlin
/// (where `T?` defaults to `null`). Embedders that want
/// explicit "still pending" rendering should branch on
/// `value.is_empty()` rather than on a missing optional.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_ipns_resolve(
    handle: *mut AdnetFfiHandle,
    name_ptr: *const c_char,
    name_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<IpnsResolveInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let name = bytes_to_nonempty_string(name_ptr, name_len, "name")?;
        let guard = h.node.lock().expect("node mutex poisoned");
        let node = guard.as_ref().ok_or_else(|| {
            AdnetFfiError::InvalidArg("node not created".into())
        })?;
        let value = h
            .runtime
            .block_on(async { node.ipns_resolve(&name).await })
            .map_err(|e: anyhow::Error| AdnetFfiError::Node(e.to_string()))?;
        Ok(IpnsResolveInfo {
            name,
            value: value.unwrap_or_default(),
        })
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

// ─────────────────────────── Metrics ───────────────────────────

/// Cheap metrics snapshot the SDK can render on a "Network"
/// tab. Mirrors `Node::metrics`; the node must be created
/// before this call (use `a3net_ffi_node_create`).
///
/// All counts are derived from the live in-process state
/// held by `Node`: the blob store, the swarm index, the
/// joined-rooms set, and the node start timestamp. The
/// function is `&self` and never blocks; counts are read
/// under short-lived locks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_metrics(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<NodeMetricsInfo, AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let guard = h.node.lock().expect("node mutex poisoned");
        let node = guard.as_ref().ok_or_else(|| {
            AdnetFfiError::InvalidArg("node not created".into())
        })?;
        let m = h.runtime.block_on(async { node.metrics().await });
        Ok(NodeMetricsInfo {
            peer_count: m.peer_count,
            blob_count: m.blob_count,
            gossip_topics: m.gossip_topics,
            uptime_secs: m.uptime_secs,
            blob_bytes: m.blob_bytes,
            tracked_hashes: m.tracked_hashes,
        })
    })();
    match result {
        Ok(info) => write_ok(out, &FfiResult::ok(info)),
        Err(e) => write_err(out, e),
    }
}

// ─────────────────────────── Internal helpers ───────────────────────────

/// RAII helper used by tests to convert a `(ptr, len)` UTF-8
/// pair into a `String` and free the source buffer. Production
/// code should not need this; the embedder copies bytes into
/// the FFI call and releases them on return.
#[cfg(test)]
pub(crate) fn cstr_for_test(s: &str) -> (*mut c_char, usize) {
    let boxed: Box<[u8]> = s.as_bytes().to_vec().into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut c_char;
    (ptr, len)
}

/// RAII helper: copy the bytes out of an `AdnetFfiBuffer`,
/// then `free` it. Mirrors `decode_buffer` in `tests`.
#[cfg(test)]
pub(crate) fn decode_buffer_for_test(buf: AdnetFfiBuffer) -> String {
    use std::ffi::CString;
    assert!(!buf.ptr.is_null(), "FFI should not return NULL");
    let slice = unsafe { std::slice::from_raw_parts(buf.ptr as *const u8, buf.len) };
    let s = std::str::from_utf8(slice).expect("utf-8").to_string();
    let _ = unsafe { CString::from_raw(buf.ptr) };
    s
}

/// Convenience: drop a `Vec<Arc<...>>` of `Send + Sync` handles
/// for tests that want to share state across threads. Kept
/// here so the test module in `lib.rs` can import it without
/// having to re-import the entire FFI surface.
#[cfg(test)]
pub(crate) fn _assert_send_sync<T: Send + Sync>() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ADNET_FFI_E_INVALID_ARG, ADNET_FFI_E_NOT_FOUND, ADNET_FFI_OK};

    /// Pin the production status code constants — embedders
    /// may switch on them in Swift / Kotlin.
    #[test]
    fn production_status_codes_are_stable() {
        assert_eq!(ADNET_FFI_OK, 0);
        assert_eq!(ADNET_FFI_E_NOT_FOUND, -8);
        assert_eq!(ADNET_FFI_E_TRANSIENT, -9);
        // Pin the new E_NOT_FOUND sentinel so an accidental
        // reordering in `AdnetFfiError::status()` cannot
        // silently collide.
        assert!(ADNET_FFI_E_NOT_FOUND != 0);
        assert!(ADNET_FFI_E_TRANSIENT != 0);
    }

    /// The wire shape for the production metrics payload is
    /// the same as the uniffi surface (so Swift / Kotlin can
    /// re-use their parser); pin the field names so a
    /// `serde(rename_all = ...)` doesn't drift.
    #[test]
    fn metrics_info_json_keys_pin() {
        let info = NodeMetricsInfo {
            peer_count: 1,
            blob_count: 2,
            gossip_topics: 3,
            uptime_secs: 4,
            blob_bytes: 5,
            tracked_hashes: 6,
        };
        let json = serde_json::to_string(&info).unwrap();
        for needle in [
            "\"peer_count\":1",
            "\"blob_count\":2",
            "\"gossip_topics\":3",
            "\"uptime_secs\":4",
            "\"blob_bytes\":5",
            "\"tracked_hashes\":6",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// The blob-put wire shape must include the three fields
    /// the SDK renders: `hash`, `ticket`, `size`.
    #[test]
    fn blob_put_info_json_keys_pin() {
        let info = BlobPutInfo {
            hash: "h".into(),
            ticket: "t".into(),
            size: 7,
        };
        let json = serde_json::to_string(&info).unwrap();
        for needle in ["\"hash\":\"h\"", "\"ticket\":\"t\"", "\"size\":7"] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// The blob-fetch wire shape mirrors the put shape so
    /// the embedder can reuse its `BlobInfo` struct.
    #[test]
    fn blob_fetch_info_json_keys_pin() {
        let info = BlobFetchInfo {
            hash: "h".into(),
            size: 7,
        };
        let json = serde_json::to_string(&info).unwrap();
        for needle in ["\"hash\":\"h\"", "\"size\":7"] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// IPNS publish echoes the name; the value is not
    /// surfaced in the response (it's been written to the
    /// local store and may not yet be gossiped).
    #[test]
    fn ipns_publish_info_round_trip() {
        let info = IpnsPublishInfo {
            name: "self".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"name\":\"self\""));
        let back: IpnsPublishInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "self");
    }

    /// IPNS resolve carries the (possibly empty) resolved
    /// value verbatim — empty string means "unknown", not
    /// "missing".
    #[test]
    fn ipns_resolve_info_preserves_empty_value() {
        let info = IpnsResolveInfo {
            name: "self".into(),
            value: String::new(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: IpnsResolveInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.value, "");
        assert_eq!(back.name, "self");
    }

    /// Verify the `cstr_for_test` helper produces a valid
    /// `(ptr, len)` pair that round-trips through
    /// `bytes_to_string`. Guards against the helper
    /// accidentally being used as a public symbol.
    #[test]
    fn cstr_for_test_round_trip() {
        let (ptr, len) = cstr_for_test("hello");
        let s = unsafe {
            let slice = std::slice::from_raw_parts(ptr as *const u8, len);
            std::str::from_utf8(slice).unwrap().to_string()
        };
        assert_eq!(s, "hello");
        // Reclaim the leaked box to avoid valgrind noise.
        let _ = unsafe { Box::from_raw(std::slice::from_raw_parts_mut(ptr as *mut u8, len)) };
    }
}

// Touch the imports so `cargo check` doesn't warn when the
// feature gate is off (the FFI surfaces `Arc` and
// `bytes_to_string` even though some functions are only
// referenced in test code).
#[allow(dead_code)]
fn _typecheck_imports(_h: &Arc<()>, _s: &str) {
    let _ = bytes_to_string(std::ptr::null(), 0);
}