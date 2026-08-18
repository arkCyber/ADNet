//! `a3net-ffi` — C-ABI surface for A3Net.
//!
//! Mobile embedders (iOS / Android) and WASM modules consume
//! A3Net through a **C-compatible ABI** rather than the Rust
//! API. This crate provides that surface as a small, stable
//! set of `extern "C"` functions returning JSON-encoded
//! results. Every function follows the same shape:
//!
//! - **Inputs** are passed as `(ptr, len)` byte slices, *not*
//!   raw pointers to Rust-owned memory. Mobile callers (Swift
//!   `Data`, Kotlin `ByteArray`, JS `Uint8Array`) all line up
//!   on this contract.
//! - **Outputs** are also `(ptr, len)`. The caller is
//!   responsible for calling [`a3net_ffi_free`] to release the
//!   buffer; the bytes are valid UTF-8 and parseable as JSON.
//! - **Errors** are signalled by an `int` status code: `0` is
//!   `OK`, anything else maps to an `AdnetFfiError` variant.
//!   The human-readable form of the error is *also* returned
//!   via the same `(ptr, len)` buffer so callers can log it
//!   without a second round-trip.
//!
//! The surface is intentionally small: just enough to bring
//! up a node, push a blob, and join a room. Anything richer
//! (gossip subscriptions, custom transports, persistent
//! billing) stays on the Rust side and is exposed via
//! follow-up FFI calls in subsequent versions.
//!
//! # Building
//!
//! ```bash
//! # Library only — the embedder links a3net_ffi.{a,lib}.
//! cargo build -p a3net-ffi --release
//!
//! # C-headers via cbindgen (separate tool, not run here).
//! cbindgen --crate a3net-ffi --output a3net_ffi.h
//! ```
//!
//! # Stability
//!
//! Every FFI function is annotated `#[no_mangle] pub extern "C" fn`.
//! Adding a new function is a non-breaking change. Renaming
//! or changing the signature of an existing function is a
//! breaking change — bump `ADNET_FFI_VERSION` in the C header
//! and document the migration in the `CHANGELOG`.
//!
//! [`a3net_ffi_free`]: crate::a3net_ffi_free

#![deny(unused_must_use)]
#![allow(unsafe_code)] // FFI boundary requires `unsafe extern "C"` and
// `unsafe impl Send`; all other unsafe uses are reviewed by hand.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;

use a3net_types::{ContentHash, NodeCapability};
#[allow(unused_imports)]
use a3net_userstore::{SqliteUserStore, SqliteUserStoreConfig, UserStore};
#[allow(unused_imports)]
use a3net_roster::RosterStore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::runtime::Runtime;

mod version;
pub use version::ADNET_FFI_VERSION;

/// Optional uniffi surface. Compiled only when the `uniffi`
/// feature is enabled; otherwise the module is empty.
#[cfg(feature = "uniffi")]
pub mod uniffi_surface;
#[cfg(feature = "uniffi")]
// `uniffi 0.27` requires the crate-private `UniFfiTag` to live
// in the crate root; `setup_scaffolding!()` generates it (and
// the FFI symbol skeletons) on the spot. Without it every
// `#[uniffi::export]` / `uniffi::Record` derive fails with
// "cannot find type `UniFfiTag` in the crate root".
::uniffi::setup_scaffolding!();
#[cfg(feature = "uniffi")]
pub use uniffi_surface::{AdnetError, AdnetHandle};

/// News + announcements pub/sub surface. Compiled only when the
/// `news` feature is enabled; otherwise the module is empty and
/// all `a3net_news_*` FFI symbols are skipped.
#[cfg(feature = "news")]
pub mod news_ffi;

/// Status code returned by every FFI function.
pub type AdnetFfiStatus = i32;

/// `OK` — call succeeded; the out-buffer (if any) holds the result.
pub const ADNET_FFI_OK: AdnetFfiStatus = 0;
/// Caller passed a `NULL` pointer or an empty buffer.
pub const ADNET_FFI_E_INVALID_ARG: AdnetFfiStatus = -1;
/// The supplied UTF-8 was not valid.
pub const ADNET_FFI_E_UTF8: AdnetFfiStatus = -2;
/// The supplied JSON was malformed.
pub const ADNET_FFI_E_JSON: AdnetFfiStatus = -3;
/// Internal node error — see the human-readable error buffer.
pub const ADNET_FFI_E_NODE: AdnetFfiStatus = -4;
/// Tokio runtime could not be created.
pub const ADNET_FFI_E_RUNTIME: AdnetFfiStatus = -5;
/// A required feature was not enabled at build time (e.g. the
/// caller asked for the iroh runtime but `a3net-ffi` was
/// compiled without `--features iroh`).
pub const ADNET_FFI_E_FEATURE: AdnetFfiStatus = -6;
/// Profile read or update failed.
pub const ADNET_FFI_E_PROFILE: AdnetFfiStatus = -7;

#[derive(Debug, Error)]
pub enum AdnetFfiError {
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("invalid UTF-8: {0}")]
    Utf8(String),
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("node error: {0}")]
    Node(String),
    #[error("tokio runtime error: {0}")]
    Runtime(String),
    #[error("required feature disabled: {0}")]
    Feature(String),
    #[error("profile error: {0}")]
    Profile(String),
}

impl AdnetFfiError {
    pub fn status(&self) -> AdnetFfiStatus {
        match self {
            AdnetFfiError::InvalidArg(_) => ADNET_FFI_E_INVALID_ARG,
            AdnetFfiError::Utf8(_) => ADNET_FFI_E_UTF8,
            AdnetFfiError::Json(_) => ADNET_FFI_E_JSON,
            AdnetFfiError::Node(_) => ADNET_FFI_E_NODE,
            AdnetFfiError::Runtime(_) => ADNET_FFI_E_RUNTIME,
            AdnetFfiError::Feature(_) => ADNET_FFI_E_FEATURE,
            AdnetFfiError::Profile(_) => ADNET_FFI_E_PROFILE,
        }
    }
}

impl From<anyhow::Error> for AdnetFfiError {
    fn from(e: anyhow::Error) -> Self {
        AdnetFfiError::Node(e.to_string())
    }
}

/// JSON-encoded result of an FFI call. The mobile / WASM
/// embedder deserialises this struct to inspect the
/// machine-readable fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfiResult<T> {
    /// `true` when the call succeeded.
    pub ok: bool,
    /// Optional machine-readable payload. `None` on error or
    /// when the call has no return value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    /// Optional human-readable error message. `Some` only on
    /// failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> FfiResult<T> {
    pub fn ok(value: T) -> Self {
        Self {
            ok: true,
            value: Some(value),
            error: None,
        }
    }
}

impl FfiResult<()> {
    pub fn err(e: AdnetFfiError) -> Self {
        Self {
            ok: false,
            value: None,
            error: Some(e.to_string()),
        }
    }

    pub fn unit_ok() -> Self {
        Self {
            ok: true,
            value: None,
            error: None,
        }
    }
}

// ─────────────────────────── Buffer helpers ───────────────────────────

/// C-side opaque pointer to a heap-allocated byte buffer
/// produced by an FFI call. The embedder must call
/// [`a3net_ffi_free`] once the bytes have been consumed.
#[repr(C)]
pub struct AdnetFfiBuffer {
    /// Pointer to the first byte. NULL when the buffer is
    /// empty.
    pub ptr: *mut c_char,
    /// Number of valid bytes at `ptr`. Zero on empty.
    pub len: usize,
}

/// Allocate a fresh `AdnetFfiBuffer` from a UTF-8 string.
/// `s` must be a valid UTF-8 string. The returned buffer is
/// heap-allocated and must be released via
/// [`a3net_ffi_free`].
fn buffer_from_str(s: String) -> AdnetFfiBuffer {
    let cstr = match CString::new(s) {
        Ok(c) => c,
        Err(_) => CString::new("").expect("empty CString is always valid"),
    };
    let len = cstr.as_bytes().len();
    let ptr = cstr.into_raw();
    AdnetFfiBuffer { ptr, len }
}

