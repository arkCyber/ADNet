//! Combined relay entrypoint.
//!
//! `CombinedRelayConfig` lets an operator start the DHT bootstrap,
//! the WAN HTTP relay, the iroh DERP server, and the operator
//! control plane with a single builder call. Internally this is
//! just orchestration — every component already has its own
//! `start_*` / `shutdown` API; this module wires them together
//! behind one handle so the binary doesn't have to repeat the
//! dance.
//!
//! ```no_run
//! use a3net_relay::combined::{CombinedRelayConfig, start_combined};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = CombinedRelayConfig::default();
//! let handle = start_combined(cfg).await?;
//! handle.shutdown().await;
//! # Ok(()) }
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::RelayConfig;
use crate::control::{start_control, ControlConfig, ControlState};
use crate::dht_bootstrap::{serve as serve_bootstrap, BootstrapConfig, BootstrapHandle};
use crate::proxy_policy::{HostPolicy, SafeRedirectPolicy, DEFAULT_MAX_BODY_BYTES, DEFAULT_UPSTREAM_TIMEOUT};
use crate::server::{RelayServer, RelayServerHandle, ServerPolicy};

/// Configuration for the combined relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedRelayConfig {
    pub relay: Option<RelayConfig>,
    pub dht_bootstrap: Option<DhtBootstrapConfig>,
    pub control: Option<ControlConfigExternal>,
    pub derp: Option<bool>, // feature-gated at the call site
}

/// Plain-old-data DHT bootstrap config — operators pass these in
/// JSON without dragging in the runtime types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtBootstrapConfig {
    pub bind: SocketAddr,
    pub static_peers: Vec<PeerInfoJson>,
    pub peers_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfoJson {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlConfigExternal {
    pub bind: SocketAddr,
    pub auth_token: Option<String>,
}

impl Default for CombinedRelayConfig {
    fn default() -> Self {
        Self {
            relay: Some(RelayConfig::default()),
            dht_bootstrap: None,
            control: Some(ControlConfigExternal {
                bind: "127.0.0.1:9091".parse().unwrap(),
                auth_token: None,
            }),
            derp: Some(false),
        }
    }
}

/// Handle returned by [`start_combined`]. Drops the child handles.
pub struct CombinedRelayHandle {
    #[cfg(feature = "wan-relay")]
    pub relay: Option<RelayServerHandle>,
    pub dht_bootstrap: Option<BootstrapHandle>,
    pub control_addr: Option<SocketAddr>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl CombinedRelayHandle {
    pub async fn shutdown(mut self) {
        #[cfg(feature = "wan-relay")]
        if let Some(r) = self.relay.take() {
            r.shutdown();
        }
        if let Some(b) = self.dht_bootstrap.take() {
            b.shutdown();
        }
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Spawn every component configured in `cfg`. Each component is
/// independently optional — operators who only want the relay do
/// not need to start the bootstrap and vice versa.
pub async fn start_combined(
    cfg: CombinedRelayConfig,
) -> Result<CombinedRelayHandle, String> {
    let state = ControlState::new();
    state.mark_started();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();

    // DHT bootstrap.
    let mut dht_handle: Option<BootstrapHandle> = None;
    if let Some(dht_cfg) = cfg.dht_bootstrap.clone() {
        let mut bcfg = BootstrapConfig::new();
        // Inline peers take precedence.
        if !dht_cfg.static_peers.is_empty() {
            bcfg = bcfg.with_static_peers(
                dht_cfg.static_peers.into_iter().map(|p| (p.node_id, p.addr)),
            );
        }
        if let Some(file) = dht_cfg.peers_file {
            match crate::dht_bootstrap::load_peers_from_file(&file) {
                Ok(peers) => bcfg = bcfg.with_static_peers(peers),
                Err(e) => tracing::warn!(error = %e, "could not load peers file"),
            }
        }
        match serve_bootstrap(dht_cfg.bind, bcfg).await {
            Ok(h) => {
                state.set_dht_bootstrap_running(true);
                dht_handle = Some(h);
            }
            Err(e) => return Err(format!("dht bootstrap bind: {e}")),
        }
    }

    // WAN HTTP relay (only when the wan-relay feature is on).
    #[cfg(feature = "wan-relay")]
    let mut relay_handle: Option<RelayServerHandle> = None;
    #[cfg(feature = "wan-relay")]
    {
        if let Some(relay_cfg) = cfg.relay.clone() {
            let bind = if relay_cfg.serve_bind.is_empty() {
                "0.0.0.0".to_string()
            } else {
                relay_cfg.serve_bind.clone()
            };
            let port = relay_cfg.serve_port;
            let policy = ServerPolicy {
                host_policy: HostPolicy::DefaultBlockPrivate,
                max_body_bytes: DEFAULT_MAX_BODY_BYTES,
                upstream_timeout: DEFAULT_UPSTREAM_TIMEOUT,
                redirect_policy: SafeRedirectPolicy::new(HostPolicy::DefaultBlockPrivate),
            };
            match RelayServer::start_with_policy(&bind, port, crate::billing::BillingMode::Disabled, policy).await {
                Ok(h) => {
                    state.set_relay_running(true);
                    relay_handle = Some(h);
                }
                Err(e) => return Err(format!("relay bind: {e}")),
            }
        }
    }

    // Control plane.
    let mut control_addr: Option<SocketAddr> = None;
    if let Some(ext) = cfg.control.clone() {
        let mut ctrl_cfg = ControlConfig::default().with_bind(ext.bind);
        if let Some(token) = ext.auth_token.clone() {
            ctrl_cfg = ctrl_cfg.with_token(token);
        }
        match start_control(ctrl_cfg, state.clone(), shutdown_rx).await {
            Ok(h) => control_addr = h.local,
            Err(e) => return Err(format!("control bind: {e}")),
        }
    }

    Ok(CombinedRelayHandle {
        #[cfg(feature = "wan-relay")]
        relay: relay_handle,
        dht_bootstrap: dht_handle,
        control_addr,
        shutdown_tx: Some(shutdown_tx),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_relay_and_control_but_not_bootstrap() {
        let cfg = CombinedRelayConfig::default();
        assert!(cfg.relay.is_some());
        assert!(cfg.dht_bootstrap.is_none());
        assert!(cfg.control.is_some());
    }

    #[test]
    fn peer_info_round_trip() {
        let j = serde_json::to_string(&PeerInfoJson {
            node_id: "abc".into(),
            addr: "1.2.3.4:7777".into(),
        })
        .unwrap();
        let back: PeerInfoJson = serde_json::from_str(&j).unwrap();
        assert_eq!(back.node_id, "abc");
        assert_eq!(back.addr, "1.2.3.4:7777");
    }

    #[test]
    fn combined_config_round_trip() {
        let cfg = CombinedRelayConfig {
            relay: None,
            dht_bootstrap: Some(DhtBootstrapConfig {
                bind: "127.0.0.1:9999".parse().unwrap(),
                static_peers: vec![PeerInfoJson {
                    node_id: "alice".into(),
                    addr: "1.1.1.1:7777".into(),
                }],
                peers_file: None,
            }),
            control: None,
            derp: Some(false),
        };
        let s = serde_json::to_string(&cfg).unwrap();
        let back: CombinedRelayConfig = serde_json::from_str(&s).unwrap();
        assert!(back.dht_bootstrap.is_some());
        assert_eq!(back.dht_bootstrap.unwrap().static_peers.len(), 1);
    }
}