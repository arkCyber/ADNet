//! Smart home control center for [`crate::Node`].
//!
//! Feature-gated behind `smarthome` (= `dep:adnet-smarthome`). Wires a
//! [`adnet_smarthome::SmartHomeHub`] (device registry, mDNS discovery,
//! Xiaomi MIoT cloud bridge, automation engine) plus its REST API onto
//! this node so a NAS deployment can double as a home smart-device
//! control center.

use std::sync::Arc;

use adnet_smarthome as smarthome;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors surfaced by the smart-home control plane.
#[derive(Debug, Error)]
pub enum SmartHomeNodeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hub error: {0}")]
    Hub(#[from] smarthome::SmartHomeError),
}

/// Configuration for the smart-home control center.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartHomeConfig {
    /// REST API bind address (devices/automations control plane).
    pub bind: std::net::SocketAddr,
    /// Bearer token required on every API request (except
    /// `/healthz`). Strongly recommended if `bind` is anything other
    /// than a loopback address — this API can control physical
    /// devices (locks, cameras, ...).
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Enable periodic mDNS discovery of local devices.
    pub enable_discovery: bool,
    /// Seconds between discovery scans.
    pub discovery_interval_secs: u64,
    /// Seconds of silence before a device is marked offline.
    pub heartbeat_timeout_secs: u64,
    /// Directory for persisted device registry state. Relative paths
    /// are resolved against the node's `data_dir`.
    pub data_subdir: String,
}

impl Default for SmartHomeConfig {
    fn default() -> Self {
        Self {
            bind: std::net::SocketAddr::from(([127, 0, 0, 1], 8781)),
            auth_token: None,
            enable_discovery: true,
            discovery_interval_secs: 300,
            heartbeat_timeout_secs: 120,
            data_subdir: "smarthome".to_string(),
        }
    }
}

/// Xiaomi MIoT cloud credentials, obtained via QR login
/// (`adnet_smarthome::miot::qrlogin`).
pub type MiotAuth = smarthome::MiotAuth;

/// Handle to the running smart-home control center.
pub struct SmartHomeHandle {
    pub hub: Arc<smarthome::SmartHomeHub>,
    pub api: smarthome::ApiHandle,
}

impl std::fmt::Debug for SmartHomeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartHomeHandle")
            .field("api_addr", &self.api.local_addr())
            .finish()
    }
}

impl SmartHomeHandle {
    pub fn local_addr(&self) -> std::net::SocketAddr {
        self.api.local_addr()
    }

    /// Subscribe to hub events (device online/offline, property
    /// changes, discovery, ...).
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<smarthome::HubEvent> {
        self.hub.subscribe()
    }

    pub fn shutdown(&self) {
        self.api.shutdown();
    }
}

/// Build and start a [`smarthome::SmartHomeHub`] + REST API rooted at
/// this node's data directory. `miot_auth` is optional: without it the
/// hub still runs mDNS discovery, the device registry, and automations,
/// but MIoT-specific calls (`set_property` / `get_properties` /
/// `invoke_action`) return `NotSupported`.
pub async fn start(
    node_data_dir: &std::path::Path,
    cfg: SmartHomeConfig,
    miot_auth: Option<MiotAuth>,
) -> Result<SmartHomeHandle, SmartHomeNodeError> {
    let data_dir = node_data_dir.join(&cfg.data_subdir);
    std::fs::create_dir_all(&data_dir)?;

    let hub_cfg = smarthome::HubConfig {
        bind_addr: cfg.bind,
        enable_discovery: cfg.enable_discovery,
        discovery_interval_secs: cfg.discovery_interval_secs,
        heartbeat_timeout_secs: cfg.heartbeat_timeout_secs,
        data_dir: data_dir.to_string_lossy().into_owned(),
    };

    let mut hub = smarthome::SmartHomeHub::new(hub_cfg);
    if let Some(auth) = miot_auth {
        hub = hub.with_miot(auth)?;
    }
    let hub = Arc::new(hub);

    Arc::clone(&hub).start().await?;

    let api = smarthome::api::serve(
        Arc::clone(&hub),
        smarthome::ApiConfig { bind: cfg.bind, auth_token: cfg.auth_token },
    )
    .await?;

    Ok(SmartHomeHandle { hub, api })
}
