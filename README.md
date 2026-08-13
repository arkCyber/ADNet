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
├── adnet-types          # NodeId, NodeAddr, ContentHash, RangeSpec, Ticket, Topic, Announcement
├── adnet-blobstore      # BLAKE3 chunked blob store (iroh-blobs layout)
├── adnet-gossip         # Topic-based pub/sub (iroh-gossip parity)
├── adnet-mesh           # HTTP fallback transport (serve + parallel fetch, supports Range:)
├── adnet-transport      # QUIC backend (quinn) + future iroh-net adapter
├── adnet-node           # Node orchestration (store + bus + transport + mesh)
├── adnet-cli            # `adnet` command-line demo
├── adnet-reputation     # Global peer reputation (PeerScore) system
├── adnet-pairing        # Device pairing with trusted credentials
├── adnet-chatstore     # Encrypted chat with trust management
├── adnet-moderation    # Content moderation and takedown service
├── adnet-model-catalog  # Model catalog with provider discovery
├── adnet-roster        # Contact directory management
├── adnet-share         # P2P file sharing via tickets
├── adnet-socialfeed    # Social feed with likes and comments
├── adnet-news          # News feed with subscriptions
├── adnet-userstore     # User profile management
├── adnet-webhook       # Webhook endpoint management
├── adnet-invite        # Email invitation rendering
├── adnet-qr           # QR payload handling
├── adnet-mesh-coordinator  # Mesh coordinator
├── adnet-mesh-firewall # Mesh firewall
└── adnet-video         # Real-time video streaming (H.264/VP8/VP9)
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
              ├─▶ adnet-transport ──▶ adnet-types, adnet-blobstore
              ├─▶ adnet-reputation
              ├─▶ adnet-pairing
              ├─▶ adnet-chatstore
              └─▶ adnet-moderation
```

Strictly downward — no cycles, no horizontal coupling.

## Build

```bash
cargo build --workspace
cargo test  --workspace
cargo run   -p adnet-cli -- --help
```

## CLI Features

### Core Commands

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

### MFS (Mutable File System)

```bash
# Create directories
adnet files mkdir /my-dir
adnet files mkdir /my-dir/subdir --parents

# List directory contents
adnet files ls /
adnet files ls /my-dir

# Copy and move files
adnet files cp /source/path /dest/path
adnet files mv /old/path /new/path

# Write and read files
adnet files write /my-dir/hello.txt "Hello World"
adnet files read /my-dir/hello.txt

# Remove files (use --recursive for directories)
adnet files rm /my-dir/file.txt
adnet files rm /my-dir --recursive

# File statistics
adnet files stat /my-dir/hello.txt
```

### Pubsub (Publish/Subscribe)

```bash
# List subscribed topics
adnet pubsub ls

# List peers subscribed to a topic
adnet pubsub peers my-topic

# Subscribe to a topic
adnet pubsub sub my-topic

# Publish a message
adnet pubsub pub my-topic "Hello, world!"
```

### Key Management (IPNS)

```bash
# Generate a new Ed25519 key pair
adnet key gen my-key

# List all keys
adnet key list

# Remove a key
adnet key rm my-key

# Rename a key
adnet key rename old-name new-name

# Export/Import keys
adnet key export my-key --output my-key.json
adnet key import my-key --input my-key.json
```

### IPNS Name Management

```bash
# Publish an IPNS name
adnet name publish /ipfs/Qm...

# Resolve an IPNS name
adnet name resolve /ipns/...

# List local IPNS names
adnet name local
```

### Reputation System

```bash
# Show all peer reputations
adnet reputation show

# Get specific peer score
adnet reputation get <peer-id>

# Adjust peer score manually
adnet reputation adjust <peer-id> 10.0

# Reset peer score
adnet reputation reset <peer-id>

# Show reputation statistics
adnet reputation stats
```

### Content Moderation

```bash
# Block content
adnet moderation block <cid> --reason copyright

