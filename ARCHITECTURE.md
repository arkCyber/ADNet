# A3Net Architecture

This document captures the layered architecture decisions and the migration
path from the Exodus P2P CDN reference implementation.

## Goals

1. **Layered workspace** — every crate has a single responsibility and a
   clear "lower" / "higher" position.
2. **Stable types** — `a3net-types` is the only crate that other crates may
   freely depend on; nothing else is part of the public wire contract.
3. **Transport-agnostic** — the `a3net-node` orchestration never touches a
   concrete transport; it only sees the [`Transport`] trait.
4. **iroh-ready** — every layer has a clear seam for swapping the in-process
   / QUIC implementation with an `iroh-net` / `iroh-gossip` / `iroh-blobs`
   backed one. The seam is a trait for the runtime path and a feature flag
   for the iroh dependency.

## Layer overview

```
┌─────────────────────────────────────────────────────────────────┐
│ a3net-cli              # CLI demo                                │
└─────────────────────────────────────────────────────────────────┘
                                │
┌─────────────────────────────────────────────────────────────────┐
│ a3net-node             # Node orchestration                     │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  BlobStore  │  GossipBus  │  MeshHandle  │  Transport    │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
       │              │              │               │
       ▼              ▼              ▼               ▼
┌──────────────┐ ┌────────────┐ ┌──────────┐ ┌──────────────────┐
│ a3net-       │ │ a3net-     │ │ a3net-   │ │ a3net-transport  │
│ blobstore    │ │ gossip     │ │ mesh     │ │  (quinn QUIC +   │
│              │ │            │ │          │ │   future iroh)   │
└──────────────┘ └────────────┘ └──────────┘ └──────────────────┘
                       │
                       ▼
                ┌────────────────┐
                │ a3net-types    │
                │  NodeId,       │
                │  NodeAddr,     │
                │  ContentHash,  │
                │  RangeSpec,    │
                │  Ticket, Topic │
                └────────────────┘
```

## Wire-format parity with iroh

A3Net copies the iroh conventions where they make sense:

