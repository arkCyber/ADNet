//! News + announcements pub/sub FFI surface.
//!
//! Compiled only when the `news` feature is enabled on
//! `a3net-ffi`. Every function follows the canonical contract:
//! `(ptr, len)` byte slices for inputs and outputs, status
//! codes from `AdnetFfiStatus`, and JSON-encoded payloads.
//!
//! Functions:
//!
//! - `a3net_news_publish` — publish a bulletin (room, kind,
//!   severity, category, title, summary, body, signer, signature)
//! - `a3net_news_timeline` — paginated newest-first listing
//! - `a3net_news_get` — fetch a single bulletin by id
//! - `a3net_news_mark_read` — mark a bulletin read by the local
//!   node
//! - `a3net_news_subscribe` — start a live subscription; returns
//!   a handle
//! - `a3net_news_poll` — drain buffered events from the
//!   subscription
//! - `a3net_news_unsubscribe` — destroy the subscription handle

use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::Arc;

use a3net_news::{
    BulletinCategory, BulletinItem, BulletinKind, BulletinSeverity, InProcessGossip, NewsService,
    NewsServiceConfig, ValidationPolicy,
};
use a3net_types::{NodeId, RoomId};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::{
    bytes_to_nonempty_string, bytes_to_string, write_err, write_ok, AdnetFfiBuffer, AdnetFfiError,
    AdnetFfiStatus, FfiResult,
};/// Status code for "the `news` feature was not enabled at build
/// time" — keeps the surface visible to callers that probe
/// before using the API.
pub const ADNET_FFI_E_FEATURE_NEWS: AdnetFfiStatus = -10;

/// Input payload for `a3net_news_publish`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsPublishRequest {
    pub room: String,
    pub kind: String,
    pub severity: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    /// Optional wallet signature scheme (e.g. "ecdsa-secp256k1").
    /// Pass empty string when unsigned.
    #[serde(default)]
    pub signer_scheme: String,
    /// Hex-encoded wallet signature. Empty when unsigned.
    #[serde(default)]
    pub signer_address: String,
    /// Hex-encoded signature bytes. Empty when unsigned.
    #[serde(default)]
    pub signature_hex: String,
}

/// Output payload for `a3net_news_publish`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsPublishResult {
    pub bulletin_id: String,
    pub room: String,
    pub sequence: u32,
}

