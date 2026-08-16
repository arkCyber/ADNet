# a3net-iroh-interop

A3Net ↔ iroh-go / iroh-net cross-language interop harness for the
P1 deliverable in `AUDIT_GAP_FINAL.md`.

## Status (this round)

| Scenario | PR smoke | Nightly | Why |
|---|---|---|---|
| `AdnetPublishSidecarSubscribes` (A3Net → sidecar, gossip) | ✅ | ✅ | Single hop, no iroh endpoint on the A3Net side required |
| `SidecarPublishAdnetSubscribes` (sidecar → A3Net, gossip) | ✅ | ✅ | Same — the A3Net `gossip` bus publishes via the QUIC transport, the iroh wire is not on the A3Net → sidecar path in v0.1 |
| `SidecarPutAdnetFetch` (blob over iroh ALPN) | ⏸ deferred to v0.2 | ✅ | Requires a real iroh `Endpoint` on the A3Net side that dials the sidecar's endpoint id. `a3net-transport` has the adapter, but the `Node::fetch_blob` path is local-only in v0.1 (see `crates/a3net-node/src/node.rs` for the `read from local store` impl that landed in this round). |
| `AdnetPutSidecarFetch` | ⏸ deferred to v0.2 | ✅ | Symmetric of the above. |
| DHT / IPNS / docs / relay | ⏸ deferred to v0.2 | ✅ | Needs the iroh endpoint on the A3Net side + mainline DHT bootstrap, both out of scope for PR smoke. |

## Why the blob leg is deferred

The PR smoke subset covers what is *actually testable today* without
introducing a new code path inside `a3net-transport`. The blob leg
needs the iroh `Endpoint` to live inside `a3net-node` so that
`Node::fetch_blob` can fall back to a remote iroh download when the
hash is not in the local store. That work is scoped for v0.2.

The PR smoke subset is still the **first** time A3Net's gossip bus
runs against a non-A3Net peer at the wire level, which is the
high-risk surface to de-risk in this round.

## What ships in this crate

- `wire` — HTTP/JSON protocol the sidecar implements.
- `sidecar::client` — typed Rust client the harness uses to drive
  the sidecar.
- `sidecar::server` — reverse HTTP channel the sidecar dials back
  into to surface events it observed on the bus.
- `driver` — `InteropHarness` + `Scenario` + `ScenarioReport`.
- `ticket_bridge` — iroh 1.0 ↔ A3Net ticket format conversion
  (z-base-32 postcard vs `a3net-blob://`). Behind `--features iroh`.

## How to run a smoke test locally

```bash
# 1. Build the iroh-go reference sidecar (see sidecar/iroh-go-sidecar/README.md).
# 2. Then, from the repo root:
cargo run -p a3net-iroh-interop --example smoke -- \
    --sidecar ./sidecar/iroh-go-sidecar/iroh-go-sidecar \
    --scenario a3net_publish_sidecar_subscribes
```

The example prints a JSON `ScenarioReport` and exits 0 on pass, 1
on failure.

## CI integration

See `.github/workflows/iroh-interop.yml`. The PR job only runs
the gossip scenarios; the nightly job adds the blob / DHT / IPNS
scenarios (gated on `ADNET_IROH_INTEROP_FULL=1` so it can be
disabled when the iroh-go nightly breaks).