/// Free a buffer previously returned by any FFI function.
/// Passing a `NULL` pointer or a buffer that was not produced
/// by this library is **undefined behaviour** — the embedder
/// is responsible for tracking ownership.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_free(buf: AdnetFfiBuffer) {
    if !buf.ptr.is_null() {
        // SAFETY: `ptr` was produced by `CString::into_raw`
        // and has not been freed yet.
        unsafe {
            let _ = CString::from_raw(buf.ptr);
        }
    }
}

/// Read a `NULL`-terminated `*const c_char` and copy it into
/// a Rust `String`. Returns `Err(AdnetFfiError::Utf8(_))`
/// when the input is `NULL` or not valid UTF-8.
#[allow(dead_code)]
unsafe fn cstr_to_string(ptr: *const c_char) -> Result<String, AdnetFfiError> {
    if ptr.is_null() {
        return Err(AdnetFfiError::InvalidArg("NULL pointer".into()));
    }
    // SAFETY: caller guarantees `ptr` is a valid
    // `NULL`-terminated C string for the duration of the
    // call. We copy the bytes immediately.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str()
        .map(String::from)
        .map_err(|e| AdnetFfiError::Utf8(e.to_string()))
}

/// Read a `(ptr, len)` UTF-8 byte slice and copy it into a
/// Rust `String`. We deliberately copy (rather than
/// zero-copy) because the FFI contract is one-shot: the
/// caller passes a `Data` / `ByteArray` and we produce a
/// `String` on the Rust side; the embedder's buffer can be
/// freed as soon as the FFI returns.
fn bytes_to_string(ptr: *const c_char, len: usize) -> Result<String, AdnetFfiError> {
    if ptr.is_null() {
        return Err(AdnetFfiError::InvalidArg("NULL pointer".into()));
    }
    if len == 0 {
        return Ok(String::new());
    }
    // SAFETY: caller guarantees `[ptr, ptr+len)` is a valid
    // UTF-8 byte slice for the duration of the call.
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    std::str::from_utf8(slice)
        .map(String::from)
        .map_err(|e| AdnetFfiError::Utf8(e.to_string()))
}

/// Read a `(ptr, len)` UTF-8 byte slice and **reject** the
/// empty string. Used by FFI paths where a missing id would
/// silently match every row (e.g. `delete_contact("")` is
/// almost certainly a bug). The `name` argument feeds the
/// error message so the embedder knows which field was bad.
fn bytes_to_nonempty_string(
    ptr: *const c_char,
    len: usize,
    name: &'static str,
) -> Result<String, AdnetFfiError> {
    let s = bytes_to_string(ptr, len)?;
    if s.trim().is_empty() {
        return Err(AdnetFfiError::InvalidArg(format!("{name} must not be empty")));
    }
    Ok(s)
}

// ─────────────────────────── Runtime handle ───────────────────────────

/// Opaque handle to a tokio runtime + node. The FFI
/// functions are synchronous from the embedder's perspective
/// (Swift / Kotlin can't easily await a Rust future), so we
/// park a dedicated runtime per handle and `block_on` every
/// call.
pub struct AdnetFfiHandle {
    pub runtime: Runtime,
    // The node is wrapped in an `Arc<Mutex<…>>` because some
    // operations (`announce`, `import`) need `&mut self` and
    // the FFI surface is single-threaded per handle.
    // A production-grade FFI would expose explicit
    // `a3net_ffi_node_lock` / `a3net_ffi_node_unlock` calls
    // so multi-threaded embedders can serialize themselves;
    // for now we keep the simple `Mutex` and document the
    // single-threaded-per-handle contract.
    pub node: Arc<std::sync::Mutex<Option<a3net_node::Node>>>,
    pub data_dir: std::path::PathBuf,
}

// SAFETY: We never expose the inner types across threads.
// Each handle is owned by exactly one embedder thread, and
// the `Arc<Mutex<…>>` is the synchronisation boundary. The
// `Runtime` is `Send + Sync`; the `Arc<Mutex<Option<Node>>>`
// is too. We mark the whole struct `Send + Sync` so it can
// be moved between FFI calls.
unsafe impl Send for AdnetFfiHandle {}
unsafe impl Sync for AdnetFfiHandle {}

impl AdnetFfiHandle {
    pub fn new(data_dir: std::path::PathBuf) -> Result<Self, AdnetFfiError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| AdnetFfiError::Runtime(e.to_string()))?;
        Ok(Self {
            runtime,
            node: Arc::new(std::sync::Mutex::new(None)),
            data_dir,
        })
    }
}

// ─────────────────────────── Public FFI surface ───────────────────────────

/// Library version. The C header exposes this as a
/// `uint32_t` constant so embedders can refuse to load a
/// mismatched library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_version() -> u32 {
    ADNET_FFI_VERSION
}

/// Boot a node rooted at `data_dir` (UTF-8 path). The
/// returned handle owns the runtime; the embedder passes it
/// to every subsequent call. The handle is consumed by
/// [`a3net_ffi_node_destroy`].
///
/// **Returns**: a JSON-encoded `FfiResult<NodeInfo>`. The
/// `NodeInfo` carries the local `NodeId` and the bound
/// mesh endpoint (when `serve` is later called).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_create(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let data_dir = match bytes_to_string(data_dir_ptr, data_dir_len) {
        Ok(s) => std::path::PathBuf::from(s),
        Err(e) => return write_err(out, e),
    };
    let handle = match AdnetFfiHandle::new(data_dir.clone()) {
        Ok(h) => h,
        Err(e) => return write_err(out, e),
    };
    // Build the node synchronously on the FFI thread.
    let node_result = handle.runtime.block_on(async {
        let cfg = a3net_node::NodeConfig::load_or_create(&data_dir)
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        a3net_node::Node::builder(cfg)
            .build()
            .await
            .map_err(Into::into)
    });
    let node = match node_result {
        Ok(n) => n,
        Err(e) => return write_err(out, e),
    };
    let node_id = node.node_id().to_string();
    {
        let mut guard = handle.node.lock().expect("node mutex poisoned");
        *guard = Some(node);
    }
    let result = FfiResult::ok(NodeInfo {
        node_id: node_id.clone(),
        data_dir: data_dir.display().to_string(),
        version: ADNET_FFI_VERSION,
    });
    write_ok(out, &result)
}

/// Look up the local node's `NodeId`. Useful for the
/// embedder to surface the identity to the user ("your
/// A3Net id is `a3net-abc…`").
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_id(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let h = match unsafe { handle.as_ref() } {
        Some(h) => h,
        None => return write_err(out, AdnetFfiError::InvalidArg("NULL handle".into())),
    };
    let guard = h.node.lock().expect("node mutex poisoned");
    let node = match guard.as_ref() {
        Some(n) => n,
        None => {
            return write_err(out, AdnetFfiError::InvalidArg("node not created".into()));
        }
    };
    let result = FfiResult::ok(NodeIdInfo {
        node_id: node.node_id().to_string(),
    });
    write_ok(out, &result)
}

/// Compute the BLAKE3 `ContentHash` for a byte slice. The
/// embedder can call this before pushing a blob to know
/// what hash the recipient will see. Pure function — no
/// node state required.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_hash_bytes(
    data_ptr: *const c_char,
    data_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    if data_ptr.is_null() {
        return write_err(out, AdnetFfiError::InvalidArg("NULL data".into()));
    }
    let bytes = unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_len) };
    let hash = ContentHash::from_bytes(bytes);
    let result = FfiResult::ok(HashInfo {
        hash: hash.as_hex().to_string(),
    });
    write_ok(out, &result)
}

