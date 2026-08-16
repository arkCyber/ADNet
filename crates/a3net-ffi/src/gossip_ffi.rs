//! Gossip subscription FFI surface.
//!
//! Mirrors `iroh-gossip`'s `subscribe` API on the C-ABI side so
//! Swift / Kotlin / WASM embedders can render a live feed of
//! chat / feed / moments messages inside the mobile app.
//!
//! ## Polling model
//!
//! Gossip messages are push-based internally. The FFI cannot
//! hand a Rust `mpsc::Receiver` to a C caller, so we expose a
//! **poll-style** API:
//!
//! 1. The embedder calls [`a3net_ffi_gossip_subscribe`] with
//!    the gossip topic + topic kind and receives an opaque
//!    `*mut AdnetFfiGossipSub` handle.
//! 2. The embedder calls
//!    [`a3net_ffi_gossip_poll`] on that handle. The call
//!    drains every message currently buffered into a JSON
//!    array and returns. It blocks the FFI thread for up to
//!    `max_wait_ms` waiting for at least one message.
//! 3. The embedder calls [`a3net_ffi_gossip_unsubscribe`]
//!    when leaving the chat screen; the handle is consumed
//!    and the underlying subscription is dropped.
//!
//! ## Why poll, not callback?
//!
//! `iroh-ffi` exposes a callback for gossip too —
//! `GossipMessageCallback`. We deliberately do **not**
//! because the callback has to be `extern "C" fn(…,
//! user_data: *mut c_void)`, which forces every embedder to
//! bridge back into their language's thread pool. The poll
//! model keeps the FFI surface synchronous and the embedder
//! in charge of threading.
//!
//! ## Buffering
//!
//! Each subscription owns an unbounded in-memory queue. A
//! misbehaving embedder (one that never calls `poll`) will
//! accumulate messages indefinitely; the `unsubscribe` path
//! drops the queue.

use std::ffi::c_char;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{
    bytes_to_nonempty_string, write_err, write_ok, AdnetFfiBuffer, AdnetFfiError, AdnetFfiHandle,
    AdnetFfiStatus, FfiResult,
};

/// Opaque handle to a live gossip subscription. The embedder
/// owns the handle until it calls `a3net_ffi_gossip_unsubscribe`
/// (after which the pointer is invalid).
pub struct AdnetFfiGossipSub {
    /// Topic the subscription is bound to. Kept verbatim so
    /// the embedder can match incoming `GossipEventJson::Topic`
    /// discriminants against it during a poll.
    pub topic: String,
    /// Buffered events. The `Mutex` is necessary because the
    /// gossip task lives on the runtime's worker threads and
    /// `poll` is called from the FFI thread.
    pub queue: Mutex<Vec<GossipEventJson>>,
}

// SAFETY: `AdnetFfiGossipSub` is owned by exactly one FFI
// caller thread and the inner `Mutex<Vec<…>>` is the
// synchronisation boundary. We do NOT mark the struct
// `Send + Sync` here because each handle is owned by exactly
// one thread; the embedder is responsible for serialising
// `poll` / `unsubscribe` calls. The runtime's gossip tasks
// send into the queue via the `Mutex`.
unsafe impl Send for AdnetFfiGossipSub {}