/// Output payload for `a3net_news_timeline`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsTimelineResult {
    pub entries: Vec<NewsTimelineEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsTimelineEntry {
    pub bulletin_id: String,
    pub room: String,
    pub author: String,
    pub sequence: u32,
    pub kind: String,
    pub category: String,
    pub severity: String,
    pub title: String,
    pub summary: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Per-room subscription handle. The mobile side keeps the
/// `u64` token and calls `a3net_news_poll` / `a3net_news_unsubscribe`.
#[repr(C)]
pub struct AdnetNewsSubHandle {
    inner: Option<SubState>,
}

/// Live subscription handle held by foreign callers until they
/// either consume all events via `a3net_news_poll` or release the
/// handle with `a3net_news_unsubscribe`. We hold the `NewsService`
/// + `Runtime` to keep the broadcast channel alive across poll
/// calls.
#[allow(dead_code)]
struct SubState {
    service: Arc<NewsService>,
    runtime: Arc<Runtime>,
    receiver: tokio::sync::broadcast::Receiver<a3net_news::BulletinEvent>,
    room: RoomId,
}

/// Output payload for `a3net_news_poll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsPollResult {
    pub events: Vec<NewsEventJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsEventJson {
    /// One of: `insert`, `correction`, `retraction`, `replay_complete`.
    pub kind: String,
    /// Bulletin id of the affected record (insert / corrected /
    /// retraction). Empty when the event is `replay_complete`.
    #[serde(default)]
    pub bulletin_id: String,
    /// Room id when relevant.
    #[serde(default)]
    pub room: String,
    /// Bulletin kind string (insert event only).
    #[serde(default)]
    pub bulletin_kind: String,
    /// Severity string (insert event only).
    #[serde(default)]
    pub severity: String,
    /// Bulletin title (insert event only).
    #[serde(default)]
    pub title: String,
    /// Bulleting id superseded by a correction or retraction.
    #[serde(default)]
    pub superseded_id: String,
    /// Replayed count for `replay_complete`.
    #[serde(default)]
    pub replayed: usize,
}

impl NewsEventJson {
    fn from_event(ev: a3net_news::BulletinEvent) -> Self {
        match ev {
            a3net_news::BulletinEvent::Insert(item) => NewsEventJson {
                kind: "insert".into(),
                bulletin_id: item.bulletin_id.to_string(),
                room: item.room_id.as_str().to_string(),
                bulletin_kind: item.kind.as_str().to_string(),
                severity: item.severity.as_str().to_string(),
                title: item.title,
                ..Default::default()
            },
            a3net_news::BulletinEvent::Correction {
                superseded_id,
                corrected,
            } => NewsEventJson {
                kind: "correction".into(),
                bulletin_id: corrected.bulletin_id.to_string(),
                superseded_id: superseded_id.to_string(),
                ..Default::default()
            },
            a3net_news::BulletinEvent::Retraction {
                superseded_id,
                retraction,
            } => NewsEventJson {
                kind: "retraction".into(),
                bulletin_id: retraction.bulletin_id.to_string(),
                superseded_id: superseded_id.to_string(),
                ..Default::default()
            },
            a3net_news::BulletinEvent::ReplayComplete { room, replayed } => {
                NewsEventJson {
                    kind: "replay_complete".into(),
                    room: room.as_str().to_string(),
                    replayed,
                    ..Default::default()
                }
            }
        }
    }
}

impl Default for NewsEventJson {
    fn default() -> Self {
        Self {
            kind: String::new(),
            bulletin_id: String::new(),
            room: String::new(),
            bulletin_kind: String::new(),
            severity: String::new(),
            title: String::new(),
            superseded_id: String::new(),
            replayed: 0,
        }
    }
}

// ─────────────────────────── helpers ───────────────────────────

fn open_service(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
) -> Result<(NewsService, Runtime), AdnetFfiError> {
    let data_dir = bytes_to_nonempty_string(data_dir_ptr, data_dir_len, "data_dir")?;
    let store_dir = PathBuf::from(&data_dir).join("news");
    std::fs::create_dir_all(&store_dir).map_err(|e| {
        AdnetFfiError::Node(format!("create news store dir: {e}"))
    })?;
    let runtime = Runtime::new().map_err(|e| AdnetFfiError::Runtime(e.to_string()))?;
    let transport = Arc::new(InProcessGossip::new());
    let cfg = NewsServiceConfig {
        store_dir,
        policy: ValidationPolicy::Audit,
        event_channel_capacity: 1024,
    };
    let node = NodeId::random();
    let svc = NewsService::open(node, transport, cfg)
        .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
    Ok((svc, runtime))
}

fn parse_kind(s: &str) -> Result<BulletinKind, AdnetFfiError> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "announcement" => BulletinKind::Announcement,
        "advisory" => BulletinKind::Advisory,
        "alert" | "news" | "news_article" | "newsarticle" | "article" => {
            BulletinKind::NewsArticle
        }
        "correction" => BulletinKind::Correction,
        "retraction" => BulletinKind::Retraction,
        other => {
            return Err(AdnetFfiError::Json(format!("unknown kind `{other}`")))
        }
    })
}

fn parse_severity(s: &str) -> Result<BulletinSeverity, AdnetFfiError> {
    use BulletinSeverity as B;
    Ok(match s.to_ascii_lowercase().as_str() {
        "info" => B::Info,
        "notice" | "notable" => B::Notable,
        "warning" | "important" => B::Important,
        "critical" => B::Critical,
        other => {
            return Err(AdnetFfiError::Json(format!("unknown severity `{other}`")))
        }
    })
}