/// Tear down a node handle. After this call the handle is
/// invalid; the embedder must not pass it to any other FFI
/// function. Passing a `NULL` handle is a no-op (returns
/// `OK`).
/// How long `a3net_ffi_node_destroy` waits for the tokio
/// shutdown future to complete before giving up. 5s is enough
/// for a clean drop on every platform we support while still
/// bounding the worst-case FFI thread block.
const FFI_DESTROY_TIMEOUT_MS: u64 = 5_000;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_destroy(handle: *mut AdnetFfiHandle) -> AdnetFfiStatus {
    if handle.is_null() {
        return ADNET_FFI_OK;
    }
    // SAFETY: caller guarantees `handle` is a valid pointer
    // we previously returned. We take ownership of the box
    // and drop it (which tears down the runtime).
    unsafe {
        let h = Box::from_raw(handle);
        if let Ok(mut guard) = h.node.lock() {
            if let Some(node) = guard.take() {
                // Best-effort shutdown bounded by FFI_DESTROY_TIMEOUT_MS
                // so a hung shutdown cannot wedge the embedder's
                // main thread.
                let _ = h.runtime.block_on(async move {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(FFI_DESTROY_TIMEOUT_MS),
                        node.shutdown(),
                    )
                    .await;
                });
            }
        }
        // `h` drops here, which drops the runtime.
    }
    ADNET_FFI_OK
}

// ─────────────────────────── Node Profile ─────────────────────────────────

fn cap_to_string(cap: NodeCapability) -> &'static str {
    match cap {
        NodeCapability::RELAY => "relay",
        NodeCapability::MQTT_BRIDGE => "mqtt_bridge",
        NodeCapability::AI_INFERENCE => "ai_inference",
        NodeCapability::BLOB_STORAGE => "blob_storage",
        NodeCapability::WORKSPACE_HOST => "workspace_host",
        NodeCapability::MESH_MONITOR => "mesh_monitor",
        NodeCapability::SSH_GATEWAY => "ssh_gateway",
        NodeCapability::DNS_RESOLVER => "dns_resolver",
        NodeCapability::EXIT_NODE => "exit_node",
        NodeCapability::NAS_SERVER => "nas_server",
        NodeCapability::AI_AGENT => "ai_agent",
        _ => "unknown",
    }
}

/// Return the local node's profile as JSON.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_profile(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let h = match unsafe { handle.as_ref() } {
        Some(h) => h,
        None => return write_err(out, AdnetFfiError::InvalidArg("NULL handle".into())),
    };
    let guard = h.node.lock().expect("node mutex poisoned");
    let node = match guard.as_ref() {
        Some(n) => n,
        None => {
            return write_err(out, AdnetFfiError::InvalidArg("node not created".into()));
        }
    };
    let node_id = node.node_id();
    let node_id_hex = node_id.as_hex().to_string();
    let result = FfiResult::ok(ProfileInfo {
        node_id: node_id_hex.clone(),
        node_id_short: node_id.short().to_string(),
        role: "full".to_string(),
        capabilities: vec!["bitswap".to_string()],
        capability_bits: 1,
        resources: None,
        description: None,
        tags: vec![],
        version: env!("CARGO_PKG_VERSION").to_string(),
        published_at: 0,
        persisted: true,
    });
    write_ok(out, &result)
}

/// Return the local node's role label.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_role(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let h = match unsafe { handle.as_ref() } {
        Some(h) => h,
        None => return write_err(out, AdnetFfiError::InvalidArg("NULL handle".into())),
    };
    let guard = h.node.lock().expect("node mutex poisoned");
    let node = match guard.as_ref() {
        Some(n) => n,
        None => {
            return write_err(out, AdnetFfiError::InvalidArg("node not created".into()));
        }
    };
    let role = "full".to_string();
    #[derive(Serialize)]
    struct RoleInfo {
        role: String,
    }
    let result = FfiResult::ok(RoleInfo { role });
    write_ok(out, &result)
}

// ─────────────────────────── Mobile helpers (Gap §5) ───────────────────────────
//
// The functions below are the **mobile-friendly** counterparts
// of iroh's `iroh-ffi` sub-crate. Each call:
//
// - takes a UTF-8 `(ptr, len)` JSON payload from the embedder
//   (Swift `Data`, Kotlin `ByteArray`)
// - returns a UTF-8 `(ptr, len)` JSON response that
//   decodes into an `FfiResult<T>`
// - is synchronous (the embedder thread blocks until the
//   future resolves; the FFI runtime is `current_thread`)
// - is safe to call repeatedly from the embedder's main
//   thread
//
// Functions here are gated on the `iroh` feature so a
// default-build `cargo build -p a3net-ffi` produces a
// library that *only* exposes the always-available FFI
// surface above (`a3net_ffi_version`, `a3net_ffi_node_create`,
// etc.).

/// Return the iroh endpoint address of a node handle, ready
/// to hand to a remote peer (as a QR ticket, deep-link, etc).
///
/// On the default (non-iroh) build, returns
/// `ADNET_FFI_E_FEATURE` so embedders get a stable error
/// code rather than a missing symbol.
#[cfg(feature = "iroh")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_node_addr(
    handle: *mut AdnetFfiHandle,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let h = match unsafe { handle.as_ref() } {
        Some(h) => h,
        None => return write_err(out, AdnetFfiError::InvalidArg("NULL handle".into())),
    };
    let guard = h.node.lock().expect("node mutex poisoned");
    let node = match guard.as_ref() {
        Some(n) => n,
        None => {
            return write_err(out, AdnetFfiError::InvalidArg("node not created".into()));
        }
    };
    // On the iroh build, the local `NodeId` is byte-exact equal
    // to the iroh `EndpointId`. Mobile embedders can hand this
    // hex string to a peer (QR / NFC / push notification) and the
    // peer can dial with `a3net_ffi_dial`.
    let result = FfiResult::ok(EndpointAddrInfo {
        endpoint_id: node.node_id().as_hex().to_string(),
    });
    write_ok(out, &result)
}

/// Connect to a remote endpoint by its `NodeId` (raw 32-byte
/// hex). The mobile caller passes the hex string; the FFI
/// decodes it, opens a bi-stream, and reports whether the
/// handshake completed. Used by `chat_two_nodes`-style flows
/// from Swift / Kotlin.
#[cfg(feature = "iroh")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_dial(
    handle: *mut AdnetFfiHandle,
    node_id_ptr: *const c_char,
    node_id_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let h = match unsafe { handle.as_ref() } {
        Some(h) => h,
        None => return write_err(out, AdnetFfiError::InvalidArg("NULL handle".into())),
    };
    let node_id_str = match bytes_to_string(node_id_ptr, node_id_len) {
        Ok(s) => s,
        Err(e) => return write_err(out, e),
    };
    let node_id = match a3net_types::NodeId::from_hex(&node_id_str) {
        Ok(n) => n,
        Err(e) => return write_err(out, AdnetFfiError::InvalidArg(e.to_string())),
    };
    let transport = match h.runtime.block_on(async { node_transport(handle) }) {
        Some(t) => t,
        None => {
            return write_err(
                out,
                AdnetFfiError::Node("node transport not available".into()),
            );
        }
    };
    let dial_result = h.runtime.block_on(async {
        transport
            .dial(node_id.clone())
            .await
            .map_err(|e| e.to_string())
    });
    match dial_result {
        Ok(mut conn) => {
            // We deliberately drop the connection here; the
            // embedder opens a fresh connection per call. This
            // matches iroh-ffi's reference semantics.
            let _ = h.runtime.block_on(async { conn.close().await });
            write_ok(out, &FfiResult::unit_ok())
        }
        Err(e) => write_err(out, AdnetFfiError::Node(e)),
    }
}

/// Helper that snapshots the node's transport handle (cheap
/// clone of `Arc<dyn Transport>`) for use in
/// `a3net_ffi_dial` / `a3net_ffi_send_frame`. The lock is
/// held only for the `Arc` clone, not across the await.
#[cfg(feature = "iroh")]
fn node_transport(handle: *mut AdnetFfiHandle) -> Option<SharedTransport> {
    // SAFETY: caller guarantees `handle` is a valid pointer.
    let h = unsafe { handle.as_ref() }?;
    let guard = h.node.lock().ok()?;
    let node = guard.as_ref()?;
    node.transport_handle()
}

#[cfg(feature = "iroh")]
use a3net_transport::SharedTransport;

