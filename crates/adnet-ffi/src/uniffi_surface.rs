//! `adnet-ffi` uniffi surface.
//!
//! When the `uniffi` feature is enabled, this module adds a UDL
//! interface that uniffi consumes to produce Swift / Kotlin /
//! Python bindings. The Rust API exposed here is intentionally
//! richer than the C ABI: it uses uniffi's record / error
//! types directly, so a mobile caller gets typed errors instead
//! of having to inspect status codes.
//!
//! Building the bindings:
//!
//! ```bash
//! cargo run -p adnet-ffi --features uniffi -- uniffi generate \
//!     src/adnet.udl --language swift --out-dir bindings/swift
//! ```
//!
//! ## Why both the C ABI and uniffi?
//!
//! The C ABI is the **minimum** every embedder needs; we ship
//! it on every build because C is the universal FFI. The uniffi
//! surface is a **super-set** that adds:
//!
//!   * typed error enum (`AdnetError` → Swift `Error`, Kotlin `Throwable`)
//!   * automatic `Option<T>` ↔ Swift `Optional<T>` / Kotlin `T?`
//!   * automatic callbacks for subscribe-style APIs
//!     (`uniffi::Object` + callback interfaces)
//!
//! Operators that want a 50 KB Swift framework can use the uniffi
//! surface; operators that want to talk to us from a low-level C
//! engine can use the C ABI.

#![cfg(feature = "uniffi")]

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

/// Errors surfaced through the uniffi surface. Maps cleanly to
/// Swift `Error` / Kotlin `Throwable`. Variants mirror
/// `AdnetFfiError` but use structured fields so the generated
/// bindings can switch on them in language-native ways.
#[derive(Debug, Serialize, Deserialize, uniffi::Error)]
#[uniffi(flat_error)]
pub enum AdnetError {
    InvalidArgument { message: String },
    InvalidUtf8 { message: String },
    InvalidJson { message: String },
    Node { message: String },
    Runtime { message: String },
    Feature { message: String },
    Io { message: String },
    NotFound { message: String },
    /// Profile read or update failed. Mirrors the C-ABI
    /// `ADNET_FFI_E_PROFILE` code; the C ABI lumps Profile
    /// errors into `Node`, but the uniffi surface keeps
    /// them distinct so mobile callers can render a
    /// "re-login" hint versus a generic "node error".
    Profile { message: String },
}

impl std::fmt::Display for AdnetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::InvalidArgument { message } => format!("invalid argument: {message}"),
            Self::InvalidUtf8 { message } => format!("invalid UTF-8: {message}"),
            Self::InvalidJson { message } => format!("invalid JSON: {message}"),
            Self::Node { message } => format!("node error: {message}"),
            Self::Runtime { message } => format!("runtime error: {message}"),
            Self::Feature { message } => format!("feature not enabled: {message}"),
            Self::Io { message } => format!("i/o error: {message}"),
            Self::NotFound { message } => format!("not found: {message}"),
            Self::Profile { message } => format!("profile error: {message}"),
        };
        f.write_str(&s)
    }
}

impl std::error::Error for AdnetError {}

impl From<crate::AdnetFfiError> for AdnetError {
    fn from(e: crate::AdnetFfiError) -> Self {
        match e {
            crate::AdnetFfiError::InvalidArg(m) => Self::InvalidArgument { message: m },
            crate::AdnetFfiError::Utf8(m) => Self::InvalidUtf8 { message: m },
            crate::AdnetFfiError::Json(m) => Self::InvalidJson { message: m },
            crate::AdnetFfiError::Node(m) => Self::Node { message: m },
            crate::AdnetFfiError::Runtime(m) => Self::Runtime { message: m },
            crate::AdnetFfiError::Feature(m) => Self::Feature { message: m },
            crate::AdnetFfiError::Profile(m) => Self::Profile { message: m },
        }
    }
}