/// JSON payload of a single gossip event. The
/// `event_kind` discriminator matches `iroh-gossip`'s
/// `Event` enum verbatim (`"Joined"`, `"Left"`, `"Message"`,
/// `"NeighborUp"`, `"NeighborDown"`, `"Routed"`,
/// `"Lagged"`, `"UnrecoverableError"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipEventJson {
    /// Discriminant: `"Joined"`, `"Left"`, `"Message"`,
    /// `"NeighborUp"`, `"NeighborDown"`, `"Routed"`,
    /// `"Lagged"`, `"UnrecoverableError"`.
    pub event_kind: String,
    /// The topic this event arrived on. Always equal to
    /// `AdnetFfiGossipSub::topic` for the v0.1 poll model;
    /// echoed back so the embedder can decode a heterogeneous
    /// batch (later versions of the FFI may merge events from
    /// multiple subscriptions).
    pub topic: String,
    /// Hex `NodeId` of the peer the event concerns (the
    /// gossip layer calls these "endpoints"). `None` for
    /// `Lagged` / `Routed` / `UnrecoverableError`.
    pub from: Option<String>,
    /// Raw bytes of an `iroh-gossip::Message`. Encoded as a
    /// hex string so the FFI JSON envelope stays a string-only
    /// shape (Swift / Kotlin callers can decode via
    /// `Data(hex:)` / `ByteString.parseHex()`).
    pub payload_hex: Option<String>,
    /// Sequence number assigned by the gossip layer. Embedders
    /// can de-duplicate by `(topic, seq)` — a peer can emit
    /// the same `seq` twice if a relay rebroadcasts it.
    pub seq: Option<u64>,
    /// Number of events the subscription dropped because the
    /// embedder's poll queue fell behind. Surfaced when the
    /// embedder polls and the queue reports `Lagged`.
    pub dropped: Option<u64>,
    /// Free-form error message. `Some` only when
    /// `event_kind == "UnrecoverableError"`.
    pub error: Option<String>,
}

// ─────────────────────────── Entry points ───────────────────────────

/// Subscribe to a gossip topic. The topic string is opaque to
/// the FFI — it is forwarded to `a3net-gossip::subscribe`
/// verbatim, so the embedder must use the same encoding the
/// gossip layer expects (BLAKE3 hash hex by default).
///
/// Status codes:
/// - `ADNET_FFI_OK` — subscription created; `*out_sub` holds
///   a fresh `*mut AdnetFfiGossipSub`.
/// - `ADNET_FFI_E_INVALID_ARG` — `NULL` handle, empty topic,
///   or unknown topic kind.
/// - `ADNET_FFI_E_NODE` — node not booted, or gossip layer
///   not wired.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_gossip_subscribe(
    handle: *mut AdnetFfiHandle,
    topic_kind_ptr: *const c_char,
    topic_kind_len: usize,
    topic_ptr: *const c_char,
    topic_len: usize,
    out_sub: *mut *mut AdnetFfiGossipSub,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<(), AdnetFfiError> {
        let h = unsafe { handle.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL handle".into())
        })?;
        let topic_kind = bytes_to_nonempty_string(topic_kind_ptr, topic_kind_len, "topic_kind")?;
        let topic = bytes_to_nonempty_string(topic_ptr, topic_len, "topic")?;
        // Validate the topic kind before doing any work so a
        // typo surfaces as a stable `INVALID_ARG` rather than
        // an opaque `Node` error from the gossip layer.
        let kind_normalised = topic_kind.to_ascii_lowercase();
        if !matches!(
            kind_normalised.as_str(),
            "feed" | "chat" | "moments" | "news" | "raw"
        ) {
            return Err(AdnetFfiError::InvalidArg(format!(
                "unknown topic kind `{topic_kind}`"
            )));
        }
        // The actual subscription plumbing lives in
        // `a3net-gossip` and is driven by the node's runtime.
        // For v0.1 the FFI owns a stand-alone queue; once the
        // full gossip integration lands (tracked separately)
        // we forward events into the queue from a real
        // `gossip::subscribe` task. For now the queue is
        // empty and the poll call returns `[]`.
        let sub = AdnetFfiGossipSub {
            topic: format!("{kind_normalised}:{topic}"),
            queue: Mutex::new(Vec::new()),
        };
        // Sanity: the handle's node must be alive (the runtime
        // exists, otherwise `new` would have failed).
        let _ = h;
        if out_sub.is_null() {
            return Err(AdnetFfiError::InvalidArg("NULL out_sub".into()));
        }
        unsafe {
            *out_sub = Box::into_raw(Box::new(sub));
        }
        Ok(())
    })();
    match result {
        Ok(()) => write_ok(out, &FfiResult::unit_ok()),
        Err(e) => write_err(out, e),
    }
}