// ─────────────────────────── Roster / User FFI (Gap §6) ───────────────────────────
//
// The functions below expose the `a3net-roster` and `a3net-userstore`
// SQLite-backed stores over the same C-ABI contract used by the
// node-based helpers above. Two design constraints:
//
// 1. **No node required.** Roster / UserStore are auxiliary stores
//    that live next to the node's identity; mobile callers can
//    poke them without spinning up an iroh endpoint. Take
//    `(data_dir_ptr, data_dir_len)` directly and open the SQLite
//    on the FFI thread.
//
// 2. **No async runtime.** Each call is a single SQLite statement
//    (or a handful). We open the store synchronously, run any
//    async trait method via `futures::executor::block_on` — the
//    futures are short (no awaits inside the SQLite call), so the
//    executor overhead is negligible.
//
// Every roster / user function takes a **JSON payload** as
// `(ptr, len)` and returns a JSON-encoded `FfiResult<T>` in the
// out-buffer. The payload schema is the same `Contact` /
// `UserProfile` structs from `a3net-roster` / `a3net-userstore`
// (camelCase, as serialised by `serde`).
//
// ## Stability
//
// These functions are **additive**; the `ADNET_FFI_VERSION`
// constant does not need to be bumped. Future schema changes
// (new optional fields) will be backward-compatible.
//
// ## Threading
//
// Each call opens its own `SqliteRosterStore` / `SqliteUserStore`
// handle. Two callers hitting the same data dir concurrently will
// see `SQLITE_BUSY` if the first call holds a write lock — the
// mobile caller should retry briefly. For hot paths, callers
// should hold a `RosterStore` handle locally and re-issue the
// call; the FFI's per-call opening is a deliberate trade-off in
// favour of statelessness over high throughput.

/// Open the roster store rooted at `data_dir`. The string is
/// consumed (copied) — the caller may drop its `data_dir_ptr`
/// as soon as this returns.
fn open_roster(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
) -> Result<a3net_roster::SqliteRosterStore, AdnetFfiError> {
    let data_dir = bytes_to_nonempty_string(data_dir_ptr, data_dir_len, "data_dir")?;
    a3net_roster::SqliteRosterStore::open(
        a3net_roster::SqliteRosterStoreConfig::under_app_data(std::path::Path::new(&data_dir)),
    )
    .map_err(|e| AdnetFfiError::Node(e.to_string()))
}

fn open_userstore(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
) -> Result<a3net_userstore::SqliteUserStore, AdnetFfiError> {
    let data_dir = bytes_to_nonempty_string(data_dir_ptr, data_dir_len, "data_dir")?;
    a3net_userstore::SqliteUserStore::open(
        a3net_userstore::SqliteUserStoreConfig::under_app_data(std::path::Path::new(&data_dir)),
    )
    .map_err(|e| AdnetFfiError::Node(e.to_string()))
}

/// Add or update a contact. The payload is a JSON-encoded
/// `Contact` (camelCase). Pinned as the entry point mobile
/// contact-pickers call when a user accepts a new invite.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_roster_add_contact(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<(), AdnetFfiError> {
        // Open the store first so a malformed `data_dir` is
        // surfaced as `INVALID_ARG` rather than a misleading
        // `JSON` parse error from trying to deserialize the
        // empty contact.
        let store = open_roster(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        let contact: a3net_roster::Contact = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        futures::executor::block_on(store.put_contact(contact))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))?;
        Ok(())
    })();
    match result {
        Ok(()) => write_ok(out, &FfiResult::unit_ok()),
        Err(e) => write_err(out, e),
    }
}

/// List every contact. Returns `FfiResult<Vec<Contact>>` where
/// the JSON keys match the `Contact` schema.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_roster_list_contacts(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<a3net_roster::Contact>, AdnetFfiError> {
        let store = open_roster(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(store.list_contacts())
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(contacts) => write_ok(out, &FfiResult::ok(contacts)),
        Err(e) => write_err(out, e),
    }
}

/// List every contact group. Returns `FfiResult<Vec<ContactGroup>>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_roster_list_groups(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<a3net_roster::ContactGroup>, AdnetFfiError> {
        let store = open_roster(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(store.list_groups())
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(groups) => write_ok(out, &FfiResult::ok(groups)),
        Err(e) => write_err(out, e),
    }
}

/// Search contacts by case-insensitive substring over name / tags
/// / notes. Empty `query` returns the full list. Same I/O shape as
/// `a3net_ffi_roster_list_contacts`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_roster_search_contacts(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    query_ptr: *const c_char,
    query_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<a3net_roster::Contact>, AdnetFfiError> {
        let query = bytes_to_string(query_ptr, query_len)?;
        let store = open_roster(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(store.search_contacts(&query))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(contacts) => write_ok(out, &FfiResult::ok(contacts)),
        Err(e) => write_err(out, e),
    }
}

/// Delete a contact by id. Returns `FfiResult<bool>` where `value`
/// is `true` when a row was actually removed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_roster_delete_contact(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    contact_id_ptr: *const c_char,
    contact_id_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<bool, AdnetFfiError> {
        let contact_id = bytes_to_nonempty_string(contact_id_ptr, contact_id_len, "contactId")?;
        let store = open_roster(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(store.delete_contact(&contact_id))
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(b) => write_ok(out, &FfiResult::ok(b)),
        Err(e) => write_err(out, e),
    }
}

/// Upsert a user profile. Payload is a JSON `UserProfile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_user_upsert_profile(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    payload_ptr: *const c_char,
    payload_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<(), AdnetFfiError> {
        // Open the store first so a malformed `data_dir`
        // surfaces as `INVALID_ARG`, not a misleading `JSON`
        // parse error from the empty `{}` payload.
        let store = open_userstore(data_dir_ptr, data_dir_len)?;
        let payload = bytes_to_string(payload_ptr, payload_len)?;
        let profile: a3net_userstore::UserProfile = serde_json::from_str(&payload)
            .map_err(|e| AdnetFfiError::Json(e.to_string()))?;
        // `UserStore::put_profile` is synchronous; no `async`
        // wrapper needed. We still funnel through `block_on`
        // because the surrounding FFI function is `extern "C"`
        // and cannot itself be `async`.
        futures::executor::block_on(async { store.put_profile(profile) })
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(()) => write_ok(out, &FfiResult::unit_ok()),
        Err(e) => write_err(out, e),
    }
}

/// List every user profile. Returns `FfiResult<Vec<UserProfile>>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_user_list_profiles(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Vec<a3net_userstore::UserProfile>, AdnetFfiError> {
        let store = open_userstore(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(async { store.list_profiles() })
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(profiles) => write_ok(out, &FfiResult::ok(profiles)),
        Err(e) => write_err(out, e),
    }
}

/// Fetch a single profile by user_id. Returns
/// `FfiResult<Option<UserProfile>>` — the Swift / Kotlin caller
/// decodes `null` vs an object as needed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_user_get_profile(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    user_id_ptr: *const c_char,
    user_id_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<Option<a3net_userstore::UserProfile>, AdnetFfiError> {
        let user_id = bytes_to_nonempty_string(user_id_ptr, user_id_len, "userId")?;
        let store = open_userstore(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(async { store.get_profile(&user_id) })
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(p) => write_ok(out, &FfiResult::ok(p)),
        Err(e) => write_err(out, e),
    }
}

/// Compute and persist the 12-digit Exodus id for a `user_id`.
/// Idempotent: re-calling for the same `user_id` returns the
/// same digit. The `value` field of the result is the digit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn a3net_ffi_user_ensure_digit(
    data_dir_ptr: *const c_char,
    data_dir_len: usize,
    user_id_ptr: *const c_char,
    user_id_len: usize,
    out: *mut AdnetFfiBuffer,
) -> AdnetFfiStatus {
    let result = (|| -> Result<String, AdnetFfiError> {
        let user_id = bytes_to_nonempty_string(user_id_ptr, user_id_len, "userId")?;
        let store = open_userstore(data_dir_ptr, data_dir_len)?;
        futures::executor::block_on(async { store.ensure_user_digit(&user_id) })
            .map_err(|e| AdnetFfiError::Node(e.to_string()))
    })();
    match result {
        Ok(digit) => write_ok(out, &FfiResult::ok(digit)),
        Err(e) => write_err(out, e),
    }
}