fn parse_category(s: &str) -> Result<BulletinCategory, AdnetFfiError> {
    use BulletinCategory as C;
    Ok(match s.to_ascii_lowercase().as_str() {
        "general" => C::General,
        "security" => C::Security,
        "ops" | "outage" => C::Outage,
        "weather" => C::Weather,
        "health" => C::Health,
        "safety" => C::Safety,
        "traffic" => C::Traffic,
        "politics" => C::Politics,
        "economy" => C::Economy,
        "tech" => C::Tech,
        "community" => C::Community,
        "sports" => C::Sports,
        "culture" => C::Culture,
        other => {
            return Err(AdnetFfiError::Json(format!(
                "unknown category `{other}`"
            )))
        }
    })
}

// ─────────────────────────── entry points ───────────────────────────

/// Publish a bulletin. `payload` is JSON-encoded
/// [`NewsPublishRequest`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_publish(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<NewsPublishResult, AdnetFfiError> {
        let (svc, runtime) = open_service(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        let req: NewsPublishRequest = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        let kind = parse_kind(&req.kind)?;
        let severity = parse_severity(&req.severity)?;
        let category = parse_category(&req.category)?;
        let nonce = format!(
            "ffi-{}",
            chrono::Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or(0)
        );
        let mut item = BulletinItem::new(
            kind,
            category,
            severity,
            RoomId::new(&req.room),
            svc.local_node().clone(),
            &req.title,
            &req.summary,
            &req.body,
            nonce.as_bytes(),
            None,
        )
        .map_err(|e| AdnetFfiError::Node(format!("build bulletin: {e}")))?;
        if !req.signature_hex.is_empty() && !req.signer_address.is_empty() {
            let sig_bytes = hex_decode(&req.signature_hex)
                .map_err(|e| AdnetFfiError::Json(format!("signature_hex: {e}")))?;
            let addr_bytes = hex_decode(&req.signer_address)
                .map_err(|e| AdnetFfiError::Json(format!("signer_address: {e}")))?;
            if addr_bytes.len() != 20 {
                return Err(AdnetFfiError::Json(
                    "signer_address must be 20 bytes".into(),
                ));
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&addr_bytes);
            item.attach_signature(
                a3net_types::WalletAddress::from_bytes(addr),
                sig_bytes,
            );
        }
        let stored = runtime
            .block_on(svc.publish(item))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        Ok(NewsPublishResult {
            bulletin_id: stored.bulletin_id.to_string(),
            room: stored.room_id.as_str().to_string(),
            sequence: stored.sequence,
        })
    })();
    match result {
        Ok(v) => write_ok(out, &FfiResult::ok(v)),
        Err(e) => write_err(out, e),
    }
}

/// Fetch a paginated timeline. `payload` is JSON-encoded
/// `{ "room": "...", "before_seq": null|N, "limit": 20 }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_timeline(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<NewsTimelineResult, AdnetFfiError> {
        let (svc, _runtime) = open_service(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        #[derive(Deserialize)]
        struct Req {
            room: String,
            before_seq: Option<u32>,
            limit: usize,
        }
        let req: Req = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        let entries = svc
            .timeline(&RoomId::new(&req.room), req.before_seq, req.limit.max(1))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        let mapped = entries
            .into_iter()
            .map(|b| NewsTimelineEntry {
                bulletin_id: b.item.bulletin_id.to_string(),
                room: b.item.room_id.as_str().to_string(),
                author: b.item.author_id.to_string(),
                sequence: b.item.sequence,
                kind: b.item.kind.as_str().to_string(),
                category: b.item.category.as_str().to_string(),
                severity: b.item.severity.as_str().to_string(),
                title: b.item.title,
                summary: b.item.summary,
                created_at: b.item.created_at.timestamp(),
                expires_at: b.item.expires_at.timestamp(),
            })
            .collect();
        Ok(NewsTimelineResult { entries: mapped })
    })();
    match result {
        Ok(v) => write_ok(out, &FfiResult::ok(v)),
        Err(e) => write_err(out, e),
    }
}

