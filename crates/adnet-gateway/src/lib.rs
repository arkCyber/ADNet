//! `adnet-gateway` — IPFS-compatible HTTP Gateway for ADNet.
//!
//! This crate provides an HTTP gateway that is compatible with the IPFS HTTP
//! Gateway specification. It allows clients to retrieve content from the
//! ADNet network using paths like `/ipfs/<cid>` and `/ipns/<name>`.
//!
//! ## Supported Endpoints
//!
//! | Endpoint | Method | Description |
//! |----------|--------|-------------|
//! | `/ipfs/{cid}` | GET | Retrieve content by CID |
//! | `/ipfs/{cid}/{path}` | GET | Retrieve path within a DAG |
//! | `/ipns/{name}` | GET | Resolve IPNS name and retrieve content |
//! | `/api/v0/dag/{subcmd}` | POST | DAG operations |
//! | `/api/v0/pin/{subcmd}` | POST | Pin management |
//! | `/api/v0/cat` | POST | Retrieve content |
//! | `/api/v0/block/{subcmd}` | POST | Block operations |
//!
//! ## Design Goals
//!
//! - **Standards Compliant**: Follows the IPFS HTTP Gateway specification
//! - **Content Addressing**: Uses BLAKE3 content hashes as CIDs
//! - **DAG Support**: Supports UnixFS DAG traversal and resolution
//! - **Pinning**: Provides persistent storage guarantees
//! - **CORS**: Supports cross-origin requests
//!
//! ## Example
//!
//! ```rust,ignore
//! use adnet_gateway::{GatewayConfig, GatewayServer};
//! use adnet_blobstore::BlobStore;
//!
//! let config = GatewayConfig::default();
//! let store = BlobStore::new("/data/blobs")?;
//! let server = GatewayServer::new(config, store);
//! server.serve("0.0.0.0:8080").await?;
//! ```

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

pub mod config;
pub mod handler;
pub mod dag;
pub mod ipns;
pub mod pin;
pub mod router;
pub mod object;
pub mod refs;
pub mod stats;
pub mod dht;
pub mod swarm;
pub mod ipc;
pub mod bitswap_api;

pub mod websocket;

pub mod auth;

pub use config::GatewayConfig;
pub use handler::{GatewayHandler, IpfsResponse, GatewayError, IpfsPath};
pub use dag::{DagService, DagPutResult, DagGetResult};
pub use pin::{PinService, PinInfo, PinStatus, PinType, GcResult, PinStats, GcService};
pub use router::GatewayRouter;
pub use object::{ObjectService, ObjectStats, ObjectData};
pub use refs::{RefsService, Ref};
pub use stats::{StatsService, RepoStats, BandwidthStats};
pub use ipns::{IpnService, IpnResolveResult, IpnPublishResult, IpnRecordInfo};
pub use dht::{DhtService, FindProvsResult, ProviderInfo, ProvideResult};
pub use ipc::{
    GatewayIpcClient, GatewayIpcConfig, GatewayIpcService, GatewayIpcServiceHandle,
    IpcClientError, GATEWAY_IPC_VERSION, broadcast_shutdown,
};
pub use swarm::{
    SwarmApi, SwarmPeer,
    BitswapApi, BitswapLedger, BitswapStats,
    KeyApi, KeyInfo,
    RepoApi,
};
pub use bitswap_api::{
    BitswapStatResponse, BitswapLedgerResponse, BitswapListResponse,
    WantlistEntry, ReprovideResponse,
    create_bitswap_router, create_bitswap_state, BitswapAppState,
};

pub use websocket::{
    PubSubService, WsMessage, Event, EventTopic,
    start_websocket_server,
};

pub use auth::{
    AuthService, AuthConfig, AuthResult, AuthContext,
    Role, User, RateLimit, RateLimitInfo,
    AuthorizationResult,
    bearer_token, basic_auth,
};

/// Gateway metrics for observability
pub mod metrics {
    use std::sync::Arc;

    use adnet_observability::metrics::{Counter, Gauge};
    use adnet_observability::registry::Registry;

    #[derive(Debug)]
    pub struct GatewayMetrics {
        pub requests_total: Arc<Counter>,
        pub requests_success: Arc<Counter>,
        pub requests_not_found: Arc<Counter>,
        pub requests_error: Arc<Counter>,
        pub bytes_served: Arc<Counter>,
        pub active_requests: Arc<Gauge>,
    }

    impl GatewayMetrics {
        pub fn register(registry: &Registry) -> Self {
            Self {
                requests_total: registry.register_counter(
                    "adnet_gateway_requests_total",
                    "Total gateway requests",
                ),
                requests_success: registry.register_counter(
                    "adnet_gateway_requests_success_total",
                    "Successful gateway requests",
                ),
                requests_not_found: registry.register_counter(
                    "adnet_gateway_requests_not_found_total",
                    "Not found responses",
                ),
                requests_error: registry.register_counter(
                    "adnet_gateway_requests_error_total",
                    "Error responses",
                ),
                bytes_served: registry.register_counter(
                    "adnet_gateway_bytes_served_total",
                    "Total bytes served",
                ),
                active_requests: registry.register_gauge(
                    "adnet_gateway_active_requests",
                    "Active concurrent requests",
                ),
            }
        }
    }

    impl Default for GatewayMetrics {
        fn default() -> Self {
            Self::register(&std::sync::Arc::new(Registry::default()))
        }
    }

    /// Stand-alone helper that wires the gateway's registry into
    /// the `adnet-observability` HTTP server so callers can expose
    /// `/metrics`, `/health`, `/metrics.json` and `/diagnostics`
    /// without hand-rolling the axum router.
    ///
    /// The function is **opt-in** — the gateway itself does not
    /// start this server; production callers wire it in from
    /// their own bootstrap (e.g. `adnet serve` in the CLI). It
    /// requires the `http-server` feature to be enabled on
    /// `adnet-observability`.
    #[cfg(feature = "metrics-http")]
    pub async fn install_metrics_server(
        bind_addr: std::net::SocketAddr,
        registry: std::sync::Arc<Registry>,
    ) -> ::std::io::Result<adnet_observability::http::MetricsServer> {
        adnet_observability::http::serve(adnet_observability::http::MetricsServerConfig {
            bind_addr,
            registry: Some(registry),
        })
        .await
    }
}