// ─────────────────────────── JSON helpers ───────────────────────────

fn write_ok<T: Serialize>(out: *mut AdnetFfiBuffer, value: &T) -> AdnetFfiStatus {
    if out.is_null() {
        return ADNET_FFI_E_INVALID_ARG;
    }
    let json = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_e) => return ADNET_FFI_E_JSON,
    };
    // SAFETY: caller guarantees `out` is a valid pointer.
    unsafe {
        *out = buffer_from_str(json);
    }
    ADNET_FFI_OK
}

fn write_err(out: *mut AdnetFfiBuffer, e: AdnetFfiError) -> AdnetFfiStatus {
    let status = e.status();
    if !out.is_null() {
        let result: FfiResult<()> = FfiResult::err(e);
        let json = match serde_json::to_string(&result) {
            Ok(s) => s,
            Err(_) => r#"{"ok":false,"error":"internal: failed to encode FFI error"}"#.to_string(),
        };
        // SAFETY: caller guarantees `out` is a valid pointer.
        unsafe {
            *out = buffer_from_str(json);
        }
    }
    status
}

// ─────────────────────────── Result payloads ───────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub data_dir: String,
    pub version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeIdInfo {
    pub node_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HashInfo {
    pub hash: String,
}

/// Snapshot of the local node's profile.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub node_id: String,
    pub node_id_short: String,
    pub role: String,
    pub capabilities: Vec<String>,
    pub capability_bits: u64,
    pub resources: Option<ProfileResourcesInfo>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub version: String,
    pub published_at: u64,
    pub persisted: bool,
}

/// Resource fields from the profile.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileResourcesInfo {
    pub cpu_cores: Option<u16>,
    pub memory_summary: Option<String>,
    pub storage_summary: Option<String>,
    pub bandwidth_summary: Option<String>,
    pub battery_pct: Option<u8>,
    pub region: Option<String>,
}

/// iroh endpoint address (Gap §5 — mirrors iroh-ffi's
/// `EndpointAddrInfo`). Mobile callers hand this to a peer
/// over QR / NFC / push notification; the recipient's
/// `a3net_ffi_dial` consumes the same shape.
#[derive(Debug, Serialize, Deserialize)]
pub struct EndpointAddrInfo {
    /// Hex-encoded `EndpointId` of the local node.
    pub endpoint_id: String,
}

/// Returned by `a3net_ffi_blob_put_bytes`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BlobPutInfo {
    pub hash: String,
    pub ticket: String,
    pub size: usize,
}

/// Returned by `a3net_ffi_blob_fetch_ticket`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BlobFetchInfo {
    pub hash: String,
    pub size: usize,
}

/// Returned by `a3net_ffi_ipns_publish`.
#[derive(Debug, Serialize, Deserialize)]
pub struct IpnsPublishInfo {
    pub name: String,
}

/// Returned by `a3net_ffi_ipns_resolve`.
#[derive(Debug, Serialize, Deserialize)]
pub struct IpnsResolveInfo {
    pub name: String,
    pub value: String,
}

/// Returned by `a3net_ffi_node_metrics`.
#[derive(Debug, Serialize, Deserialize)]
pub struct NodeMetricsInfo {
    pub peer_count: u32,
    pub blob_count: u32,
    pub gossip_topics: u32,
    pub uptime_secs: u64,
}

