# ADNet Architecture

This document captures the layered architecture decisions and the migration
path from the Exodus P2P CDN reference implementation.

## Goals

1. **Layered workspace** — every crate has a single responsibility and a
   clear "lower" / "higher" position.
2. **Stable types** — `adnet-types` is the only crate that other crates may
   freely depend on; nothing else is part of the public wire contract.
3. **Transport-agnostic** — the `adnet-node` orchestration never touches a
   concrete transport; it only sees the [`Transport`] trait.
4. **iroh-ready** — every layer has a clear seam for swapping the in-process
   / QUIC implementation with an `iroh-net` / `iroh-gossip` / `iroh-blobs`
   backed one. The seam is a trait for the runtime path and a feature flag
   for the iroh dependency.

## Layer overview

```
┌─────────────────────────────────────────────────────────────────┐
│ adnet-cli              # CLI demo                                │
└─────────────────────────────────────────────────────────────────┘
                                │
┌─────────────────────────────────────────────────────────────────┐
│ adnet-node             # Node orchestration                     │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  BlobStore  │  GossipBus  │  MeshHandle  │  Transport    │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
       │              │              │               │
       ▼              ▼              ▼               ▼
┌──────────────┐ ┌────────────┐ ┌──────────┐ ┌──────────────────┐
│ adnet-       │ │ adnet-     │ │ adnet-   │ │ adnet-transport  │
│ blobstore    │ │ gossip     │ │ mesh     │ │  (quinn QUIC +   │
│              │ │            │ │          │ │   future iroh)   │
└──────────────┘ └────────────┘ └──────────┘ └──────────────────┘
                       │
                       ▼
                ┌────────────────┐
                │ adnet-types    │
                │  NodeId,       │
                │  NodeAddr,     │
                │  ContentHash,  │
                │  RangeSpec,    │
                │  Ticket, Topic │
                └────────────────┘
```

## Wire-format parity with iroh

ADNet copies the iroh conventions where they make sense:

