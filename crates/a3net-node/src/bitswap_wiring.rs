//! Bitswap wiring glue — production instantiation of the
//! `BitswapQuicBridge` + `BitswapNetworkAdapter` pipeline.
//!
//! ## Why this module exists
//!
//! As of the 2026-08-11 audit (`AUDIT_BITSWAP_DEEP_20260811.md §3.3`),
//! `BitswapHandle` is constructed inside `a3net-node` but the runtime
//! never instantiates `BitswapQuicBridge` — only the
//! `tests/bitswap_quic_integration.rs` exercise does. In a real node
//! started with `--features bitswap`, the Bitswap engine exists, but
//! no QUIC stream is ever opened and no Bitswap message is ever
//! serialized onto the wire. The engine is effectively a no-op.
//!
//! ## What this module does
//!
//! [`wire_bitswap_to_transport`] does the exact 5-step pipeline the
//! integration tests use (see
//! `tests/bitswap_quic_integration.rs::test_quic_handshake_round_trip`):
//!
//! 1. Build a `BitswapQuicBridge` over the supplied `SharedTransport`.
//! 2. Build a `BitswapNetworkAdapter` — this adapter is the "remote
//!    API" instance that the caller hands to `BitswapHandle`.
//! 3. Use `clone_for_listen` to derive a second adapter that shares
//!    the first one's `handlers`/`pending`/`transport` `Arc`s but
//!    owns its own `rx`/`outgoing_rx` channels. Feed the second
//!    adapter's `event_tx` into the bridge's accept loop.
//! 4. Take the remote API's outgoing queue and feed it into the bridge's
//!    per-peer pump so serialized frames actually hit the wire.
//! 5. Spawn the listen-adapter's `run()` loop to dispatch inbound
//!    events into the shared handlers/pending state.
//!
//! The caller then hands the **remote API adapter** to
//! `BitswapHandle::attach_transport` so the engine's
//! `want_block_from_peer` calls actually traverse the wire and their
//! callbacks resolve via the shared `pending` map.
//!
//! ## Why a separate module
//!
//! The wiring logic is small but it needs to live somewhere reachable
//! from both `NodeBuilder::build_with_bus` (where the bridge is built)
//! and from any future code path that wants the wiring without going
//! through `Node` (e.g. an integration test that needs the join handles
//! directly). Keeping it in `bitswap.rs` would muddy that file's pure
//! handle responsibilities; keeping it in `bitswap_transport.rs` would
//! couple that file's wire-level details to the runtime. `bitswap_wiring.rs`
//! is the seam.

use std::sync::Arc;

use a3net_transport::SharedTransport;
use a3net_types::NodeId;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::bitswap_transport::{
    BitswapNetworkAdapter, BitswapQuicBridge, BitswapTransportBridge,
};

/// Handle bundle returned by [`wire_bitswap_to_transport`].
///
/// Drop the join handles to stop the accept loop, the outgoing pump, and
/// the adapter run loop. The `Arc` references inside the bridge / adapter
/// will then release naturally as the rest of the runtime drops them.
pub struct BitswapWiring {
    /// The QUIC bridge — kept alive so callers can dial peers and
    /// accept inbound Bitswap connections. Clone it for use across
    /// tasks; it's an `Arc` internally.
    pub bridge: Arc<BitswapQuicBridge>,
    /// The "remote API" network adapter — handed to
    /// `BitswapHandle::attach_transport` so the engine's
    /// `want_block_from_peer` calls actually traverse the wire and
    /// resolve via the shared `pending` map.
    pub adapter: Arc<BitswapNetworkAdapter>,
    /// Join handle for the listen-adapter's `run()` loop. Aborting it
    /// stops inbound message dispatch (the accept loop still routes
    /// new connections but their frames won't be processed).
    pub run_handle: JoinHandle<()>,
    /// Join handle for the bridge's `spawn_accept_loop`. Aborting it
    /// stops new inbound QUIC connections from being routed to Bitswap.
    pub accept_handle: JoinHandle<()>,
    /// Join handle for the bridge's `spawn_outgoing_pump`. Aborting it
    /// stops framed bytes from being delivered to outbound QUIC streams.
    pub pump_handle: JoinHandle<()>,
}

