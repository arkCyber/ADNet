# ADNet

**ADNet** is a Rust workspace that re-implements the iroh-flavoured P2P CDN
building blocks from `Exodus@src-backup` as a clean, composable crate family.

The goal: a layered, well-tested cargo workspace where every crate has a
single responsibility and the integration points for iroh's
`iroh-net` / `iroh-gossip` / `iroh-blobs` crates are reserved behind clear
traits and feature flags.

## Workspace layout

```
crates/
├── adnet-types      # NodeId, NodeAddr, ContentHash, RangeSpec, Ticket, Topic, Announcement
├── adnet-blobstore  # BLAKE3 chunked blob store (iroh-blobs layout)
├── adnet-gossip     # Topic-based pub/sub (iroh-gossip parity)
├── adnet-mesh       # HTTP fallback transport (serve + parallel fetch, supports Range:)
├── adnet-transport  # QUIC backend (quinn) + future iroh-net adapter
├── adnet-node       # Node orchestration (store + bus + transport + mesh)
└── adnet-cli        # `adnet` command-line demo
```

## iroh backend (opt-in, `--features iroh`)

ADNet ships two parallel backends for transport / gossip / blob storage:

| Layer       | Default backend                       | `--features iroh` backend                                       |
|-------------|---------------------------------------|-----------------------------------------------------------------|
| Transport   | `quinn` + `rustls` (`QuicTransport`)  | `iroh::Endpoint` (`IrohTransport`) — QUIC w/ NAT traversal + DERP relay |
| Gossip      | In-process broadcast                  | `iroh-gossip` (`IrohGossipTransport`) — HyParView + PlumTree    |
| Blob store  | Disk-backed directory layout          | `iroh-blobs::store::fs::FsStore` — Bao-verified streams         |
| Runtime     | `adnet-node::Node`                    | `adnet-node::IrohRuntime` — wires `iroh::Router` (blobs + gossip ALPN) |

The `iroh` feature cascades: `adnet-node --features iroh` turns it on
in `adnet-transport`, `adnet-gossip`, and `adnet-blobstore`. You can
also enable it on a single crate (e.g. `cargo test -p adnet-transport
--features iroh`).

### What you gain with `--features iroh`

- **NAT traversal**: rendezvous / hole-punching via iroh's relay
  (`iroh-relay`, DERP protocol) — no manual port forwarding.
- **Pkarr address discovery**: signed DNS records (`pkarr`) let peers
  publish a stable address that resolves to current direct + relay
  endpoints.
- **Bao-verified blob transfer**: every byte streamed from a peer is
  hash-checked against the BLAKE3 ticket, so partial / corrupt downloads
  are detected at the application layer.
- **HyParView + PlumTree gossip**: a proper P2P pub/sub overlay instead
  of in-process broadcast.
- **ALPN multiplexing**: `iroh::Router` accepts `iroh_blobs::ALPN` and
  `iroh_gossip::ALPN` over a single QUIC connection — no separate ports.

### Build-time cost

Enabling `--features iroh` pulls in `iroh`, `iroh-base`, `iroh-blobs`,
`iroh-gossip`, `iroh-relay`, `iroh-docs`, and `pkarr` (roughly 150+
transitive crates, several heavy `aws-lc-rs` / `quinn` / `rustls` paths).
The workspace now pins `rust-version = "1.91"` and `edition = "2024"` to
match `iroh 1.0.x`. Expect:

- **First cold build**: ~5–10 min on a modern laptop (depends on
  network for crate downloads and toolchain upgrade).
- **Incremental rebuilds**: a few seconds for trait-only edits, tens
  of seconds for changes inside `iroh.rs` / `iroh_runtime.rs`.
- **Test compile**: `cargo test --features iroh -p adnet-transport`
  pays the full cost on the first run; subsequent runs are cheap.

If you only want the default backend, **don't** pass `--features iroh`
— the default build (`cargo build --workspace`) stays around the
~1 min cold build.

### Quick try

```bash
# Default (no iroh) — pure quinn-based transport
cargo build --workspace
cargo test  --workspace

# With iroh backend — adds NAT traversal, DERP, Bao-verified blobs
cargo build --workspace --features iroh
cargo test  --workspace --features iroh

# Compile-check the iroh-only modules without rebuilding everything
cargo check -p adnet-node --features iroh
```

## Crate dependency graph