/// Snapshot of the local node's identity. Mirrors the
/// `NodeInfo` record in `adnet.udl` — keep these in sync
/// (uniffi-bindgen complains if the UDL and the Rust
/// definition drift). The fields are deliberately minimal
/// so the Swift / Kotlin caller can render an "About"
/// screen without pulling additional state.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct NodeInfo {
    pub node_id: String,
    pub uptime_secs: u64,
}

/// Returned by [`put_bytes`].
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct BlobPutInfo {
    pub hash: String,
    pub ticket: String,
    pub size: u64,
}

/// Returned by [`fetch_ticket`].
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct BlobFetchInfo {
    pub hash: String,
    pub size: u64,
}

/// Returned by [`ipns_publish`].
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct IpnsPublishInfo {
    pub name: String,
}

/// Returned by [`ipns_resolve`].
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct IpnsResolveInfo {
    pub name: String,
    pub value: String,
}

/// Health / metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct NodeMetricsInfo {
    pub peer_count: u32,
    pub blob_count: u32,
    pub gossip_topics: u32,
    pub uptime_secs: u64,
}

/// Reserved for the gossip subscribe API. The uniffi callback
/// interface machinery (UDL file + Swift/Kotlin listener) is
/// tracked in a follow-up PR; for v0.1 we expose a placeholder
/// that returns the topic name as a subscription id so the
/// Swift demo can render a successful UI flow.
#[uniffi::export]
pub fn adnet_ffi_placeholder_gossip_subscribe(
    topic: String,
) -> Result<String, AdnetError> {
    if topic.is_empty() {
        return Err(AdnetError::InvalidArgument {
            message: "topic must not be empty".into(),
        });
    }
    Ok(format!("gossip::{}", topic))
}

/// Opaque handle to an ADNet node. Mobile callers create one
/// via [`AdnetHandle::new`] and call methods on it. The
/// handle owns a tokio runtime so calls from Swift / Kotlin
/// can be synchronous from the caller's perspective.
///
/// For v0.1 the handle holds only the runtime; the underlying
/// `adnet_node::Node` is constructed on first use and held by
/// the inner mutex. A future PR will lift the node creation
/// into a separate "boot" call so callers can introspect the
/// node id before any blob operations.
#[derive(uniffi::Object)]
pub struct AdnetHandle {
    runtime: Runtime,
    inner: Arc<std::sync::Mutex<Option<adnet_node::Node>>>,
    data_dir: PathBuf,
    node_id: Arc<std::sync::Mutex<Option<String>>>,
}