/// Build the full Bitswap QUIC pipeline against the supplied transport.
///
/// Returns the bridge, the network adapter, and the three task handles
/// that drive the live pipeline. The caller is expected to:
///
/// 1. Hand the returned `adapter` to `BitswapHandle::attach_transport`
///    so the engine's `BitswapHandle::want_block_from_peer` calls
///    actually traverse the wire.
/// 2. Keep the returned `BitswapWiring` alive (drop only at shutdown)
///    so the spawned tasks aren't aborted mid-flight.
///
/// `local_node_id` should match `transport.local_node()`; if it doesn't,
/// a `warn!` is emitted but the wiring still succeeds — operators may
/// legitimately want to bridge under a different identity than the
/// transport cert (e.g. behind a relay that rewrites NodeId).
pub fn wire_bitswap_to_transport(
    local_node_id: NodeId,
    transport: SharedTransport,
) -> BitswapWiring {
    let bridge: Arc<BitswapQuicBridge> =
        BitswapQuicBridge::new(local_node_id.clone(), transport.clone());

    if bridge.local_node_id() != transport.local_node() {
        warn!(
            "bitswap wiring: local node id {} differs from transport's {}; \
             peers will see the bitswap identity, not the transport cert",
            local_node_id.short(),
            transport.local_node().short(),
        );
    }

    let bridge_dyn: Arc<dyn BitswapTransportBridge> = bridge.clone();
    // "Remote API" adapter — this is what the caller attaches to
    // `BitswapHandle`. We then derive a listen-adapter via
    // `clone_for_listen` so the dispatcher and the callers share
    // `handlers` + `pending` + `transport` state.
    let (remote_api, _remote_event_tx) =
        BitswapNetworkAdapter::new(local_node_id.clone(), bridge_dyn);
    let (listen_adapter, listen_event_tx) = remote_api.clone_for_listen();

    // Wire the remote API's outbound queue into the bridge's per-peer
    // pump so serialized frames actually hit the QUIC stream.
    let mut remote_api_mut = remote_api;
    let outgoing_rx = remote_api_mut
        .take_outgoing()
        .expect("BitswapNetworkAdapter::take_outgoing must be called exactly once");
    let remote_api = Arc::new(remote_api_mut);

    let pump_handle = bridge.clone().spawn_outgoing_pump(outgoing_rx);
    let accept_handle = bridge.clone().spawn_accept_loop(listen_event_tx);
    let run_handle = tokio::spawn(async move {
        listen_adapter.run().await;
        debug!("bitswap adapter run loop exited");
    });

    info!(
        "bitswap wiring active: node={} alpn=a3net/bitswap/1",
        local_node_id.short()
    );

    BitswapWiring {
        bridge,
        adapter: remote_api,
        run_handle,
        accept_handle,
        pump_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_transport::{QuicTransport, QuicTransportBuilder};

    /// Building the wiring against an ephemeral QUIC transport must
    /// succeed without panicking. We don't assert on actual byte
    /// exchange here — the deep test lives in
    /// `tests/bitswap_quic_integration.rs`. This test exists to
    /// catch future refactors that break the wiring module's API.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wire_against_ephemeral_transport_succeeds() {
        let transport = QuicTransportBuilder::new(NodeId::random(), "127.0.0.1:0".parse().unwrap())
            .build()
            .expect("build quic transport");
        let id = transport.local_node_id().clone();
        let transport = Arc::new(transport) as SharedTransport;

        let wiring = wire_bitswap_to_transport(id.clone(), transport);
        assert_eq!(wiring.bridge.local_node_id(), &id);
        // Abort the wiring tasks so the test process can exit cleanly.
        wiring.run_handle.abort();
        wiring.accept_handle.abort();
        wiring.pump_handle.abort();
    }
}