| Concept                | iroh equivalent                  | ADNet type                                  |
|------------------------|----------------------------------|---------------------------------------------|
| 32-byte content address | `iroh_base::hash::Hash`         | `adnet_types::ContentHash` (BLAKE3, 64 hex) |
| Endpoint id            | `iroh_base::node_addr::NodeId`   | `adnet_types::NodeId` (32 random bytes)     |
| Routable address       | `iroh_base::node_addr::NodeAddr` | `adnet_types::NodeAddr` (host:port + relay) |
| Single range request   | `iroh_blobs::get::RangeSpec`     | `adnet_types::RangeSpec` (`All`/`Single`/`Multi`) |
| Blob ticket            | `iroh_blobs::ticket::BlobTicket` | `adnet_types::BlobTicket` (with optional range) |
| Peer ticket            | (out of band)                    | `adnet_types::PeerTicket` (iroh-peer://…)   |
| Gossip topic           | `iroh_gossip::net::TopicId`      | `adnet_types::Topic` (BLAKE3 of label)      |

The on-the-wire formats (`adnet-blob://`, `adnet-peer://`, `adnet-room-…`
topic names) are deliberately chosen to be drop-in compatible so that an
external iroh node could parse an ADNet ticket and vice versa.

## Migration from Exodus P2P CDN

The reference Exodus `src-tauri/src/p2p_cdn` module is a monolithic module
with seven sub-files. ADNet re-shapes them into:

| Exodus file                          | ADNet location                                    | Notes |
|--------------------------------------|---------------------------------------------------|-------|
| `p2p_cdn/types.rs`                   | `crates/adnet-types/src/{content,room,ticket,topic,announce,range}.rs` | Split into focused modules; new `range.rs` for byte-range support |
| `p2p_cdn/store.rs`                   | `crates/adnet-blobstore/src/{store,chunked,traits}.rs` | Async traits added; chunked reader/writer added |
| `p2p_cdn/mesh_ticket.rs`             | `crates/adnet-types/src/ticket.rs`                | Renamed `ExodusCdnTicket` → `BlobTicket`, new `PeerTicket` + `NodeAddrTicket` added |
| `p2p_cdn/mesh_server.rs`             | `crates/adnet-mesh/src/server.rs`                 | Same wire protocol; **+ HTTP `Range:` support** |
| `p2p_cdn/mesh_fetch.rs`              | `crates/adnet-mesh/src/client.rs`                 | Renamed `fetch_from_mesh_peers` → `fetch_from_mesh`, takes `RangeSpec` |
| `p2p_cdn/gossip_bridge.rs`           | `crates/adnet-gossip/src/bridge.rs`               | Wraps `Announcement` ↔ `AnnouncementPayload` |
| `p2p_cdn/swarm.rs`                   | `crates/adnet-node/src/{node,state}.rs`           | `P2pCdnState` → `Node` + `SwarmIndex` |
| `p2p_cdn/external_gossip.rs`         | `crates/adnet-gossip/src/transport.rs`            | Replaced by pluggable `GossipTransport` trait |
| `p2p_cdn/iroh_adapter.rs`            | `crates/adnet-transport/src/iroh.rs`              | Feature-gated `iroh` backend |
| `microservice/p2p_gossip_service.rs` | `crates/adnet-gossip/src/transport.rs` (`InProcessGossip`) | The same Unix-socket bridge can be re-added as another `GossipTransport` impl |

## Where iroh fits

The `IROH_ARCHITECTURE_ANALYSIS.md` in the backup recommends a *hybrid*
architecture: keep the existing service layer but plug iroh-net, iroh-gossip,
and iroh-docs underneath. ADNet pre-figures that hybrid by:

- Making every transport call site go through a trait.
- Encoding the on-wire content addresses (BLAKE3), ticket format
  (`adnet-blob://…`), and topic format (`adnet-room-…`) to be
  drop-in-compatible with iroh's primitives.
- Reserving an `iroh` feature flag on `adnet-transport` for the future
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

- `adnet-transport::quic::tests::quic_roundtrip_with_dial_and_accept` —
  spins up a real QUIC server + client in one process, exchanges framed
  bytes end-to-end through the real `quinn` stack.
- `adnet-mesh::server::tests::mesh_serves_range_requests` — verifies
  HTTP `Range:` responses (single + multi + suffix) on the mesh server.
- `adnet-mesh::client::tests::range_fetch_single` — verifies client
  parses `206 Partial Content` and `multipart/byteranges` correctly.
- `adnet-node::examples::two_node_echo` — spawns two `Node` instances in
  one process, joins the same room, exchanges an announcement.

## Social / chat typed records

The Exodus P2P CDN + microservice split ships a full set of typed
records for group chat, direct messaging, and social feed (朋友圈).
Those were ported into `adnet-types` so they sit next to the rest of
the wire contract and can be reused from any crate (gossip overlay,
IPC services, higher-level orchestration):

| Module                                 | Records                                                                 | Notes |
|----------------------------------------|--------------------------------------------------------------------------|-------|
| `adnet_types::integrity`               | `direct_hash`, `group_hash`, `post_hash`, `verify_*`                     | SHA-256 over the canonical field order; same convention as `Exodus@src-backup/.../group_chat_service.rs` |
| `adnet_types::group_chat`              | `GroupChat`, `GroupMessage`, `GroupMember`, `GroupInvitation`, `DirectChat`, `DirectMessage`, `MessageReceipt`, `UserSequence`, `GroupSequence` | Every record has `stamp_integrity_hash` / `verify_integrity` helpers |
| `adnet_types::social_feed`             | `SocialPost`, `PostAttachment`, `SocialComment`, `SocialReaction`, `FollowRelationship` | `SocialPost::is_visible_to` covers `public` / `friends` / `private` |

The same records are wrapped in a typed Unix-socket JSON-RPC service
in `adnet-ipc::group_chat_service` so an existing Exodus Tauri
microservice can be replaced without changing clients.

## IPC layer enrichment

`adnet-ipc` was extended to support durable, multi-process operation:

- `BlobsIpcService` accepts a `data_dir: Option<PathBuf>`. When set, the
  service proxies `add_blob` / `get_blob` / `list_blobs` through
  `adnet-blobstore::BlobStore`, so blobs survive process restarts and
  are content-addressed for the rest of the workspace.
- The JSON-RPC server now enforces a 16 MiB request-line cap and the
  client reads a full newline-delimited response frame (with its own
  16 MiB response cap) — the previous implementation assumed a single
  socket read returned exactly one frame.

## Blobstore introspection

`adnet-blobstore::BlobStore` now ships three operations every
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
| `adnet-types`     | `node_id_roundtrip`, `blob_ticket_demo`, `announcement_demo` | Identity / ticket / gossip wire formats |
| `adnet-blobstore` | `round_trip`                                  | Chunked import + range + multi-chunk reads |
| `adnet-gossip`    | `two_node_publish`                            | Publish/subscribe via `InProcessGossip` |
| `adnet-mesh`      | `server_and_client`                           | HTTP whole / `Range:` / multipart fetches |
| `adnet-transport` | `quic_roundtrip`                              | Native QUIC dial + accept + framed echo |
| `adnet-node`      | `blob_share`, `two_node_echo`                 | Two-node orchestration end-to-end |
| `adnet-cli`       | `programmatic`                                | Re-use the `clap` parser from outside the binary |

Run any with `cargo run -p <crate> --example <name>`.

## Audit summary (Exodus@src-backup → ADNet)

The `Exodus@src-backup` project was audited end-to-end and the
functionality that maps cleanly onto the ADNet layering was ported
across. Anything that depended on Tauri / the desktop shell was left
in the backup; anything that is purely a typed record, transport
helper, or content-addressed store now lives in the workspace.

| Exodus module (backup)                            | Landed in ADNet                                                       | Status |
|---------------------------------------------------|----------------------------------------------------------------------|--------|
| `p2p_cdn/iroh_adapter.rs` + `IROH_ARCHITECTURE*`  | `adnet-transport::iroh` (feature-gated) + `ARCHITECTURE.md` "Where iroh fits" | Reserved seam; QUIC remains the production path |
| `p2p_cdn/storage.rs` + `p2p_cdn/chunked.rs`       | `adnet-blobstore::{store, chunked, import}` + new `list_complete` / `remove` / `total_size` / `contains` | ✅ Imported |
| `p2p_cdn/download.rs` filename sanitisation       | `adnet-blobstore::filename::safe_filename` (with tests)              | ✅ Imported |
| `microservice/p2p_blobs_service.rs`               | `adnet-ipc::blobs_service` (`BlobsIpcService` + disk-backed config)   | ✅ Imported & extended |
| `microservice/p2p_gossip_service.rs`              | `adnet-ipc::gossip_service` (`GossipIpcService`)                       | ✅ Imported |
| `microservice/group_chat_service.rs` (群聊/点对点) | `adnet-types::{group_chat, integrity}` + `adnet-ipc::group_chat_service` | ✅ Imported |
| `microservice/social_feed_service.rs` (朋友圈)    | `adnet-types::social_feed`                                            | ✅ Imported |
| `Exodus P2P CDN relay` (WAN HTTP proxy)           | `adnet-relay` (new crate)                                             | ✅ Imported (separate crate) |
| Exodus per-node shared folder workflow            | `adnet-workspace` (new crate)                                         | ✅ Imported (separate crate) |

After this audit:

- 11 crates in the workspace (`adnet-types`, `adnet-blobstore`,
  `adnet-gossip`, `adnet-mesh`, `adnet-resilience`, `adnet-transport`,
  `adnet-ipc`, `adnet-relay`, `adnet-workspace`, `adnet-node`,
  `adnet-cli`).
- 191 unit tests across the workspace, 0 failing.
- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean.

## Runtime integration layer

Beyond the per-crate audit, `adnet-node` exposes a small set of
runtime-integration seams that the layered crates do not provide on
their own. These are what turn the workspace from a collection of
parts into a single ADNet process.

| Concern | Where | What it does |
|---------|-------|--------------|
| Persistent node identity | `NodeConfig::load_or_create` | Reads `{data_dir}/node_id` if present, otherwise generates a fresh `NodeId` and writes it back. Mirrors iroh's per-process `SecretKey` persistence so tickets and gossip addresses stay stable across restarts. |
| Gossip → swarm fan-in | `Node::start_discovery` | When a room is joined, a detached task subscribes to the gossip topic and ingests every decoded announcement (from any peer) into the local `SwarmIndex`. Without this, two nodes sharing a gossip bus never see each other's content. |
| Local provide | `Node::peers_for` | If the local `BlobStore` already has the blob, the node returns a self-ticket pointing at the local mesh endpoint. Matches iroh's "local provide" behaviour and short-circuits the network round-trip. |
| Embedded relay | `NodeBuilder::with_relay_config` + `Node::ensure_relay` | Optional `adnet-relay::RelayServer` started at `build()` time (or on demand) so a node can host a public-routable HTTP mesh relay. Mirrors iroh's `RelayMode::Default`. |
| Graceful shutdown | `Node::shutdown` | Stops the mesh server, the relay (if any), leaves joined rooms, and tears down the transport. Idempotent. |
| `Cmd::Serve` SIGINT | `adnet-cli/src/main.rs` | Replaces the previous `std::future::pending()` block with a `tokio::select!` racing `ctrl_c()` so Ctrl-C actually shuts the server down. |