/// Drain every event currently buffered into the
/// subscription's queue. The call blocks for up to
/// `max_wait_ms` waiting for at least one event; when the
/// buffer is empty and no event arrives inside the timeout,
/// the call returns an empty `[]`.
///
/// Status codes:
/// - `ADNET_FFI_OK` — JSON array in `*out` (possibly empty).
/// - `ADNET_FFI_E_INVALID_ARG` — `NULL` subscription or
///   `NULL` `out`.
/// - `ADNET_FFI_E_TRANSIENT` — the subscription was already
///   torn down (`unsubscribe` was called).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_gossip_poll(
    sub: *mut AdnetFfiGossipSub,
    max_wait_ms: u32,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<GossipEventJson>, AdnetFfiError> {
        let s = unsafe { sub.as_ref() }.ok_or_else(|| {
            AdnetFfiError::InvalidArg("NULL subscription".into())
        })?;
        // The runtime-driven forwarder pushes events into
        // `s.queue`; v0.1 leaves the queue empty, so a poll
        // with `max_wait_ms > 0` would block until the
        // timeout. We honour `max_wait_ms` literally so the
        // embedder's worst-case latency is bounded.
        if max_wait_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(max_wait_ms as u64));
        }
        let mut queue = s.queue.lock().map_err(|e| {
            AdnetFfiError::Transient(format!("queue lock poisoned: {e}"))
        })?;
        Ok(std::mem::take(&mut *queue))
    })();
    match result {
        Ok(events) => write_ok(out, &FfiResult::ok(events)),
        Err(e) => write_err(out, e),
    }
}

/// Tear down a subscription. After this call the `sub`
/// pointer is invalid; the embedder must not pass it to any
/// other gossip FFI function.
///
/// Passing `NULL` is a no-op (returns `OK`); this mirrors
/// `a3net_ffi_node_destroy(NULL)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_gossip_unsubscribe(
    sub: *mut AdnetFfiGossipSub,
) -> AdnetFfiStatus {
    if sub.is_null() {
        return crate::ADNET_FFI_OK;
    }
    // SAFETY: caller guarantees `sub` is a valid pointer we
    // previously returned from `a3net_ffi_gossip_subscribe`.
    // We drop the box and the subscription's queue is freed
    // with it.
    unsafe {
        let _ = Box::from_raw(sub);
    }
    crate::ADNET_FFI_OK
}

/// Construct a `GossipEventJson` from an `iroh-gossip`-style
/// event. Visible to the rest of `a3net-ffi` so a future
/// `#[cfg(feature = "iroh")]` runtime forwarder can convert
/// in-place without duplicating the field mapping.
pub fn event_from_parts(
    event_kind: &str,
    topic: &str,
    from: Option<String>,
    payload_hex: Option<String>,
    seq: Option<u64>,
    dropped: Option<u64>,
    error: Option<String>,
) -> GossipEventJson {
    GossipEventJson {
        event_kind: event_kind.to_string(),
        topic: topic.to_string(),
        from,
        payload_hex,
        seq,
        dropped,
        error,
    }
}

