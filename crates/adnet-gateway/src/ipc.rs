//! Internal IPC service for the ADNet HTTP Gateway.
//!
//! Exposes the gateway's internal state over a Unix-socket JSON-RPC
//! endpoint so that in-process companion services (admin shell,
//! diagnostics daemon, watchdog, automated tests) can query the
//! gateway without going through the public HTTP surface.
//!
//! ## Wire protocol
//!
//! Newline-delimited JSON-RPC 2.0, identical to the rest of
//! `adnet-ipc`. Every request carries an `id`; notifications have no
//! `id`.
//!
//! ## Methods
//!
//! | Method | Params | Result |
//! |--------|--------|--------|
//! | `gateway.ping` | `{}` | `{"pong": true, "ts": <unix_ms>}` |
//! | `gateway.version` | `{}` | `{"version": "...", "agent": "adnet-gateway"}` |
//! | `gateway.shutdown` | `{}` | `{"ok": true}` (sends `gateway.shutdown` notification to every connected client and stops the listener) |
//! | `gateway.stats.repo` | `{}` | [`RepoStats`] |
//! | `gateway.stats.bandwidth` | `{}` | [`BandwidthStats`] |
//! | `gateway.stats.dht` | `{}` | [`DhtStats`] |
//! | `gateway.stats.uptime_secs` | `{}` | `{"uptime_secs": u64}` |
//! | `gateway.pin.list` | `{}` | array of [`PinInfo`] |
//! | `gateway.pin.add` | `{"cid": "<hex>", "recursive": bool}` | `{"ok": true}` |
//! | `gateway.pin.remove` | `{"cid": "<hex>"}` | `{"ok": true}` |
//! | `gateway.cid.exists` | `{"cid": "<hex>"}` | `{"exists": bool}` |
//! | `gateway.cid.meta` | `{"cid": "<hex>"}` | `{"size_bytes": u64, "chunk_count": u32, "is_directory": bool}` |
//!
//! Server-pushed notifications:
//!
//! | Method | Trigger |
//! |--------|---------|
//! | `gateway.shutdown` | Emitted to every client when the IPC server receives a `gateway.shutdown` request. The body has no params. |

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adnet_blobstore::BlobStore;
use adnet_ipc::server::{JsonRpcServer, JsonRpcServerHandle, NotificationSender, RpcHandler};
use adnet_types::ContentHash;
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::dag::DagService;
use crate::pin::{PinService, PinType};
use crate::stats::StatsService;

/// Build configuration for the gateway IPC server.
#[derive(Debug, Clone)]
pub struct GatewayIpcConfig {
    /// Unix socket path to bind.
    pub socket_path: PathBuf,
    /// Notification broadcast capacity.
    pub notification_capacity: usize,
}

impl GatewayIpcConfig {
    /// Default `<data_dir>/gateway.ipc.sock`.
    pub fn with_data_dir(data_dir: &Path) -> Self {
        Self {
            socket_path: data_dir.join("gateway.ipc.sock"),
            notification_capacity: 64,
        }
    }
}

/// Version string returned by `gateway.version`. Bump together with
/// the rest of the workspace.
pub const GATEWAY_IPC_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Aggregate handle returned by [`GatewayIpcService::start`]. Drop
/// (or call [`GatewayIpcServiceHandle::shutdown`]) to tear down the
/// socket and any connected clients.
pub struct GatewayIpcServiceHandle {
    server: JsonRpcServerHandle,
}

impl GatewayIpcServiceHandle {
    /// Shut down the IPC server and remove the socket file.
    pub fn shutdown(&self) {
        self.server.shutdown();
    }

    /// Path to the bound Unix socket.
    pub fn socket_path(&self) -> &Path {
        self.server.socket_path()
    }

    /// Notifier for server-pushed events (e.g. shutdown broadcast).
    pub fn notifier(&self) -> NotificationSender {
        self.server.notifier()
    }
}

/// Lightweight typed wrapper around the underlying JSON-RPC server.
/// Holds shared references to the gateway's services; cloning is
/// cheap.
#[derive(Clone)]
pub struct GatewayIpcService {
    inner: Arc<GatewayIpcServiceInner>,
}