#[uniffi::export]
impl AdnetHandle {
    /// Boot a node rooted at `data_dir`.
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Result<Arc<Self>, AdnetError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| AdnetError::Runtime {
                message: e.to_string(),
            })?;
        let path = PathBuf::from(&data_dir);
        let handle = Self {
            runtime,
            inner: Arc::new(std::sync::Mutex::new(None)),
            data_dir: path,
            node_id: Arc::new(std::sync::Mutex::new(None)),
        };
        Ok(Arc::new(handle))
    }

    /// Eagerly construct the inner node so subsequent calls
    /// can introspect the local id. Idempotent — repeat calls
    /// return the cached id.
    pub fn ensure_booted(&self) -> Result<String, AdnetError> {
        if let Some(id) = self.node_id.lock().expect("poisoned").clone() {
            return Ok(id);
        }
        let mut guard = self.inner.lock().expect("poisoned");
        let node = if let Some(n) = guard.as_ref() {
            n
        } else {
            let cfg = self
                .runtime
                .block_on(async {
                    adnet_node::NodeConfig::load_or_create(&self.data_dir)
                })
                .map_err(|e| AdnetError::Io {
                    message: e.to_string(),
                })?;
            let n = self
                .runtime
                .block_on(async { adnet_node::Node::builder(cfg).build().await })
                .map_err(|e: anyhow::Error| AdnetError::Node {
                    message: e.to_string(),
                })?;
            *guard = Some(n);
            guard.as_ref().expect("just inserted")
        };
        let id = node.node_id().to_string();
        *self.node_id.lock().expect("poisoned") = Some(id.clone());
        Ok(id)
    }

    /// Return the local node's hex-encoded `NodeId`.
    pub fn node_id(&self) -> Result<String, AdnetError> {
        let guard = self.inner.lock().expect("poisoned");
        let n = guard
            .as_ref()
            .ok_or_else(|| AdnetError::InvalidArgument {
                message: "node not created".into(),
            })?;
        Ok(n.node_id().to_string())
    }

    /// Snapshot of the local node's identity. Convenience
    /// accessor that bundles `node_id()` with an `uptime`
    /// hint (kept as 0 until the node exposes a real clock
    /// — see the C-ABI NodeInfo for the richer shape).
    pub fn info(&self) -> Result<NodeInfo, AdnetError> {
        let id = self.node_id()?;
        Ok(NodeInfo {
            node_id: id,
            uptime_secs: 0,
        })
    }

    /// Persist a blob, return hash + ticket.
    ///
    /// **Implementation note**: ADNet does not yet expose a
    /// `put_blob` API on `Node`; we hash the bytes in-place and
    /// emit the hex hash as a v0.1 placeholder ticket. The
    /// `fetch_ticket` path consumes the same shape, so the
    /// Swift / Kotlin demo is runnable end-to-end.
    pub fn put_bytes(&self, data: Vec<u8>) -> Result<BlobPutInfo, AdnetError> {
        let hash = adnet_types::ContentHash::from_bytes(&data);
        Ok(BlobPutInfo {
            hash: hash.as_hex().to_string(),
            ticket: hash.as_hex().to_string(),
            size: data.len() as u64,
        })
    }

    /// Fetch a blob by ticket.
    ///
    /// **Implementation note**: mirrors the C ABI
    /// `adnet_ffi_blob_fetch_ticket` — accepts a hex
    /// `ContentHash` as the ticket, returns the same hash
    /// with `size=0` (the underlying `Node::fetch_blob`
    /// is not yet wired over uniffi, so the round-trip
    /// stays byte-stable for the v0.1 SDK demo).
    pub fn fetch_ticket(&self, ticket: String) -> Result<BlobFetchInfo, AdnetError> {
        if ticket.is_empty() {
            return Err(AdnetError::InvalidArgument {
                message: "ticket must not be empty".into(),
            });
        }
        let hash = adnet_types::ContentHash::from_hex(&ticket).map_err(|e| {
            AdnetError::InvalidArgument {
                message: format!("bad ticket: {e}"),
            }
        })?;
        Ok(BlobFetchInfo {
            hash: hash.as_hex().to_string(),
            size: 0,
        })
    }

    /// Publish an IPNS record. `value` is typically a
    /// `/adnet/blob/<hex-hash>` path. For v0.1 we accept the
    /// call and return the name unchanged; the underlying
    /// transport is wired in a follow-up PR.
    pub fn ipns_publish(
        &self,
        name: String,
        _value: String,
    ) -> Result<IpnsPublishInfo, AdnetError> {
        if name.is_empty() {
            return Err(AdnetError::InvalidArgument {
                message: "name must not be empty".into(),
            });
        }
        Ok(IpnsPublishInfo { name })
    }

    /// Resolve an IPNS name to its current value (empty when
    /// no record is known locally).
    pub fn ipns_resolve(&self, name: String) -> Result<IpnsResolveInfo, AdnetError> {
        if name.is_empty() {
            return Err(AdnetError::InvalidArgument {
                message: "name must not be empty".into(),
            });
        }
        Ok(IpnsResolveInfo {
            name,
            value: String::new(),
        })
    }

    /// Cheap metrics snapshot the SDK can render on a
    /// "Network" tab. Counts are placeholders until
    /// `Node::metrics` is added.
    ///
    /// Mirrors the C ABI: requires the node to be booted
    /// (call `ensure_booted()` first) so the caller can
    /// distinguish "node not yet ready" from "0 active
    /// peers / topics".
    pub fn metrics(&self) -> Result<NodeMetricsInfo, AdnetError> {
        let guard = self.inner.lock().expect("poisoned");
        if guard.is_none() {
            return Err(AdnetError::InvalidArgument {
                message: "node not created".into(),
            });
        }
        Ok(NodeMetricsInfo {
            peer_count: 0,
            blob_count: 0,
            gossip_topics: 0,
            uptime_secs: 0,
        })
    }

    /// Tear down the node. After this call the handle is
    /// invalid until `ensure_booted()` is called again.
    pub fn destroy(&self) -> Result<(), AdnetError> {
        // Take the node out so the next call sees a clean
        // slate (without this, `node_id()` / `info()` /
        // `metrics()` would still report a stale id and
        // blind-zero counters).
        let mut guard = self.inner.lock().expect("poisoned");
        if let Some(node) = guard.take() {
            let _ = self
                .runtime
                .block_on(async { node.shutdown().await });
        }
        drop(guard);
        *self.node_id.lock().expect("poisoned") = None;
        Ok(())
    }
}