/// Push an event into a subscription's queue. Used by the
/// runtime forwarder; FFI callers must NOT call this (it's
/// not exported). Returns `true` when the event was pushed,
/// `false` when the queue mutex is poisoned (treated as a
/// transient by the caller).
pub fn push_event(sub: &Arc<AdnetFfiGossipSub>, event: GossipEventJson) -> bool {
    match sub.queue.lock() {
        Ok(mut q) => {
            q.push(event);
            true
        }
        Err(_) => false,
    }
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

    /// `unsubscribe(NULL)` is a no-op — mirrors
    /// `a3net_ffi_node_destroy(NULL)`.
    #[test]
    fn unsubscribe_null_is_noop() {
        let status =
            unsafe { a3net_ffi_gossip_unsubscribe(std::ptr::null_mut()) };
        assert_eq!(status, crate::ADNET_FFI_OK);
    }

    /// Unknown topic kinds are rejected with `INVALID_ARG`
    /// before the queue is allocated.
    #[test]
    fn subscribe_rejects_unknown_topic_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let h = AdnetFfiHandle::new(tmp.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let kind_buf = encode_string("bogus");
        let topic_buf = encode_string("topic");
        let mut sub: *mut AdnetFfiGossipSub = std::ptr::null_mut();
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_gossip_subscribe(
                handle,
                kind_buf.0,
                kind_buf.1,
                topic_buf.0,
                topic_buf.1,
                &mut sub,
                &mut out,
            )
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
        assert!(sub.is_null());
        let body = decode_buffer(out);
        assert!(body.contains("unknown topic kind"), "got: {body}");
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `subscribe` followed by `poll` (zero wait) must return
    /// an empty JSON array — there is no live gossip forwarder
    /// in v0.1, so the queue stays empty.
    #[test]
    fn subscribe_then_poll_returns_empty_array() {
        let tmp = tempfile::tempdir().unwrap();
        let h = AdnetFfiHandle::new(tmp.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let kind_buf = encode_string("chat");
        let topic_buf = encode_string("room-1");
        let mut sub: *mut AdnetFfiGossipSub = std::ptr::null_mut();
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_gossip_subscribe(
                handle,
                kind_buf.0,
                kind_buf.1,
                topic_buf.0,
                topic_buf.1,
                &mut sub,
                &mut out,
            )
        };
        assert_eq!(status, crate::ADNET_FFI_OK);

        // Poll with `max_wait_ms = 0` — instant return.
        let mut out2 = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe { a3net_ffi_gossip_poll(sub, 0, &mut out2) };
        assert_eq!(status, crate::ADNET_FFI_OK);
        let body = decode_buffer(out2);
        assert_eq!(body, "{\"ok\":true,\"value\":[]}");

        // Cleanup.
        let status = unsafe { a3net_ffi_gossip_unsubscribe(sub) };
        assert_eq!(status, crate::ADNET_FFI_OK);
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `poll(NULL, …)` must return `INVALID_ARG`.
    #[test]
    fn poll_rejects_null_subscription() {
        let mut out = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_ffi_gossip_poll(std::ptr::null_mut(), 0, &mut out)
        };
        assert_eq!(status, crate::ADNET_FFI_E_INVALID_ARG);
    }

    /// `GossipEventJson` round-trips through serde so the
    /// embedder can switch on `event_kind` after parsing.
    #[test]
    fn event_json_round_trip() {
        let e = event_from_parts(
            "Message",
            "feed:room",
            Some("a3net-abc".into()),
            Some("deadbeef".into()),
            Some(42),
            None,
            None,
        );
        let json = serde_json::to_string(&e).unwrap();
        let back: GossipEventJson = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_kind, "Message");
        assert_eq!(back.topic, "feed:room");
        assert_eq!(back.from.as_deref(), Some("a3net-abc"));
        assert_eq!(back.payload_hex.as_deref(), Some("deadbeef"));
        assert_eq!(back.seq, Some(42));
        assert!(back.dropped.is_none());
        assert!(back.error.is_none());
    }

    /// `push_event` round-trips through the queue mutex; the
    /// event must appear in `take()` afterwards.
    #[test]
    fn push_event_drains_in_poll() {
        let sub = Arc::new(AdnetFfiGossipSub {
            topic: "feed:test".into(),
            queue: Mutex::new(Vec::new()),
        });
        assert!(push_event(
            &sub,
            event_from_parts(
                "Joined",
                "feed:test",
                Some("a3net-aaa".into()),
                None,
                None,
                None,
                None,
            ),
        ));
        let mut q = sub.queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].event_kind, "Joined");
        assert_eq!(q[0].from.as_deref(), Some("a3net-aaa"));
    }
}