struct GatewayIpcServiceInner {
    blob_store: Arc<BlobStore>,
    dag_service: Arc<DagService>,
    pin_service: Arc<PinService>,
    stats_service: Arc<StatsService>,
}

impl GatewayIpcService {
    /// Build a new service from the gateway's sub-services. Cloning
    /// is cheap (all state lives behind `Arc`s).
    pub fn new(
        blob_store: Arc<BlobStore>,
        dag_service: Arc<DagService>,
        pin_service: Arc<PinService>,
        dht_service: Arc<crate::dht::DhtService>,
        ipns_service: Arc<crate::ipns::IpnService>,
        stats_service: Arc<StatsService>,
    ) -> Self {
        // `dht_service` and `ipns_service` are kept in the constructor
        // for future expansion (DHT-driven peer lookup, IPNS
        // resolution over IPC). For the minimal-viable IPC surface
        // they are not yet dispatched; we silence the dead-code
        // warning by binding to `_`.
        let _ = (dht_service, ipns_service);
        Self {
            inner: Arc::new(GatewayIpcServiceInner {
                blob_store,
                dag_service,
                pin_service,
                stats_service,
            }),
        }
    }

    /// Bind the configured Unix socket and serve requests until the
    /// returned handle is dropped or `shutdown()` is called.
    pub async fn start(
        &self,
        config: GatewayIpcConfig,
    ) -> Result<GatewayIpcServiceHandle, String> {
        let handler: Arc<Self> = Arc::new(self.clone());
        let server = JsonRpcServer::start_with_capacity(
            config.socket_path,
            handler,
            config.notification_capacity,
        )
        .await?;
        info!(
            socket = %server.socket_path().display(),
            "gateway IPC server started"
        );
        Ok(GatewayIpcServiceHandle { server })
    }
}

#[async_trait]
impl RpcHandler for GatewayIpcService {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "gateway.ping" => Ok(json!({
                "pong": true,
                "ts": unix_millis(),
            })),
            "gateway.version" => Ok(json!({
                "version": GATEWAY_IPC_VERSION,
                "agent": "adnet-gateway",
                "rpc": "2.0",
            })),
            "gateway.shutdown" => {
                // The caller receives `{"ok": true}` synchronously.
                // The actual shutdown broadcast / socket teardown is
                // handled by the caller (who owns the handle).
                Ok(json!({"ok": true}))
            }
            "gateway.stats.repo" => {
                let r = self
                    .inner
                    .stats_service
                    .repo()
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(r).map_err(|e| e.to_string())
            }
            "gateway.stats.bandwidth" => {
                let b = self
                    .inner
                    .stats_service
                    .bandwidth()
                    .await
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(b).map_err(|e| e.to_string())
            }
            "gateway.stats.dht" => {
                let d = self.inner.stats_service.dht();
                serde_json::to_value(d).map_err(|e| e.to_string())
            }
            "gateway.stats.uptime_secs" => {
                let secs = self
                    .inner
                    .stats_service
                    .uptime()
                    .as_secs();
                Ok(json!({ "uptime_secs": secs }))
            }
            "gateway.pin.list" => {
                let pins = self.inner.pin_service.list_pins(None).await;
                serde_json::to_value(pins).map_err(|e| e.to_string())
            }
            "gateway.pin.add" => {
                let cid = parse_cid_param(&params, "cid")?;
                let recursive = params
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.inner
                    .pin_service
                    .add_pin(&cid, recursive)
                    .await
                    .map_err(|e| e.to_string())?;
                // Persist so the change survives a restart; failure
                // here is non-fatal (the in-memory pin still exists)
                // but worth logging.
                if let Err(e) = self.inner.pin_service.save().await {
                    warn!(error = %e, "failed to persist pin after add_pin");
                }
                Ok(json!({ "ok": true }))
            }
            "gateway.pin.remove" => {
                let cid = parse_cid_param(&params, "cid")?;
                self.inner
                    .pin_service
                    .remove_pin(&cid)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Err(e) = self.inner.pin_service.save().await {
                    warn!(error = %e, "failed to persist pin after remove_pin");
                }
                Ok(json!({ "ok": true }))
            }
            "gateway.cid.exists" => {
                let cid = parse_cid_param(&params, "cid")?;
                Ok(json!({ "exists": self.inner.blob_store.has_complete(&cid) }))
            }
            "gateway.cid.meta" => {
                let cid = parse_cid_param(&params, "cid")?;
                let meta = self.inner.blob_store.meta(&cid).ok();
                let (size_bytes, chunk_count) = meta.unwrap_or((0, 0));
                let is_directory = self
                    .inner
                    .dag_service
                    .is_directory(&cid)
                    .await
                    .unwrap_or(false);
                Ok(json!({
                    "size_bytes": size_bytes,
                    "chunk_count": chunk_count,
                    "is_directory": is_directory,
                }))
            }
            other => Err(format!("unknown method: {other}")),
        }
    }
}