/// Look up a single bulletin. `payload` is JSON-encoded
/// `{ "room": "...", "id": "<hex>" }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_get(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Option<NewsTimelineEntry>, AdnetFfiError> {
        let (svc, _runtime) = open_service(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        #[derive(Deserialize)]
        struct Req {
            room: String,
            id: String,
        }
        let req: Req = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        let id = a3net_types::BulletinId::from_hex(&req.id)
            .map_err(|e| AdnetFfiError::Json(format!("invalid id: {e}")))?;
        let stored = svc
            .get(&RoomId::new(&req.room), &id)
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        Ok(stored.map(|b| NewsTimelineEntry {
            bulletin_id: b.item.bulletin_id.to_string(),
            room: b.item.room_id.as_str().to_string(),
            author: b.item.author_id.to_string(),
            sequence: b.item.sequence,
            kind: b.item.kind.as_str().to_string(),
            category: b.item.category.as_str().to_string(),
            severity: b.item.severity.as_str().to_string(),
            title: b.item.title,
            summary: b.item.summary,
            created_at: b.item.created_at.timestamp(),
            expires_at: b.item.expires_at.timestamp(),
        }))
    })();
    match result {
        Ok(v) => write_ok(out, &FfiResult::ok(v)),
        Err(e) => write_err(out, e),
    }
}

/// Mark a bulletin read. `payload` is JSON-encoded
/// `{ "room": "...", "id": "<hex>" }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_mark_read(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<(), AdnetFfiError> {
        let (svc, _runtime) = open_service(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        #[derive(Deserialize)]
        struct Req {
            room: String,
            id: String,
        }
        let req: Req = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        let id = a3net_types::BulletinId::from_hex(&req.id)
            .map_err(|e| AdnetFfiError::Json(format!("invalid id: {e}")))?;
        svc.mark_read(&RoomId::new(&req.room), &id)
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        Ok(())
    })();
    match result {
        Ok(v) => write_ok(out, &FfiResult::ok(v)),
        Err(e) => write_err(out, e),
    }
}

/// Start a live subscription. Returns a handle via `*out`. The
/// `payload` is JSON-encoded `{ "room": "..." }`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_subscribe(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut *mut AdnetNewsSubHandle,
) -> AdnetFfiStatus {
    let result = (|| -> Result<*mut AdnetNewsSubHandle, AdnetFfiError> {
        let (svc, runtime) = open_service(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        #[derive(Deserialize)]
        struct Req {
            room: String,
        }
        let req: Req = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        let room = RoomId::new(&req.room);
        // Join the room before subscribing so we don't miss the
        // first gossip frames.
        runtime
            .block_on(svc.join_room(&room))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        let receiver = svc.subscribe();
        let handle = Box::into_raw(Box::new(AdnetNewsSubHandle {
            inner: Some(SubState {
                service: Arc::new(svc),
                runtime: Arc::new(runtime),
                receiver,
                room,
            }),
        }));
        Ok(handle)
    })();
    match result {
        Ok(handle) => {
            if !out.is_null() {
                unsafe {
                    *out = handle;
                }
                crate::ADNET_FFI_OK
            } else {
                // Caller passed NULL — release the handle so it
                // doesn't leak.
                unsafe {
                    let _ = Box::from_raw(handle);
                }
                AdnetFfiError::InvalidArg("NULL out pointer".into()).status()
            }
        }
        Err(e) => write_err(std::ptr::null_mut(), e),
    }
}

/// Drain buffered events from a subscription. The events are
/// JSON-encoded [`NewsPollResult`]. Returns `unit_ok` when the
/// subscription has been closed (the embedder should then call
/// `a3net_news_unsubscribe`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_poll(
    handle: *mut AdnetNewsSubHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<NewsPollResult, AdnetFfiError> {
        if handle.is_null() {
            return Err(AdnetFfiError::InvalidArg("NULL handle".into()));
        }
        let handle = unsafe { &mut *handle };
        let state = handle
            .inner
            .as_mut()
            .ok_or_else(|| AdnetFfiError::InvalidArg("already unsubscribed".into()))?;
        let mut events = Vec::new();
        loop {
            match state.receiver.try_recv() {
                Ok(ev) => events.push(NewsEventJson::from_event(ev)),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    handle.inner = None;
                    break;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }
        Ok(NewsPollResult { events })
    })();
    match result {
        Ok(v) => write_ok(out, &FfiResult::ok(v)),
        Err(e) => write_err(out, e),
    }
}