/// Library version. The Swift / Kotlin caller can refuse to
/// load a mismatched library.
#[uniffi::export]
pub fn adnet_version() -> u32 {
    crate::ADNET_FFI_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variants_are_distinct() {
        // Each variant must round-trip through serde without
        // colliding — guards against a typo in `tag`.
        for v in [
            AdnetError::InvalidArgument { message: "a".into() },
            AdnetError::InvalidUtf8 { message: "b".into() },
            AdnetError::InvalidJson { message: "c".into() },
            AdnetError::Node { message: "d".into() },
            AdnetError::Runtime { message: "e".into() },
            AdnetError::Feature { message: "f".into() },
            AdnetError::Io { message: "g".into() },
            AdnetError::NotFound { message: "h".into() },
            AdnetError::Profile { message: "i".into() },
        ] {
            let json = serde_json::to_string(&v).unwrap();
            let back: AdnetError = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{v:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn from_ffi_error_maps_profile_to_profile() {
        // The C-ABI `AdnetFfiError::Profile` should NOT be
        // collapsed into `Node` — the uniffi surface
        // preserves the distinction so mobile callers can
        // react to "re-login" hints.
        let r: AdnetError =
            crate::AdnetFfiError::Profile("user not found".into()).into();
        match r {
            AdnetError::Profile { message } => {
                assert_eq!(message, "user not found");
            }
            other => panic!("expected Profile, got {other:?}"),
        }
    }

    #[test]
    fn error_display_covers_all_variants() {
        for v in [
            AdnetError::InvalidArgument { message: "x".into() },
            AdnetError::InvalidUtf8 { message: "x".into() },
            AdnetError::InvalidJson { message: "x".into() },
            AdnetError::Node { message: "x".into() },
            AdnetError::Runtime { message: "x".into() },
            AdnetError::Feature { message: "x".into() },
            AdnetError::Io { message: "x".into() },
            AdnetError::NotFound { message: "x".into() },
            AdnetError::Profile { message: "x".into() },
        ] {
            let s = v.to_string();
            assert!(s.contains("x"));
        }
    }

    #[test]
    fn info_records_round_trip() {
        let p = BlobPutInfo {
            hash: "abcd".into(),
            ticket: "ticket".into(),
            size: 1024,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: BlobPutInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(p.hash, back.hash);
        assert_eq!(p.ticket, back.ticket);
        assert_eq!(p.size, back.size);
    }

    #[test]
    fn placeholder_gossip_rejects_empty_topic() {
        let r = adnet_ffi_placeholder_gossip_subscribe(String::new());
        assert!(matches!(
            r,
            Err(AdnetError::InvalidArgument { .. })
        ));
        let ok = adnet_ffi_placeholder_gossip_subscribe("room-42".into()).unwrap();
        assert_eq!(ok, "gossip::room-42");
    }
}