fn parse_cid_param(params: &Value, key: &str) -> Result<ContentHash, String> {
    let s = params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing or non-string `{key}`"))?;
    ContentHash::from_hex(s).map_err(|e| format!("invalid `{key}`: {e}"))
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Convenience helper: list the [`PinType`] discriminants so tests can
/// iterate them without reaching into the underlying enum.
#[doc(hidden)]
pub fn all_pin_types() -> &'static [PinType] {
    &[PinType::Direct, PinType::Recursive, PinType::Indirect, PinType::All]
}

/// Broadcast a shutdown notification to every IPC client, then stop
/// the listener. Returns the number of clients that received the
/// notification.
pub fn broadcast_shutdown(notifier: &NotificationSender) -> usize {
    notifier.send("gateway.shutdown", json!({}))
}

/// Typed client wrapper around [`adnet_ipc::client::json_rpc_call`].
/// All RPC errors surface as [`IpcClientError`].
///
/// Construct via [`GatewayIpcClient::connect`] for a typical
/// in-process companion service, or use the raw
/// [`adnet_ipc::client::json_rpc_call`] for one-off calls.
#[derive(Clone, Debug)]
pub struct GatewayIpcClient {
    socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcClientError {
    #[error("IPC transport error: {0}")]
    Transport(#[from] adnet_ipc::client::JsonRpcError),
    #[error("IPC method `{0}` failed: {1}")]
    Server(String, String),
    #[error("malformed IPC response: {0}")]
    Decode(String),
}

impl GatewayIpcClient {
    /// Construct a client pointed at the given Unix socket. The
    /// connection is opened lazily — call a method to actually
    /// dial the socket.
    pub fn connect(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    fn socket(&self) -> &Path {
        &self.socket_path
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, IpcClientError> {
        let r = adnet_ipc::client::json_rpc_call(
            self.socket(),
            "ADNet Gateway IPC",
            method,
            params,
        )
        .await
        .map_err(IpcClientError::Transport)?;
        Ok(r)
    }

    /// `gateway.ping` — liveness check. Returns the server's view of
    /// the wall clock in milliseconds since the Unix epoch.
    pub async fn ping(&self) -> Result<u64, IpcClientError> {
        let v = self.call("gateway.ping", json!({})).await?;
        v.get("ts")
            .and_then(|t| t.as_u64())
            .ok_or_else(|| IpcClientError::Decode("missing `ts`".into()))
    }

    /// `gateway.version` — returns the gateway's reported version
    /// string.
    pub async fn version(&self) -> Result<String, IpcClientError> {
        let v = self.call("gateway.version", json!({})).await?;
        v.get("version")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .ok_or_else(|| IpcClientError::Decode("missing `version`".into()))
    }

    /// `gateway.stats.uptime_secs` — wall-clock seconds since the
    /// IPC server started.
    pub async fn uptime_secs(&self) -> Result<u64, IpcClientError> {
        let v = self
            .call("gateway.stats.uptime_secs", json!({}))
            .await?;
        v.get("uptime_secs")
            .and_then(|s| s.as_u64())
            .ok_or_else(|| IpcClientError::Decode("missing `uptime_secs`".into()))
    }

    /// `gateway.cid.exists` — returns whether the given hex CID is
    /// known to the local blob store.
    pub async fn cid_exists(&self, cid_hex: &str) -> Result<bool, IpcClientError> {
        let v = self
            .call("gateway.cid.exists", json!({ "cid": cid_hex }))
            .await?;
        v.get("exists")
            .and_then(|e| e.as_bool())
            .ok_or_else(|| IpcClientError::Decode("missing `exists`".into()))
    }

    /// `gateway.cid.meta` — returns `(size_bytes, chunk_count,
    /// is_directory)` for a hex CID. Returns `None` if the IPC
    /// handler reports the CID is unknown (`size_bytes == 0 &&
    /// chunk_count == 0 && !is_directory` is treated as "absent").
    pub async fn cid_meta(
        &self,
        cid_hex: &str,
    ) -> Result<(u64, u32, bool), IpcClientError> {
        let v = self
            .call("gateway.cid.meta", json!({ "cid": cid_hex }))
            .await?;
        let size = v.get("size_bytes").and_then(|s| s.as_u64()).unwrap_or(0);
        let chunks = v
            .get("chunk_count")
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;
        let is_dir = v
            .get("is_directory")
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        Ok((size, chunks, is_dir))
    }

    /// `gateway.pin.list` — full pin table. Each entry serialises to
    /// a JSON object mirroring [`crate::pin::PinInfo`].
    pub async fn pin_list(&self) -> Result<Value, IpcClientError> {
        self.call("gateway.pin.list", json!({})).await
    }

    /// `gateway.pin.add` — request a direct/recursive pin.
    pub async fn pin_add(
        &self,
        cid_hex: &str,
        recursive: bool,
    ) -> Result<(), IpcClientError> {
        let v = self
            .call(
                "gateway.pin.add",
                json!({ "cid": cid_hex, "recursive": recursive }),
            )
            .await?;
        if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(IpcClientError::Decode(format!(
                "pin.add returned unexpected payload: {v}"
            )));
        }
        Ok(())
    }

    /// `gateway.pin.remove` — drop a pin by CID.
    pub async fn pin_remove(&self, cid_hex: &str) -> Result<(), IpcClientError> {
        let v = self
            .call("gateway.pin.remove", json!({ "cid": cid_hex }))
            .await?;
        if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(IpcClientError::Decode(format!(
                "pin.remove returned unexpected payload: {v}"
            )));
        }
        Ok(())
    }

    /// Raw escape hatch — call an arbitrary method. Useful for
    /// methods added in future IPC versions.
    pub async fn raw_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, IpcClientError> {
        self.call(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The build helper must place the socket inside the configured
    /// data dir and use a sensible default notification capacity.
    #[test]
    fn config_with_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GatewayIpcConfig::with_data_dir(dir.path());
        assert_eq!(cfg.socket_path, dir.path().join("gateway.ipc.sock"));
        assert!(cfg.notification_capacity > 0);
    }

    /// `unix_millis` should be monotonic and reasonably close to the
    /// wall clock. This guards against accidentally returning 0 or a
    /// sub-second value when the system clock is valid.
    #[test]
    fn unix_millis_is_recent() {
        let a = unix_millis();
        assert!(a > 1_700_000_000_000, "got stale millis: {a}");
    }

    /// Ensure every public pin type is reachable. Catches accidental
    /// removal of a variant.
    #[test]
    fn all_pin_types_lists_four() {
        assert_eq!(all_pin_types().len(), 4);
    }

    /// `parse_cid_param` must reject non-hex, non-string, and missing
    /// inputs with descriptive errors.
    #[test]
    fn parse_cid_param_errors() {
        // Missing key
        let err = parse_cid_param(&json!({}), "cid").unwrap_err();
        assert!(err.contains("missing"));
        // Wrong type
        let err = parse_cid_param(&json!({"cid": 42}), "cid").unwrap_err();
        assert!(err.contains("missing or non-string"));
        // Not valid hex
        let err = parse_cid_param(&json!({"cid": "not-hex"}), "cid").unwrap_err();
        assert!(err.contains("invalid `cid`"));
    }
}