```
adnet-cli ──▶ adnet-node
              │
              ├─▶ adnet-types
              ├─▶ adnet-blobstore ──▶ adnet-types
              ├─▶ adnet-gossip    ──▶ adnet-types
              ├─▶ adnet-mesh      ──▶ adnet-types, adnet-blobstore
              └─▶ adnet-transport ──▶ adnet-types, adnet-blobstore
```

Strictly downward — no cycles, no horizontal coupling.

## Build

```bash
cargo build --workspace
cargo test  --workspace
cargo run   -p adnet-cli -- --help
```

## CLI quickstart

```bash
# Generate a node id and data dir
adnet --data-dir /tmp/adnet-demo init

# Start the mesh HTTP server in the foreground
adnet --data-dir /tmp/adnet-demo serve

# Import a local file and announce it into "lobby"
adnet --data-dir /tmp/adnet-demo announce \
    --room lobby \
    --file ./README.md \
    --title "ADNet README" \
    --kind article

# Print the room feed (assets + peer sources)
adnet --data-dir /tmp/adnet-demo feed --room lobby
```

## What works today (v0.2.0 milestone)

- **Real native QUIC** in `adnet-transport`: a working `quinn` + `rustls`
  + `rcgen` server/client that:
  - generates an ephemeral self-signed cert and derives the local `NodeId`
    from it (same construction as iroh),
  - opens connections to peers in the in-process registry,
  - exchanges length-prefixed [`Frame`]s in both directions.
  The transport sits behind the `Transport` trait so callers in
  `adnet-node` can be wired against it without conditional compilation.
- **HTTP `Range:` support** in `adnet-mesh` for both client and server.
  Single-range (206) and multi-range (multipart/byteranges) requests are
  parsed and served. The client strips multipart framing and returns a
  flat bytes stream so callers always see the bytes they asked for.
- **Range-aware tickets** in `adnet-types`: `BlobTicket` carries an
  optional [`RangeSpec`] so peers can advertise sub-ranges of large
  blobs (header + chunk-tail patterns common in CDN work).
- **NodeAddr routing**: `Endpoint` + `RelayUrl` + `NodeAddr` parsing
  mirrors iroh's printable form (`<id> direct=<host:port> relay=<url>`).

## Where iroh plugs in

| Backup crate             | New crate            | iroh integration point                          |
|--------------------------|----------------------|-------------------------------------------------|
| `src-tauri/src/p2p_cdn`  | `adnet-blobstore`, `adnet-mesh`, `adnet-transport` | `iroh-blobs::BlobTicket` is parsed by [`adnet_types::BlobTicket`] |
| `src-tauri/src/p2p_cdn/gossip_bridge.rs` | `adnet-gossip::bridge` | Replaced by `iroh-gossip` behind [`adnet_gossip::GossipTransport`] |
| `src-tauri/src/microservice/p2p_gossip_service.rs` | `adnet-gossip::InProcessGossip` | Future `iroh-net::Gossip` behind the same trait |
| `src-tauri/src/p2p_cdn/iroh_adapter.rs` | `adnet-transport::iroh::IrohTransport` | Feature-gated `iroh` backend (see `crates/adnet-transport/src/iroh.rs`) |

## Test count

`cargo test --workspace` currently runs **56 tests**:

```
adnet-types        28 tests
adnet-blobstore     7 tests
adnet-gossip        4 tests
adnet-mesh          5 tests
adnet-transport     9 tests
adnet-node          3 tests
```

## Examples

Every crate ships a runnable example that demonstrates its main
capability:

```bash
# adnet-types — identity / ticket / announcement wire formats
cargo run -p adnet-types --example node_id_roundtrip
cargo run -p adnet-types --example blob_ticket_demo
cargo run -p adnet-types --example announcement_demo

# adnet-blobstore — chunked round-trip with range reads
cargo run -p adnet-blobstore --example round_trip

# adnet-gossip — two-node publish/subscribe over InProcessGossip
cargo run -p adnet-gossip --example two_node_publish

# adnet-mesh — HTTP mesh server + client (whole, Range:, multi-range)
cargo run -p adnet-mesh --example server_and_client

# adnet-transport — native QUIC roundtrip with framed messages
cargo run -p adnet-transport --example quic_roundtrip

# adnet-node — multi-node blob sharing / gossip echo
cargo run -p adnet-node --example blob_share
cargo run -p adnet-node --example two_node_echo

# adnet-cli — programmatic use of the clap parser
cargo run -p adnet-cli --example programmatic
```

## License

Dual-licensed under MIT OR Apache-2.0, at your option.