/// Release a subscription handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_news_unsubscribe(handle: *mut AdnetNewsSubHandle) -> AdnetFfiStatus {
    if handle.is_null() {
        return AdnetFfiError::InvalidArg("NULL handle".into()).status();
    }
    unsafe {
        let _ = Box::from_raw(handle);
    }
    crate::ADNET_FFI_OK
}

// ─────────────────────────── helpers ───────────────────────────

/// Decode a hex string into bytes. Returns an error string on
/// invalid characters or odd length.
fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("invalid hex nibble `{}`", c as char)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Copy a `&str` into a heap-allocated `*const c_char` that
    /// the FFI surface accepts. We deliberately leak the string
    /// here so the pointer stays valid across the synchronous
    /// FFI calls inside the test — `tempdir` ensures the
    /// process exits cleanly before the leak matters.
    fn cstr(s: &str) -> *const c_char {
        CString::new(s).unwrap().into_raw() as *const c_char
    }

    #[test]
    fn hex_decode_round_trip() {
        let s = "deadbeef";
        let bytes = hex_decode(s).unwrap();
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_rejects_invalid_char() {
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn parse_kind_handles_all_aliases() {
        for k in ["announcement", "advisory", "alert", "news", "correction", "retraction"] {
            parse_kind(k).unwrap();
        }
        assert!(parse_kind("unknown").is_err());
    }

    #[test]
    fn parse_severity_handles_all_aliases() {
        for s in ["info", "notice", "notable", "warning", "important", "critical"] {
            parse_severity(s).unwrap();
        }
        assert!(parse_severity("panic").is_err());
    }

    #[test]
    fn parse_category_handles_all_aliases() {
        for c in [
            "general", "security", "ops", "outage", "weather", "health", "safety",
            "traffic", "politics", "economy", "tech", "community", "sports", "culture",
        ] {
            parse_category(c).unwrap();
        }
        assert!(parse_category("misc").is_err());
    }

    // ───────────────────── end-to-end FFI flow ─────────────────────
    //
    // These tests exercise the full C-ABI surface against a real
    // on-disk `NewsService`. Each test owns its own tempdir to
    // keep the `BulletinStore` SQLite files isolated between
    // cases.

    use crate::{a3net_ffi_free, FfiResult};

    /// Run a closure with a freshly allocated `AdnetFfiBuffer`
    /// and ensure the buffer is released when the closure
    /// returns. We use `AssertUnwindSafe` because `SubState`
    /// (held across polls) contains an `Arc<NewsService>` whose
    /// internals are not unwind-safe — but our closures only
    /// operate on the FFI buffer, so this is sound in practice.
    fn with_buffer<F>(f: F) -> Vec<u8>
    where
        F: FnOnce(&mut AdnetFfiBuffer),
    {
        let mut buf = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        // The panic-safety branch needs to release the buffer
        // even though `buf` may already be partially moved
        // through the closure. Take ownership via `std::mem::ManuallyDrop`
        // semantics: stash the raw ptr/len so we can free it
        // unconditionally regardless of where the panic landed.
        let bytes = {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(&mut buf);
                let bytes = take_bytes(buf.ptr, buf.len);
                // Zero out so the success branch won't double-free.
                buf.ptr = std::ptr::null_mut();
                buf.len = 0;
                bytes
            }));
            match result {
                Ok(b) => b,
                Err(payload) => std::panic::resume_unwind(payload),
            }
        };
        if !buf.ptr.is_null() {
            unsafe { a3net_ffi_free(buf) };
        }
        bytes
    }

    fn take_bytes(ptr: *mut c_char, len: usize) -> Vec<u8> {
        assert!(!ptr.is_null() || len == 0);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(unsafe { *ptr.add(i) } as u8);
        }
        out
    }

    fn tempdir_str() -> String {
        let dir = tempfile::tempdir().unwrap();
        dir.path().to_string_lossy().into_owned()
    }

    fn bytes_of(json: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(json).unwrap()
    }

    fn ptr_of(bytes: &[u8]) -> (*const c_char, usize) {
        (bytes.as_ptr() as *const c_char, bytes.len())
    }

    #[test]
    fn end_to_end_publish_then_timeline_then_get_then_mark_read() {
        let dir = tempdir_str();

        // 1. Publish.
        let req = serde_json::json!({
            "room": "global:ops",
            "kind": "announcement",
            "severity": "info",
            "category": "general",
            "title": "Hello FFI",
            "summary": "first FFI post",
            "body": "hello body",
            "signer_scheme": "",
            "signer_address": "",
            "signature_hex": "",
        });
        let payload = bytes_of(&req);
        let (payload_ptr, payload_len) = ptr_of(&payload);
        let dir_ptr = cstr(&dir);
        let dir_len = dir.len();
        let status = unsafe {
            a3net_news_publish(
                dir_ptr,
                dir_len,
                payload_ptr,
                payload_len,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0, "publish status={status}");

        // Reparse the published bulletin id by issuing a
        // timeline query and grabbing the first id.
        let tl_req = serde_json::json!({
            "room": "global:ops",
            "before_seq": null,
            "limit": 16,
        });
        let tl_payload = bytes_of(&tl_req);
        let (tl_ptr, tl_len) = ptr_of(&tl_payload);
        let buf_bytes = with_buffer(|buf| unsafe {
            let st = a3net_news_timeline(dir_ptr, dir_len, tl_ptr, tl_len, buf);
            assert_eq!(st, 0, "timeline status={st}");
        });
        let parsed: FfiResult<NewsTimelineResult> =
            serde_json::from_slice(&buf_bytes).expect("timeline json");
        assert!(parsed.ok, "timeline returned err={:?}", parsed.error);
        let entries = parsed
            .value
            .expect("timeline value")
            .entries;
        let published_id = entries
            .first()
            .expect("at least one entry")
            .bulletin_id
            .clone();
        assert!(
            entries.iter().any(|e| e.bulletin_id == published_id),
            "timeline missing id"
        );

        // 3. get() should return a JSON `Value` we can inspect.
        let get_req = serde_json::json!({
            "room": "global:ops",
            "id": published_id,
        });
        let get_payload = bytes_of(&get_req);
        let (get_ptr, get_len) = ptr_of(&get_payload);
        let buf_bytes = with_buffer(|buf| unsafe {
            let st = a3net_news_get(dir_ptr, dir_len, get_ptr, get_len, buf);
            assert_eq!(st, 0, "get status={st}");
        });
        let parsed: FfiResult<Option<NewsTimelineEntry>> =
            serde_json::from_slice(&buf_bytes).expect("get json");
        assert!(parsed.ok, "get returned err={:?}", parsed.error);
        let item = parsed.value.expect("get value").expect("get item");
        assert_eq!(item.bulletin_id, published_id);
        assert_eq!(item.title, "Hello FFI");

        // 4. mark_read should succeed.
        let ack_req = serde_json::json!({
            "room": "global:ops",
            "id": published_id,
        });
        let ack_payload = bytes_of(&ack_req);
        let (ack_ptr, ack_len) = ptr_of(&ack_payload);
        let status = unsafe {
            a3net_news_mark_read(
                dir_ptr,
                dir_len,
                ack_ptr,
                ack_len,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0, "mark_read status={status}");
    }

    #[test]
    fn end_to_end_subscribe_polls_replayed_event() {
        let dir = tempdir_str();
        let dir_ptr = cstr(&dir);
        let dir_len = dir.len();

        // Publish one bulletin before subscribing so the lazy
        // replay path can emit it.
        let req = serde_json::json!({
            "room": "global:ops",
            "kind": "news",
            "severity": "notable",
            "category": "ops",
            "title": "Pre-sub",
            "summary": "pre-sub summary",
            "body": "pre-sub body",
            "signer_scheme": "",
            "signer_address": "",
            "signature_hex": "",
        });
        let payload = bytes_of(&req);
        let (payload_ptr, payload_len) = ptr_of(&payload);
        let status = unsafe {
            a3net_news_publish(
                dir_ptr,
                dir_len,
                payload_ptr,
                payload_len,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0, "publish status={status}");

        // Subscribe.
        let sub_req = serde_json::json!({ "room": "global:ops" });
        let sub_payload = bytes_of(&sub_req);
        let (sub_ptr, sub_len) = ptr_of(&sub_payload);
        let mut out_handle: *mut AdnetNewsSubHandle = std::ptr::null_mut();
        let status = unsafe {
            a3net_news_subscribe(
                dir_ptr,
                dir_len,
                sub_ptr,
                sub_len,
                &mut out_handle,
            )
        };
        assert_eq!(status, 0, "subscribe status={status}");
        assert!(!out_handle.is_null(), "subscribe handle was NULL");

        // Poll repeatedly. The first subscriber triggers lazy
        // replay, so the insert event should land within a
        // handful of polls.
        let mut found_insert = false;
        for _ in 0..16 {
            let buf_bytes = with_buffer(|buf| unsafe {
                let st = a3net_news_poll(out_handle, buf);
                assert_eq!(st, 0, "poll status={st}");
            });
            if buf_bytes.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(15));
                continue;
            }
            let parsed: FfiResult<NewsPollResult> =
                serde_json::from_slice(&buf_bytes).expect("poll json");
            assert!(parsed.ok, "poll err={:?}", parsed.error);
            if let Some(poll) = parsed.value {
                if poll
                    .events
                    .iter()
                    .any(|e| e.kind == "insert" && !e.bulletin_id.is_empty())
                {
                    found_insert = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(found_insert, "expected insert event from FFI poll");

        // Unsubscribe releases the handle.
        let status = unsafe { a3net_news_unsubscribe(out_handle) };
        assert_eq!(status, 0, "unsubscribe status={status}");
    }

    #[test]
    fn publish_rejects_missing_required_fields() {
        let dir = tempdir_str();
        let dir_ptr = cstr(&dir);
        let dir_len = dir.len();

        // `kind`/`title` missing on purpose — the call must
        // return a non-zero status with a structured error
        // payload, never panic.
        let payload = br#"{"room":"global:ops"}"#.to_vec();
        let (payload_ptr, payload_len) = ptr_of(&payload);
        let mut buf = AdnetFfiBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let status = unsafe {
            a3net_news_publish(
                dir_ptr,
                dir_len,
                payload_ptr,
                payload_len,
                &mut buf,
            )
        };
        assert!(
            status != 0 || !buf.ptr.is_null(),
            "expected either non-zero status or populated error buffer"
        );
        if !buf.ptr.is_null() {
            let bytes = take_bytes(buf.ptr, buf.len);
            unsafe { a3net_ffi_free(buf) };
            let parsed: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap();
            assert!(
                parsed.get("ok") == Some(&serde_json::json!(false)),
                "expected ok=false error envelope, got: {parsed}"
            );
        }
    }
}