| Concept                | iroh equivalent                  | A3Net type                                  |
|------------------------|----------------------------------|---------------------------------------------|
| 32-byte content address | `iroh_base::hash::Hash`         | `a3net_types::ContentHash` (BLAKE3, 64 hex) |
| Endpoint id            | `iroh_base::node_addr::NodeId`   | `a3net_types::NodeId` (32 random bytes)     |
| Routable address       | `iroh_base::node_addr::NodeAddr` | `a3net_types::NodeAddr` (host:port + relay) |
| Single range request   | `iroh_blobs::get::RangeSpec`     | `a3net_types::RangeSpec` (`All`/`Single`/`Multi`) |
| Blob ticket            | `iroh_blobs::ticket::BlobTicket` | `a3net_types::BlobTicket` (with optional range) |
| Peer ticket            | (out of band)                    | `a3net_types::PeerTicket` (iroh-peer://…)   |
| Gossip topic           | `iroh_gossip::net::TopicId`      | `a3net_types::Topic` (BLAKE3 of label)      |

The on-the-wire formats (`a3net-blob://`, `a3net-peer://`, `a3net-room-…`
topic names) are deliberately chosen to be drop-in compatible so that an
external iroh node could parse an A3Net ticket and vice versa.

## Migration from Exodus P2P CDN

The reference Exodus `src-tauri/src/p2p_cdn` module is a monolithic module
with seven sub-files. A3Net re-shapes them into:

| Exodus file                          | A3Net location                                    | Notes |
|--------------------------------------|---------------------------------------------------|-------|
| `p2p_cdn/types.rs`                   | `crates/a3net-types/src/{content,room,ticket,topic,announce,range}.rs` | Split into focused modules; new `range.rs` for byte-range support |
| `p2p_cdn/store.rs`                   | `crates/a3net-blobstore/src/{store,chunked,traits}.rs` | Async traits added; chunked reader/writer added |
| `p2p_cdn/mesh_ticket.rs`             | `crates/a3net-types/src/ticket.rs`                | Renamed `ExodusCdnTicket` → `BlobTicket`, new `PeerTicket` + `NodeAddrTicket` added |
| `p2p_cdn/mesh_server.rs`             | `crates/a3net-mesh/src/server.rs`                 | Same wire protocol; **+ HTTP `Range:` support** |
| `p2p_cdn/mesh_fetch.rs`              | `crates/a3net-mesh/src/client.rs`                 | Renamed `fetch_from_mesh_peers` → `fetch_from_mesh`, takes `RangeSpec` |
| `p2p_cdn/gossip_bridge.rs`           | `crates/a3net-gossip/src/bridge.rs`               | Wraps `Announcement` ↔ `AnnouncementPayload` |
| `p2p_cdn/swarm.rs`                   | `crates/a3net-node/src/{node,state}.rs`           | `P2pCdnState` → `Node` + `SwarmIndex` |
| `p2p_cdn/external_gossip.rs`         | `crates/a3net-gossip/src/transport.rs`            | Replaced by pluggable `GossipTransport` trait |
| `p2p_cdn/iroh_adapter.rs`            | `crates/a3net-transport/src/iroh.rs`              | Feature-gated `iroh` backend |
| `microservice/p2p_gossip_service.rs` | `crates/a3net-gossip/src/transport.rs` (`InProcessGossip`) | The same Unix-socket bridge can be re-added as another `GossipTransport` impl |

## Where iroh fits

The `IROH_ARCHITECTURE_ANALYSIS.md` in the backup recommends a *hybrid*
architecture: keep the existing service layer but plug iroh-net, iroh-gossip,
and iroh-docs underneath. A3Net pre-figures that hybrid by:

- Making every transport call site go through a trait.
- Encoding the on-wire content addresses (BLAKE3), ticket format
  (`a3net-blob://…`), and topic format (`a3net-room-…`) to be
  drop-in-compatible with iroh's primitives.
- Reserving an `iroh` feature flag on `a3net-transport` for the future
  `IrohTransport` adapter.

## Transport decision matrix

| Path                   | Today                                              | When to switch to iroh |
|------------------------|----------------------------------------------------|------------------------|
| Primary                | `QuicTransport` (quinn + rustls + rcgen)           | When iroh-net matures on macOS / Linux |
| Fallback (LAN)         | `MeshServer` HTTP API (`Range:` supported)         | Keep as fallback; iroh-net covers NAT traversal |
| Discovery              | `GossipBus` over `InProcessGossip`                 | Switch to `iroh_gossip::net::Gossip` via the `GossipTransport` trait |

## Testing strategy

Every crate has its own unit tests in `#[cfg(test)] mod tests`. Integration
tests live alongside the binary / crate. The workspace `cargo test` target
should pass on macOS and Linux without external dependencies.

Notable integration tests:

- `a3net-transport::quic::tests::quic_roundtrip_with_dial_and_accept` —
  spins up a real QUIC server + client in one process, exchanges framed
  bytes end-to-end through the real `quinn` stack.
- `a3net-mesh::server::tests::mesh_serves_range_requests` — verifies
  HTTP `Range:` responses (single + multi + suffix) on the mesh server.
- `a3net-mesh::client::tests::range_fetch_single` — verifies client
  parses `206 Partial Content` and `multipart/byteranges` correctly.
- `a3net-node::examples::two_node_echo` — spawns two `Node` instances in
  one process, joins the same room, exchanges an announcement.

## Social / chat typed records

The Exodus P2P CDN + microservice split ships a full set of typed
records for group chat, direct messaging, and social feed (朋友圈).
Those were ported into `a3net-types` so they sit next to the rest of
the wire contract and can be reused from any crate (gossip overlay,
IPC services, higher-level orchestration):

| Module                                 | Records                                                                 | Notes |
|----------------------------------------|--------------------------------------------------------------------------|-------|
| `a3net_types::integrity`               | `direct_hash`, `group_hash`, `post_hash`, `verify_*`                     | SHA-256 over the canonical field order; same convention as `Exodus@src-backup/.../group_chat_service.rs` |
| `a3net_types::group_chat`              | `GroupChat`, `GroupMessage`, `GroupMember`, `GroupInvitation`, `DirectChat`, `DirectMessage`, `MessageReceipt`, `UserSequence`, `GroupSequence` | Every record has `stamp_integrity_hash` / `verify_integrity` helpers |
| `a3net_types::social_feed`             | `SocialPost`, `PostAttachment`, `SocialComment`, `SocialReaction`, `FollowRelationship` | `SocialPost::is_visible_to` covers `public` / `friends` / `private` |

The same records are wrapped in a typed Unix-socket JSON-RPC service
in `a3net-ipc::group_chat_service` so an existing Exodus Tauri
microservice can be replaced without changing clients.

## IPC layer enrichment

`a3net-ipc` was extended to support durable, multi-process operation:

- `BlobsIpcService` accepts a `data_dir: Option<PathBuf>`. When set, the
  service proxies `add_blob` / `get_blob` / `list_blobs` through
  `a3net-blobstore::BlobStore`, so blobs survive process restarts and
  are content-addressed for the rest of the workspace.
- The JSON-RPC server now enforces a 16 MiB request-line cap and the
  client reads a full newline-delimited response frame (with its own
  16 MiB response cap) — the previous implementation assumed a single
  socket read returned exactly one frame.

## Blobstore introspection

`a3net-blobstore::BlobStore` now ships three operations every
content-addressed store ends up needing:

- `list_complete() -> Vec<ContentHash>` — enumerate every fully-imported
  blob, skipping `.importing-<hash>` staging directories.
- `remove(&ContentHash) -> bool` — drop a blob; refuses to remove a
  partial / unverified entry.
- `total_size() -> u64` — sum of `sizeBytes` across every complete blob
  (for cache-eviction and disk-usage UI).

## Runnable examples

Every crate ships at least one runnable example that exercises the
public API in a way users would actually write it:

| Crate             | Example                                       | Demonstrates |
|-------------------|-----------------------------------------------|--------------|
| `a3net-types`     | `node_id_roundtrip`, `blob_ticket_demo`, `announcement_demo` | Identity / ticket / gossip wire formats |
| `a3net-blobstore` | `round_trip`                                  | Chunked import + range + multi-chunk reads |
| `a3net-gossip`    | `two_node_publish`                            | Publish/subscribe via `InProcessGossip` |
| `a3net-mesh`      | `server_and_client`                           | HTTP whole / `Range:` / multipart fetches |
| `a3net-transport` | `quic_roundtrip`                              | Native QUIC dial + accept + framed echo |
| `a3net-node`      | `blob_share`, `two_node_echo`                 | Two-node orchestration end-to-end |
| `a3net-cli`       | `programmatic`                                | Re-use the `clap` parser from outside the binary |

Run any with `cargo run -p <crate> --example <name>`.

## Audit summary (Exodus@src-backup → A3Net)

The `Exodus@src-backup` project was audited end-to-end and the
functionality that maps cleanly onto the A3Net layering was ported
across. Anything that depended on Tauri / the desktop shell was left
in the backup; anything that is purely a typed record, transport
helper, or content-addressed store now lives in the workspace.

| Exodus module (backup)                            | Landed in A3Net                                                       | Status |
|---------------------------------------------------|----------------------------------------------------------------------|--------|
| `p2p_cdn/iroh_adapter.rs` + `IROH_ARCHITECTURE*`  | `a3net-transport::iroh` (feature-gated) + `ARCHITECTURE.md` "Where iroh fits" | Reserved seam; QUIC remains the production path |
| `p2p_cdn/storage.rs` + `p2p_cdn/chunked.rs`       | `a3net-blobstore::{store, chunked, import}` + new `list_complete` / `remove` / `total_size` / `contains` | ✅ Imported |
| `p2p_cdn/download.rs` filename sanitisation       | `a3net-blobstore::filename::safe_filename` (with tests)              | ✅ Imported |
| `microservice/p2p_blobs_service.rs`               | `a3net-ipc::blobs_service` (`BlobsIpcService` + disk-backed config)   | ✅ Imported & extended |
| `microservice/p2p_gossip_service.rs`              | `a3net-ipc::gossip_service` (`GossipIpcService`)                       | ✅ Imported |
| `microservice/group_chat_service.rs` (群聊/点对点) | `a3net-types::{group_chat, integrity}` + `a3net-ipc::group_chat_service` | ✅ Imported |
| `microservice/social_feed_service.rs` (朋友圈)    | `a3net-types::social_feed`                                            | ✅ Imported |
| `Exodus P2P CDN relay` (WAN HTTP proxy)           | `a3net-relay` (new crate)                                             | ✅ Imported (separate crate) |
| Exodus per-node shared folder workflow            | `a3net-workspace` (new crate)                                         | ✅ Imported (separate crate) |

After this audit:

- 11 crates in the workspace (`a3net-types`, `a3net-blobstore`,
  `a3net-gossip`, `a3net-mesh`, `a3net-resilience`, `a3net-transport`,
  `a3net-ipc`, `a3net-relay`, `a3net-workspace`, `a3net-node`,
  `a3net-cli`).
- 191 unit tests across the workspace, 0 failing.
- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean.

## Runtime integration layer

Beyond the per-crate audit, `a3net-node` exposes a small set of
runtime-integration seams that the layered crates do not provide on
their own. These are what turn the workspace from a collection of
parts into a single A3Net process.

| Concern | Where | What it does |
|---------|-------|--------------|
| Persistent node identity | `NodeConfig::load_or_create` | Reads `{data_dir}/node_id` if present, otherwise generates a fresh `NodeId` and writes it back. Mirrors iroh's per-process `SecretKey` persistence so tickets and gossip addresses stay stable across restarts. |
| Gossip → swarm fan-in | `Node::start_discovery` | When a room is joined, a detached task subscribes to the gossip topic and ingests every decoded announcement (from any peer) into the local `SwarmIndex`. Without this, two nodes sharing a gossip bus never see each other's content. |
| Local provide | `Node::peers_for` | If the local `BlobStore` already has the blob, the node returns a self-ticket pointing at the local mesh endpoint. Matches iroh's "local provide" behaviour and short-circuits the network round-trip. |
| Embedded relay | `NodeBuilder::with_relay_config` + `Node::ensure_relay` | Optional `a3net-relay::RelayServer` started at `build()` time (or on demand) so a node can host a public-routable HTTP mesh relay. Mirrors iroh's `RelayMode::Default`. |
| Graceful shutdown | `Node::shutdown` | Stops the mesh server, the relay (if any), leaves joined rooms, and tears down the transport. Idempotent. |
| `Cmd::Serve` SIGINT | `a3net-cli/src/main.rs` | Replaces the previous `std::future::pending()` block with a `tokio::select!` racing `ctrl_c()` so Ctrl-C actually shuts the server down. |
