//! Metrics for the SSH-over-iroh tunnel.
//!
//! The crate registers four counters and a gauge against the
//! global [`a3net_observability::registry::GLOBAL`] registry
//! the first time it is touched. The pattern matches the rest
//! of the A3Net codebase (see `a3net-transport/src/iroh/metrics_bridge.rs`):
//!
//! - `static FOO: Lazy<Arc<Counter>> = Lazy::new(...)` so the
//!   registration is idempotent and lock-free on the hot path.
//! - `register_*` is the only call site that touches the
//!   registry; subsequent calls only `inc()` the cached `Arc`.
//!
//! All metric definitions are unconditionally compiled in
//! because the `a3net-observability` crate is a no-op stub
//! without the `iroh` feature. The counters themselves are
//! useful even when the iroh runtime is off (e.g. for
//! `proxy_bridge` connect attempts) — we just call them
//! through the always-available `a3net_observability::metrics`
//! facade.

use std::sync::Arc;

use a3net_observability::metrics::{Counter, Gauge};
use a3net_observability::registry::{GLOBAL, Registry};
use once_cell::sync::Lazy;

/// Number of inbound SSH-tunnel connections accepted. Increments
/// once per `SshTunnelHandler::accept` callback.
pub static TUNNEL_CONNECTIONS_ACCEPTED: Lazy<Arc<Counter>> = Lazy::new(|| {
    Registry::register_counter(
        &GLOBAL,
        "a3net_ssh_tunnel_connections_accepted_total",
        "Inbound SSH-tunnel connections accepted by the a3net-ssh server.",
    )
});

/// Number of inbound connections that failed to bidirectionally
/// proxy to the local sshd (TCP connect failure, mid-stream
/// death, …). Increments once per failed `proxy_one_connection`.
pub static TUNNEL_CONNECTIONS_FAILED: Lazy<Arc<Counter>> = Lazy::new(|| {
    Registry::register_counter(
        &GLOBAL,
        "a3net_ssh_tunnel_connections_failed_total",
        "Inbound SSH-tunnel connections that errored before completing.",
    )
});

/// Number of `proxy_bridge` (client-side) invocations. Increments
/// at the start of `proxy_bridge`. A 1:1 pairing with
/// `CLIENT_BRIDGES_COMPLETED` would imply zero leaks; in practice
/// the daemon may be SIGKILL'd, so we track both.
pub static CLIENT_BRIDGES_STARTED: Lazy<Arc<Counter>> = Lazy::new(|| {
    Registry::register_counter(
        &GLOBAL,
        "a3net_ssh_client_bridges_started_total",
        "Client-side proxy_bridge invocations started.",
    )
});

/// Number of `proxy_bridge` invocations that returned without
/// error. Lag against `CLIENT_BRIDGES_STARTED` is the leaked-task
/// gauge.
pub static CLIENT_BRIDGES_COMPLETED: Lazy<Arc<Counter>> = Lazy::new(|| {
    Registry::register_counter(
        &GLOBAL,
        "a3net_ssh_client_bridges_completed_total",
        "Client-side proxy_bridge invocations that returned cleanly.",
    )
});

/// Number of client-side bridges currently running. Incremented
/// at the start of `proxy_bridge`, decremented at every exit
/// path (success, `ssh` non-zero, QUIC failure). Operators
/// graph this as
///
/// ```text
/// rate(a3net_ssh_client_bridges_started_total[5m])
///   - rate(a3net_ssh_client_bridges_completed_total[5m])
/// ```
///
/// to spot leaked tasks; the gauge exposes the same number in
/// raw form for dashboards.
pub static CLIENT_BRIDGES_IN_FLIGHT: Lazy<Arc<Gauge>> = Lazy::new(|| {
    Registry::register_gauge(
        &GLOBAL,
        "a3net_ssh_client_bridges_in_flight",
        "Client-side proxy_bridge invocations currently in progress.",
    )
});

/// Force the metrics to materialise on startup. Useful as a
/// `Lazy::force` call from `main` so Prometheus scrapers see
/// the `a3net_ssh_*` series with value 0 even before the first
/// connection.
pub fn init() {
    Lazy::force(&TUNNEL_CONNECTIONS_ACCEPTED);
    Lazy::force(&TUNNEL_CONNECTIONS_FAILED);
    Lazy::force(&CLIENT_BRIDGES_STARTED);
    Lazy::force(&CLIENT_BRIDGES_COMPLETED);
    Lazy::force(&CLIENT_BRIDGES_IN_FLIGHT);
}