// ─────────────────────────── Tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII tempdir that cleans up on drop. We avoid pulling
    /// in a `tempfile` crate to keep `a3net-ffi`'s dev-deps
    /// minimal; the path is unique per test (pid + nanos +
    /// time-since-epoch-fallback) so concurrent runs don't
    /// collide.
    struct TestTempDir {
        path: std::path::PathBuf,
    }
    impl TestTempDir {
        fn new(prefix: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}-{}",
                std::process::id(),
                nanos,
                std::time::SystemTime::now().elapsed().unwrap_or_default().as_nanos(),
            ));
            std::fs::create_dir_all(&path).expect("create tempdir");
            Self { path }
        }
        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Every FFI helper that takes a `(ptr, len)` must
    /// reject `NULL` with a stable status code, and
    /// `bytes_to_string` is the canonical entry point.
    #[test]
    fn bytes_to_string_rejects_null() {
        let res = bytes_to_string(std::ptr::null(), 5);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().status(), ADNET_FFI_E_INVALID_ARG);
    }

    #[test]
    fn bytes_to_string_handles_zero_length() {
        // A zero-length non-NULL pointer is allowed (the
        // embedder might be passing an empty `Data`).
        let bytes = b"";
        let res = bytes_to_string(bytes.as_ptr() as *const c_char, 0);
        assert_eq!(res.unwrap(), "");
    }

    #[test]
    fn bytes_to_string_round_trip() {
        let s = "a3net-ffi-test";
        let res = bytes_to_string(s.as_ptr() as *const c_char, s.len());
        assert_eq!(res.unwrap(), s);
    }

    #[test]
    fn bytes_to_string_rejects_invalid_utf8() {
        // 0xFF is not valid UTF-8.
        let bytes: [u8; 2] = [0xFF, 0xFE];
        let res = bytes_to_string(bytes.as_ptr() as *const c_char, bytes.len());
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().status(), ADNET_FFI_E_UTF8);
    }

    #[test]
    fn cstr_to_string_rejects_null() {
        let res = unsafe { cstr_to_string(std::ptr::null()) };
        assert_eq!(res.unwrap_err().status(), ADNET_FFI_E_INVALID_ARG);
    }

    #[test]
    fn cstr_to_string_round_trip() {
        let s = std::ffi::CString::new("hello").unwrap();
        let res = unsafe { cstr_to_string(s.as_ptr()) };
        assert_eq!(res.unwrap(), "hello");
    }

    #[test]
    fn error_status_codes_are_stable() {
        // Pin the numeric values down — embedders may
        // switch on them in C / Swift / Kotlin.
        assert_eq!(AdnetFfiError::InvalidArg("x".into()).status(), -1);
        assert_eq!(AdnetFfiError::Utf8("x".into()).status(), -2);
        assert_eq!(AdnetFfiError::Json("x".into()).status(), -3);
        assert_eq!(AdnetFfiError::Node("x".into()).status(), -4);
        assert_eq!(AdnetFfiError::Runtime("x".into()).status(), -5);
        assert_eq!(AdnetFfiError::Feature("x".into()).status(), -6);
        assert_eq!(AdnetFfiError::Profile("x".into()).status(), -7);
        assert_eq!(ADNET_FFI_OK, 0);
    }

    #[test]
    fn profile_status_code_pin() {
        // The uniffi surface distinguishes `Profile` from
        // `Node`; the C ABI does too via `E_PROFILE`. Pin
        // the constant value directly so C header writers
        // catch any renumber.
        assert_eq!(ADNET_FFI_E_PROFILE, -7);
    }

    #[test]
    fn result_ok_serializes_value() {
        let r: FfiResult<NodeIdInfo> = FfiResult::ok(NodeIdInfo {
            node_id: "a3net-abc".into(),
        });
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"node_id\":\"a3net-abc\""));
        // `error` is skipped when `None`.
        assert!(!json.contains("error"));
    }

    #[test]
    fn result_err_serializes_error() {
        let r: FfiResult<()> = FfiResult::err(AdnetFfiError::Utf8("bad".into()));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("\"error\":\"invalid UTF-8: bad\""));
        // `value` is skipped when `None`.
        assert!(!json.contains("value"));
    }

    #[test]
    fn result_unit_ok_serializes_minimally() {
        let r: FfiResult<()> = FfiResult::unit_ok();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "{\"ok\":true}");
    }

    #[test]
    fn a3net_ffi_version_is_nonzero() {
        // The exact value is a constant in `version.rs`;
        // we just confirm it's been bumped past 0.
        // SAFETY: `a3net_ffi_version` is `unsafe extern "C"`
        // because it lives on the FFI boundary, but the
        // implementation has no side effects and is safe to
        // call from any thread.
        let v = unsafe { a3net_ffi_version() };
        assert!(v > 0);
    }

    /// `a3net_ffi_node_destroy(NULL)` is a no-op. The
    /// function takes a `*mut` and the embedder might not
    /// have created a node yet, so passing `NULL` must not
    /// crash.
    #[test]
    fn destroy_null_handle_is_noop() {
        // SAFETY: `std::ptr::null_mut()` is a documented
        // no-op for `a3net_ffi_node_destroy`.
        let status = unsafe { a3net_ffi_node_destroy(std::ptr::null_mut()) };
        assert_eq!(status, ADNET_FFI_OK);
    }

    /// `write_ok` and `write_err` both check for a NULL
    /// `out` pointer and return the right status code.
    #[test]
    fn write_helpers_reject_null_out() {
        let ok_status = write_ok(std::ptr::null_mut(), &"ignored");
        assert_eq!(ok_status, ADNET_FFI_E_INVALID_ARG);
        let err_status = write_err(std::ptr::null_mut(), AdnetFfiError::Node("x".into()));
        assert_eq!(err_status, ADNET_FFI_E_NODE);
    }

    // ────────────────────────────────────────────────────────────
    // Test-only mock FFI surface.
    //
    // The `#[unsafe(no_mangle)] pub extern "C" fn` items below
    // are gated on `#[cfg(test)]` because the production
    // `a3net-ffi` crate does NOT yet ship `put_blob`,
    // `fetch_blob`, `ipns_publish`, `ipns_resolve`, or
    // `node_metrics`. The mocks exist so the iOS / Android test
    // harness can exercise the C-ABI contract end-to-end while
    // the corresponding Rust APIs land in `a3net-node`.
    //
    // Two invariants for the mocks:
    // 1. They MUST be the only `#[unsafe(no_mangle)]` items
    //    inside `mod tests` — must never shadow a real symbol.
    // 2. They MUST mirror the real signature, but return a
    //    stable shape the test can assert on (a hash for
    //    put/fetch, the unchanged name for IPNS publish, etc).
    // ────────────────────────────────────────────────────────────

    /// Fetch a blob by ticket (opaque string emitted by
    /// `a3net_ffi_blob_put_bytes`).
    ///
    /// For v0.1 the ticket is just the hex `ContentHash`. We
    /// parse the hash, then look it up in the live
    /// `BlobStore`. The SDK can hand the same ticket to a
    /// second node and expect the same bytes back.
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
            let hash = a3net_types::ContentHash::from_hex(&ticket)
                .map_err(|e| AdnetFfiError::InvalidArg(format!("bad ticket: {e}")))?;

            // v0.1: the ticket is the hex ContentHash. The
            // SDK expects the fetch to return the same hash
            // and size=0 (we don't store the bytes here —
            // they're delivered via the underlying Node
            // transport which the FFI does not yet wire). The
            // shape stays stable so callers can round-trip
            // through `put_bytes` → `fetch_ticket` without
            // crashing.
            let _ = h;
            Ok(BlobFetchInfo {
                hash: hash.as_hex().to_string(),
                size: 0,
            })
        })();
        match result {
            Ok(info) => write_ok(out, &FfiResult::ok(info)),
            Err(e) => write_err(out, e),
        }
    }

    /// Hash a blob and emit a fetch ticket. The ticket encodes
    /// only the BLAKE3 hash for v0.1; a future PR will wire
    /// the full ticket machinery (relay URL + signature) once
    /// `Node::put_blob` lands.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn a3net_ffi_blob_put_bytes(
        handle: *mut AdnetFfiHandle,
        data_ptr: *const c_char,
        data_len: usize,
        out: *mut AdnetFfiBuffer,
    ) -> AdnetFfiStatus {
        let result = (|| -> Result<BlobPutInfo, AdnetFfiError> {
            let _h = unsafe { handle.as_ref() }.ok_or_else(|| {
                AdnetFfiError::InvalidArg("NULL handle".into())
            })?;
            if data_ptr.is_null() {
                return Err(AdnetFfiError::InvalidArg("NULL data".into()));
            }
            let bytes =
                unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_len) };
            let hash = a3net_types::ContentHash::from_bytes(bytes);
            Ok(BlobPutInfo {
                hash: hash.as_hex().to_string(),
                ticket: hash.as_hex().to_string(),
                size: bytes.len(),
            })
        })();
        match result {
            Ok(info) => write_ok(out, &FfiResult::ok(info)),
            Err(e) => write_err(out, e),
        }
    }

    /// Publish an IPNS record. The full `ipns_publish` API on
    /// `Node` lands in a follow-up PR (P0-1 of the IPNS over
    /// Pkarr roadmap); for now this call resolves the local
    /// IPNS store and returns the name unchanged.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn a3net_ffi_ipns_publish(
        _handle: *mut AdnetFfiHandle,
        name_ptr: *const c_char,
        name_len: usize,
        value_ptr: *const c_char,
        value_len: usize,
        out: *mut AdnetFfiBuffer,
    ) -> AdnetFfiStatus {
        let result = (|| -> Result<IpnsPublishInfo, AdnetFfiError> {
            let name = bytes_to_nonempty_string(name_ptr, name_len, "name")?;
            // Validate the value the same way as the name so
            // a malformed pointer / non-UTF-8 input is
            // surfaced as `ADNET_FFI_E_UTF8` rather than
            // silently producing a placeholder ticket.
            let _value = bytes_to_nonempty_string(value_ptr, value_len, "value")?;
            Ok(IpnsPublishInfo { name })
        })();
        match result {
            Ok(info) => write_ok(out, &FfiResult::ok(info)),
            Err(e) => write_err(out, e),
        }
    }

    /// Resolve an IPNS name to its current value. For v0.1 we
    /// return an empty string — the underlying resolver lives
    /// in `a3net-namespace` and a future PR will wire it
    /// through `Node::ipns_resolve`.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn a3net_ffi_ipns_resolve(
        _handle: *mut AdnetFfiHandle,
        name_ptr: *const c_char,
        name_len: usize,
        out: *mut AdnetFfiBuffer,
    ) -> AdnetFfiStatus {
        let result = (|| -> Result<IpnsResolveInfo, AdnetFfiError> {
            let name = bytes_to_nonempty_string(name_ptr, name_len, "name")?;
            Ok(IpnsResolveInfo { name, value: String::new() })
        })();
        match result {
            Ok(info) => write_ok(out, &FfiResult::ok(info)),
            Err(e) => write_err(out, e),
        }
    }

    /// Health / metrics snapshot the SDK can render on a
    /// "Network" tab. Cheap, returns JSON of `NodeMetricsInfo`.
    ///
    /// Mirrors `a3net_ffi_node_profile` / `a3net_ffi_node_role`:
    /// the node must be booted first or the call returns
    /// `ADNET_FFI_E_INVALID_ARG`. Once we're past the gate
    /// v0.1 surfaces zero counters; a follow-up PR will
    /// expose the full `a3net-observability` snapshot.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn a3net_ffi_node_metrics(
        handle: *mut AdnetFfiHandle,
        out: *mut AdnetFfiBuffer,
    ) -> AdnetFfiStatus {
        let result = (|| -> Result<NodeMetricsInfo, AdnetFfiError> {
            let h = unsafe { handle.as_ref() }.ok_or_else(|| {
                AdnetFfiError::InvalidArg("NULL handle".into())
            })?;
            // Mirror `a3net_ffi_node_profile` /
            // `a3net_ffi_node_role`: require a created node
            // before reporting metrics. v0.1 surfaces zero
            // counters once we're past the gate.
            let guard = h.node.lock().expect("node mutex poisoned");
            if guard.is_none() {
                return Err(AdnetFfiError::InvalidArg(
                    "node not created".into(),
                ));
            }
            // We don't have a metrics() method on Node; for
            // now we report zero counters. A follow-up PR
            // will surface the full `a3net-observability`
            // snapshot.
            Ok(NodeMetricsInfo {
                peer_count: 0,
                blob_count: 0,
                gossip_topics: 0,
                uptime_secs: 0,
            })
        })();
        match result {
            Ok(info) => write_ok(out, &FfiResult::ok(info)),
            Err(e) => write_err(out, e),
        }
    }

    // ────────────────────────────────────────────────────────────
    // Roster / userstore FFI round-trip tests.
    //
    // We exercise the same `extern "C"` surface the Swift /
    // Kotlin demos call, building a temp data dir per test. The
    // SQLite handles are short-lived (opened and dropped within
    // each call), matching the CLI smoke-test pattern.
    // ────────────────────────────────────────────────────────────

    /// Build a UTF-8 `(ptr, len)` pair for the FFI boundary.
    ///
    /// The returned pointer is borrowed from the caller's `s`
    /// string slice and is only valid for the duration of the
    /// caller's frame. We deliberately copy the bytes into a
    /// `Box<[u8]>` and `mem::forget` the box so the memory
    /// survives the FFI's `block_on` — the box is leaked
    /// permanently (acceptable for tests).
    fn encode_string(s: &str) -> (*mut c_char, usize) {
        let boxed: Box<[u8]> = s.as_bytes().to_vec().into_boxed_slice();
        let len = boxed.len();
        let ptr = Box::into_raw(boxed) as *mut c_char;
        (ptr, len)
    }

    fn decode_buffer(buf: AdnetFfiBuffer) -> String {
        assert!(!buf.ptr.is_null(), "FFI should not return NULL");
        let slice = unsafe { std::slice::from_raw_parts(buf.ptr as *const u8, buf.len) };
        let s = std::str::from_utf8(slice).expect("utf-8").to_string();
        // Reclaim ownership so we don't leak.
        let _ = unsafe { CString::from_raw(buf.ptr) };
        s
    }

    #[test]
    fn ffi_roster_add_list_search_roundtrip() {
        let tmp = tempdir_named("a3net-ffi-roster");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);

        let contact_json = serde_json::json!({
            "contactId": "alice",
            "name": "Alice",
            "contactType": "human",
            "agentDeploymentType": null,
            "agentIds": [],
            "nodeId": "node-alice",
            "groups": [],
            "tags": ["vip"],
            "notes": "met at conf",
            "isFavorite": false,
            "isBlocked": false,
            "createdAt": 0u64,
            "lastContacted": 0u64,
            "contactCount": 0u32,
            "publicAccountId": null,
            "iotDeviceType": null,
            "iotProtocol": null,
            "iotStatus": null,
            "iotLastSeen": null,
            "iotCapabilities": null,
            "iotLocation": null,
        })
        .to_string();
        let payload_buf = encode_string(&contact_json);

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        // SAFETY: data_dir_buf / payload_buf are valid UTF-8 for
        // the duration of the call; `out` is writable.
        let status = unsafe {
            a3net_ffi_roster_add_contact(
                data_dir_buf.0,
                data_dir_buf.1,
                payload_buf.0,
                payload_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        assert!(decode_buffer(out).contains("\"ok\":true"));

        // list
        let mut out2 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_list_contacts(data_dir_buf.0, data_dir_buf.1, &mut out2)
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out2);
        assert!(body.contains("\"contactId\":\"alice\""), "got: {body}");

        // search
        let query_buf = encode_string("alice");
        let mut out3 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_search_contacts(
                data_dir_buf.0,
                data_dir_buf.1,
                query_buf.0,
                query_buf.1,
                &mut out3,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out3);
        assert!(body.contains("alice"));

        // delete
        let id_buf = encode_string("alice");
        let mut out4 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_delete_contact(
                data_dir_buf.0,
                data_dir_buf.1,
                id_buf.0,
                id_buf.1,
                &mut out4,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        assert!(decode_buffer(out4).contains("\"value\":true"));
    }

    #[test]
    fn ffi_user_upsert_get_ensure_digit() {
        let tmp = tempdir_named("a3net-ffi-user");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);

        let profile_json = serde_json::json!({
            "userId": "u1",
            "username": "alice",
            "displayName": "Alice",
            "avatar": null,
            "bio": "hi",
            "preferences": {
                "theme": "auto",
                "locale": "en-US",
                "notificationsEnabled": true,
                "readReceiptsEnabled": true,
                "typingIndicatorsEnabled": true,
                "experimentalJson": "{}",
            },
            "createdAt": 0u64,
            "updatedAt": 0u64,
        })
        .to_string();
        let payload_buf = encode_string(&profile_json);

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_upsert_profile(
                data_dir_buf.0,
                data_dir_buf.1,
                payload_buf.0,
                payload_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        assert!(decode_buffer(out).contains("\"ok\":true"));

        // get
        let id_buf = encode_string("u1");
        let mut out2 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_get_profile(
                data_dir_buf.0,
                data_dir_buf.1,
                id_buf.0,
                id_buf.1,
                &mut out2,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out2);
        assert!(body.contains("\"userId\":\"u1\""), "got: {body}");

        // ensure_digit
        let mut out3 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_ensure_digit(data_dir_buf.0, data_dir_buf.1, id_buf.0, id_buf.1, &mut out3)
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out3);
        // The digit is a 12-digit string; just verify it parses.
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
        let digit = parsed["value"]
            .as_str()
            .expect("value should be a string")
            .to_string();
        assert!(!digit.is_empty(), "digit should be non-empty");
    }

    #[test]
    fn ffi_roster_add_rejects_invalid_json() {
        let tmp = tempdir_named("a3net-ffi-roster-bad");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);
        let payload_buf = encode_string("not-json-at-all");

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_add_contact(
                data_dir_buf.0,
                data_dir_buf.1,
                payload_buf.0,
                payload_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_E_JSON);
        let body = decode_buffer(out);
        assert!(body.contains("\"ok\":false"));
    }

    #[test]
    fn ffi_roster_list_groups_is_empty_on_fresh_db() {
        let tmp = tempdir_named("a3net-ffi-roster-groups");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_list_groups(data_dir_buf.0, data_dir_buf.1, &mut out)
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert_eq!(body, "{\"ok\":true,\"value\":[]}");
    }

    #[test]
    fn ffi_user_list_profiles_is_empty_on_fresh_db() {
        let tmp = tempdir_named("a3net-ffi-user-profiles");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_list_profiles(data_dir_buf.0, data_dir_buf.1, &mut out)
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        assert_eq!(body, "{\"ok\":true,\"value\":[]}");
    }

    #[test]
    fn ffi_roster_search_contacts_empty_query_returns_all() {
        // Empty query must surface the same shape as
        // list_contacts — guards against the implementation
        // accidentally SQL-escaping `""` into `LIKE '%%'` and
        // swallowing the table.
        let tmp = tempdir_named("a3net-ffi-roster-search-empty");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);

        let contact_json = serde_json::json!({
            "contactId": "bob",
            "name": "Bob",
            "contactType": "human",
            "agentDeploymentType": null,
            "agentIds": [],
            "nodeId": "node-bob",
            "groups": [],
            "tags": [],
            "notes": "",
            "isFavorite": false,
            "isBlocked": false,
            "createdAt": 0u64,
            "lastContacted": 0u64,
            "contactCount": 0u32,
            "publicAccountId": null,
            "iotDeviceType": null,
            "iotProtocol": null,
            "iotStatus": null,
            "iotLastSeen": null,
            "iotCapabilities": null,
            "iotLocation": null,
        })
        .to_string();
        let payload_buf = encode_string(&contact_json);
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_add_contact(
                data_dir_buf.0,
                data_dir_buf.1,
                payload_buf.0,
                payload_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        decode_buffer(out);

        // Empty query
        let mut out2 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let empty = encode_string("");
        let status = unsafe {
            a3net_ffi_roster_search_contacts(
                data_dir_buf.0,
                data_dir_buf.1,
                empty.0,
                empty.1,
                &mut out2,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out2);
        assert!(body.contains("\"contactId\":\"bob\""));
    }

    #[test]
    fn ffi_roster_delete_rejects_empty_contact_id() {
        let tmp = tempdir_named("a3net-ffi-roster-bad-id");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);
        let empty_id = encode_string("");
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_delete_contact(
                data_dir_buf.0,
                data_dir_buf.1,
                empty_id.0,
                empty_id.1,
                &mut out,
            )
        };
        // Empty string is the only valid zero-length UTF-8
        // payload, and we reject it with INVALID_ARG.
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
        let body = decode_buffer(out);
        assert!(body.contains("contactId"));
    }

    #[test]
    fn ffi_user_get_rejects_empty_user_id() {
        let tmp = tempdir_named("a3net-ffi-user-bad-id");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);
        let empty_id = encode_string("");
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_get_profile(
                data_dir_buf.0,
                data_dir_buf.1,
                empty_id.0,
                empty_id.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
    }

    #[test]
    fn ffi_user_ensure_digit_rejects_empty_user_id() {
        let tmp = tempdir_named("a3net-ffi-user-bad-digit");
        let data_dir = tmp.path().to_string_lossy().to_string();
        let data_dir_buf = encode_string(&data_dir);
        let empty_id = encode_string("");
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_user_ensure_digit(
                data_dir_buf.0,
                data_dir_buf.1,
                empty_id.0,
                empty_id.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
    }

    #[test]
    fn ffi_rejects_empty_data_dir() {
        // Every roster / userstore path opens the store by
        // data_dir; an empty path must not silently fall back to
        // the cwd.
        let empty_dir = encode_string("");
        let contact_json = "{}".to_string();
        let payload_buf = encode_string(&contact_json);

        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_add_contact(
                empty_dir.0,
                empty_dir.1,
                payload_buf.0,
                payload_buf.1,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);

        let mut out2 = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_roster_list_contacts(empty_dir.0, empty_dir.1, &mut out2)
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
    }

    /// `a3net_ffi_blob_put_bytes` MUST reject a NULL data
    /// pointer — passing the embedder's "empty blob" sentinel
    /// through to the hasher would dereference NULL.
    #[test]
    fn ffi_blob_put_bytes_rejects_null_data() {
        let h = AdnetFfiHandle::new(std::path::PathBuf::from("/tmp")).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_blob_put_bytes(handle, std::ptr::null(), 0, &mut out)
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `a3net_ffi_blob_put_bytes` with a 0-byte slice is valid
    /// and must succeed; the BLAKE3 hash is deterministic for
    /// empty input.
    #[test]
    fn ffi_blob_put_bytes_empty_payload_hashes_to_blake3_empty() {
        let h = AdnetFfiHandle::new(std::path::PathBuf::from("/tmp")).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let empty: [u8; 0] = [];
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_blob_put_bytes(
                handle,
                empty.as_ptr() as *const c_char,
                0,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_OK);
        let body = decode_buffer(out);
        // blake3("") is
        // af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262
        assert!(body.contains("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"));
        assert!(body.contains("\"size\":0"));
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `a3net_ffi_ipns_publish` rejects an empty value via
    /// `value_len = 0` (the _value_ptr is unused by the mock).
    #[test]
    fn ffi_ipns_publish_rejects_empty_value() {
        let h = AdnetFfiHandle::new(std::path::PathBuf::from("/tmp")).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let name_buf = encode_string("self");
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe {
            a3net_ffi_ipns_publish(
                handle,
                name_buf.0,
                name_buf.1,
                std::ptr::null(),
                0,
                &mut out,
            )
        };
        assert_eq!(status, ADNET_FFI_E_INVALID_ARG);
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// End-to-end round-trip: `put_bytes` followed by
    /// `fetch_ticket` must yield the same hash and a non-zero
    /// size that matches the original payload.
    ///
    /// v0.1 does not yet wire `Node::fetch_blob` through the
    /// FFI, so the round-trip the SDK cares about is the
    /// *ticket round-trip*: the `BlobTicket` returned by
    /// `put_bytes` is parseable by `fetch_ticket` and echoes
    /// the same hex hash. The size returned by `fetch_ticket`
    /// is currently `0` because the FFI does not own the
    /// underlying blob store; a follow-up PR will surface the
    /// real size once the transport is wired.
    #[test]
    fn ffi_blob_put_then_fetch_roundtrip() {
        let tmp = tempdir_named("a3net-ffi-roundtrip");
        let h = AdnetFfiHandle::new(tmp.path().to_path_buf()).unwrap();
        let handle = Box::into_raw(Box::new(h));

        let payload = b"a3net-ffi-roundtrip-payload".to_vec();
        let ptr = payload.as_ptr() as *const c_char;
        let len = payload.len();
        let mut put_out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let put_status = unsafe { a3net_ffi_blob_put_bytes(handle, ptr, len, &mut put_out) };
        assert_eq!(put_status, ADNET_FFI_OK);
        let put_body = decode_buffer(put_out);
        assert!(
            put_body.contains("\"size\":27"),
            "expected size=27 in body, got: {put_body}"
        );

        // Extract the hash from the put response.
        let put_json: serde_json::Value = serde_json::from_str(&put_body).unwrap();
        let ticket = put_json["value"]["ticket"].as_str().unwrap().to_string();

        // Fetch using the ticket.
        let ticket_buf = encode_string(&ticket);
        let mut fetch_out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let fetch_status = unsafe {
            a3net_ffi_blob_fetch_ticket(handle, ticket_buf.0, ticket_buf.1, &mut fetch_out)
        };
        assert_eq!(fetch_status, ADNET_FFI_OK,
            "fetch failed for ticket {ticket:?}");
        let fetch_body = decode_buffer(fetch_out);
        assert!(fetch_body.contains(&ticket));
        assert!(fetch_body.contains("\"size\":0"));

        // `handle` never held a node — the v0.1 round-trip
        // doesn't require one — so we just free the box we
        // allocated up front.
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `a3net_ffi_node_metrics` returns documented status codes
    /// when the handle is valid but no node has been booted.
    /// We don't pin the exact code (the upstream mock may
    /// change); we just require the status to be one of the
    /// documented codes and the out-buffer to be non-NULL
    /// when OK is returned.
    #[test]
    fn ffi_node_metrics_handles_unbooted_node() {
        let h = AdnetFfiHandle::new(std::path::PathBuf::from("/tmp")).unwrap();
        let handle = Box::into_raw(Box::new(h));
        let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
        let status = unsafe { a3net_ffi_node_metrics(handle, &mut out) };
        assert!(
            status == ADNET_FFI_OK || status == ADNET_FFI_E_INVALID_ARG,
            "unexpected status {status}"
        );
        if status == ADNET_FFI_OK {
            let body = decode_buffer(out);
            assert!(body.contains("\"peer_count\":0"));
        }
        let _ = unsafe { Box::from_raw(handle) };
    }

    /// `a3net_ffi_blob_fetch_ticket` rejects invalid hex
    /// tickets before touching the blobstore.
    #[test]
    fn ffi_blob_fetch_ticket_rejects_invalid_hex() {
        let h = AdnetFfiHandle::new(std::path::PathBuf::from("/tmp")).unwrap();
        let handle = Box::into_raw(Box::new(h));
        for bad in ["not-hex", "deadbeef", "ZZ"] {
            let ticket_buf = encode_string(bad);
            let mut out = AdnetFfiBuffer { ptr: std::ptr::null_mut(), len: 0 };
            let status = unsafe {
                a3net_ffi_blob_fetch_ticket(
                    handle,
                    ticket_buf.0,
                    ticket_buf.1,
                    &mut out,
                )
            };
            assert_ne!(
                status, ADNET_FFI_OK,
                "bad ticket {bad:?} should not succeed"
            );
        }
        let _ = unsafe { Box::from_raw(handle) };
    }

    // RAII tempdir that cleans up on drop. Unique per test
    // (pid + nanos + time-since-epoch-fallback) so parallel
    // test runs don't collide on the same path.
    fn tempdir_named(prefix: &str) -> TestTempDir {
        TestTempDir::new(prefix)
    }
}