# Takedown (block + delete)
adnet moderation erase <cid> --reason csam

# List blocklist
adnet moderation list --active

# Defend mode (deny-by-default)
adnet moderation defend-on
adnet moderation defend-off
```

### Device Pairing

```bash
# Create pairing invitation
adnet pair create --wallet-private /path/to/wallet.key

# List trusted devices
adnet pair list

# Revoke a device
adnet pair revoke <credential-id>
```

### Additional Commands

| Command | Description |
|---------|-------------|
| `bitswap` | Bitswap protocol operations (want, ls, stat, ledger, cancel) |
| `channel` | Gossip-based information channels |
| `dht` | DHT peer lookups |
| `routing` | DHT routing operations |
| `swarm` | libp2p-style connection management |
| `bootstrap` | Bootstrap peer list management |
| `storage` | Storage scope management |
| `profile` | Node profile management |
| `roster` | Contact directory |
| `user` | User profile management |
| `news` | News feed operations |
| `moments` | Moments/stories operations |
| `mdns` | mDNS discovery |
| `webhook` | Webhook endpoint management |
| `invite` | Email invitation rendering |
| `qr` | QR payload handling |
| `mesh` | Closed-mesh admission |

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
- **Global Peer Reputation**: Cross-subsystem reputation scoring with
  decay, persistence, and Prometheus metrics.
- **Content Moderation**: Blocklist, takedown service, and defend mode.
- **Device Pairing**: Trusted device credentials with revocation.
- **MFS Operations**: IPFS-compatible mutable file system.
- **Pubsub Messaging**: Topic-based publish/subscribe.

## Where iroh plugs in

| Backup crate             | New crate            | iroh integration point                          |
|--------------------------|----------------------|-------------------------------------------------|
| `src-tauri/src/p2p_cdn`  | `adnet-blobstore`, `adnet-mesh`, `adnet-transport` | `iroh-blobs::BlobTicket` is parsed by [`adnet_types::BlobTicket`] |
| `src-tauri/src/p2p_cdn/gossip_bridge.rs` | `adnet-gossip::bridge` | Replaced by `iroh-gossip` behind [`adnet_gossip::GossipTransport`] |
| `src-tauri/src/microservice/p2p_gossip_service.rs` | `adnet-gossip::InProcessGossip` | Future `iroh-net::Gossip` behind the same trait |
| `src-tauri/src/p2p_cdn/iroh_adapter.rs` | `adnet-transport::iroh::IrohTransport` | Feature-gated `iroh` backend (see `crates/adnet-transport/src/iroh.rs`) |

## Test count

`cargo test --workspace` currently runs **420+ tests**:

```
adnet-types          tests
adnet-blobstore      tests
adnet-gossip         tests
adnet-mesh           tests
adnet-transport      tests
adnet-node           tests
adnet-cli            420 tests
adnet-reputation     tests
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

# adnet-cli — CLI parser and all commands
cargo run -p adnet-cli --example cli_parser
```

## Architecture Highlights

### Aerospace-Grade Standards

- **DO-178C Traceability**: Key functions include traceability markers
- **Fault Tolerance**: Pre-GC snapshots, idempotent operations
- **Type Safety**: Comprehensive use of Rust's type system
- **Error Handling**: Typed errors with `thiserror`

### Reputation System

The `adnet-reputation` crate provides a unified reputation abstraction:

- **Event Types**: Bitswap, Gossipsub, Pairing, Chat
- **Scoring**: Thread-safe, sharded peer score table
- **Decay Loop**: Configurable decay toward zero
- **Persistence**: JSONL append log + state snapshots
- **Metrics**: Prometheus integration

### Content Moderation

- **Blocklist**: Hash-based content blocking with expiry
- **Takedown Service**: Block + delete workflow
- **Defend Mode**: Deny-by-default switch
- **Audit Trail**: All actions logged and revocable

## License

Dual-licensed under MIT OR Apache-2.0, at your option.
