# ADNet

**ADNet** is a Rust workspace that re-implements the iroh-flavoured P2P CDN
building blocks from `Exodus@src-backup` as a clean, composable crate family,
then layers a full **AI + Web3** application stack (encrypted chat, agent
runtime, SSH/SmartHome/Wallet, identity & token, …) on top of the same P2P
plumbing.

The goal: a layered, well-tested cargo workspace where every crate has a
single responsibility and the integration points for iroh's
`iroh-net` / `iroh-gossip` / `iroh-blobs` crates are reserved behind clear
traits and feature flags — so the same crates power both a CLI daemon, a
Tauri desktop app, an FFI surface (Swift/Kotlin/Python/JS), and a Flutter
mobile client.

> **Current milestone:** v0.3 — 70+ crates, 4 `a3chat-*` business crates,
> a `chat.v1` agent loop, FFI bindings, Tauri desktop, Flutter mobile.

---

## Table of contents

- [Workspace layout](#workspace-layout)
- [iroh backend (opt-in)](#iroh-backend-opt-in--features-iroh)
- [Crate dependency graph](#crate-dependency-graph)
- [Build](#build)
- [CLI features](#cli-features)
- [a3chat: encrypted chat service](#a3chat-encrypted-chat-service)
- [adnet-agent: AI agent runtime](#adnet-agent-ai-agent-runtime)
- [FFI / Mobile / Desktop surfaces](#ffi--mobile--desktop-surfaces)
- [Aerospace-grade engineering](#aerospace-grade-engineering)
- [What works today (v0.3 milestone)](#what-works-today-v03-milestone)
- [Where iroh plugs in](#where-iroh-plugs-in)
- [Test count](#test-count)
- [Examples](#examples)
- [License](#license)

---

## Workspace layout

74 crates, organised in concentric rings. Lower rings are stable; higher rings
add business / product features.

```
crates/
├── ── Core types & errors ───────────────────────────────────────────
│   ├── adnet-types            # NodeId, NodeAddr, ContentHash, RangeSpec, Ticket, Topic, Announcement, NodeIdentity
│   ├── adnet-error            # Workspace-wide error taxonomy
│   ├── adnet-crypto           # AEAD, KDF, key store (used by a3chat)
│   ├── adnet-identity         # Long-term identity + NodeIdentityCard
│   ├── adnet-token            # Capability tokens (billing / wallet)
│   └── adnet-observability    # Tracing + Prometheus + OpenTelemetry
│
├── ── P2P transport & storage ──────────────────────────────────────
│   ├── adnet-blobstore        # BLAKE3 chunked blob store (iroh-blobs layout)
│   ├── adnet-gossip           # Topic-based pub/sub (iroh-gossip parity)
│   ├── adnet-dht              # Kademlia-style DHT + namespace records
│   ├── adnet-namespace        # Mutable namespace records on top of DHT
│   ├── adnet-mesh             # HTTP fallback transport (Range:, parallel chunks)
│   ├── adnet-mesh-coordinator # Closed-mesh admission + coordination
│   ├── adnet-mesh-firewall    # Mesh-side policy enforcement
│   ├── adnet-transport        # QUIC backend (quinn) + iroh adapter (feature)
│   ├── adnet-relay            # DERP-style relay + session guards
│   ├── adnet-nat-traversal    # STUN / NAT-PMP / PCP / pkarr discovery
│   ├── adnet-magicdns         # MagicDNS name resolution
│   └── adnet-dns-server       # Authoritative DNS + pkarr publish
│
├── ── Node runtime & IPC ───────────────────────────────────────────
│   ├── adnet-node             # Node orchestration (store + bus + transport + mesh)
│   ├── adnet-ipc              # IPC core (irpc + axum)
│   ├── adnet-ipc-adapter      # IPC bridge (Unix socket, HTTP-RPC, noise)
│   ├── adnet-rpc              # JSON-RPC dispatcher for adnet-cli
│   ├── adnet-rpc-irpc         # irpc-style RPC codegen + runtime
│   ├── adnet-resilience       # Retry / CircuitBreaker / Cancellation / ResourceLimiter
│   ├── adnet-workspace        # Multi-crate virtual workspace glue
│   ├── adnet-gateway          # HTTP gateway (CDN edge)
│   └── adnet-security         # Capability / ACL / audit primitives
│
├── ── Application domain crates ─────────────────────────────────────
│   ├── adnet-roster           # Contact directory management
│   ├── adnet-chatstore        # iroh-docs backed encrypted message store
│   ├── adnet-share            # P2P file sharing via tickets
│   ├── adnet-socialfeed       # Social feed (likes / comments / reactions)
│   ├── adnet-news             # News feed + subscriptions
│   ├── adnet-userstore        # User profile store
│   ├── adnet-mail             # SMTP / IMAP + P2P mail bridge
│   ├── adnet-pairing          # Device pairing + trusted credentials
│   ├── adnet-moderation       # Blocklist / takedown / defend-mode
│   ├── adnet-reputation       # Global peer reputation (PeerScore)
│   ├── adnet-model-catalog    # Model catalog + provider discovery
│   ├── adnet-webhook          # Webhook endpoint management
│   ├── adnet-invite           # Email invitation rendering
│   ├── adnet-qr               # QR payload handling
│   ├── adnet-news             # News feed
│   └── adnet-media            # Media metadata + thumbnails
│
├── ── AI + Web3 product stack ──────────────────────────────────────
│   ├── adnet-agent            # Agent runtime (chat.v1), providers, tools
│   ├── adnet-eliza-bridge     # ElizaOS-compatible bridge
│   ├── adnet-chain            # On-chain state + indexer
│   ├── adnet-wallet-evm       # EVM wallet (ethers-rs)
│   └── adnet-token            # Capability / payment tokens
│
├── ── Networking surfaces ──────────────────────────────────────────
│   ├── adnet-webrtc           # webrtc-rs P2P transport (SCTP DataChannel)
│   ├── adnet-webtransport     # HTTP/3 WebTransport (wtransport)
│   ├── adnet-tun              # TUN device mesh
│   ├── adnet-exit-node        # Exit-node policy + traffic relay
│   ├── adnet-ssh              # SSH-over-P2P server / client
│   ├── adnet-webdav           # WebDAV server over the mesh
│   ├── adnet-smarthome        # SmartHome HUB + device drivers
│   └── adnet-vless-client     # VLESS / Xray-compatible client
│
├── ── Encrypted chat family (a3chat) ───────────────────────────────
│   ├── a3chat-core            # Domain types + JSON-Schema export
│   ├── a3chat-crypto          # Noise_XX DMs + Sender-Key groups (Signal-style)
│   ├── a3chat-rpc             # JSON-RPC 2.0 server + SSE notifications
│   └── a3chat-app             # Service layer: chat / contact / group / sync / presence
│
├── ── Frontends & bindings ─────────────────────────────────────────
│   ├── adnet-cli              # `adnet` command-line daemon
│   ├── adnet-tui              # Terminal UI (ratatui)
│   ├── adnet-tauri            # Tauri desktop shell
│   ├── adnet-ffi              # UniFFI C-ABI surface (Swift / Kotlin / Python)
│   ├── adnet-ffi-js           # WASM / JS bindings via wasm-bindgen
│   └── adnet-iroh-interop     # Interop sidecar for iroh-only tools
│
└── ── Testing / operations ─────────────────────────────────────────
    ├── adnet-bench                # Criterion + per-crate benches
    ├── adnet-simulator            # Network simulator (latency / loss)
    ├── adnet-fuzz                 # cargo-fuzz harnesses
    ├── adnet-integration-tests    # Cross-crate integration tests
    ├── adnet-chaos                # Chaos engineering (failover / partition)
    ├── adnet-database             # SQLite migrations + connection pool
    └── adnet-verify               # Invariant / property test exports
```

---

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
The workspace pins `rust-version = "1.91"` and `edition = "2024"` to
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

---

## Crate dependency graph

```
adnet-cli ──▶ adnet-node
              │
              ├─▶ adnet-types              ◀── adnet-identity
              ├─▶ adnet-blobstore          ◀── a3chat-app
              ├─▶ adnet-gossip                (encrypted chat store)
              ├─▶ adnet-mesh               ◀── adnet-crypto
              ├─▶ adnet-transport             ◀── a3chat-crypto
              ├─▶ adnet-reputation         ◀── adnet-token
              ├─▶ adnet-pairing            ◀── adnet-resilience
              ├─▶ adnet-chatstore          ◀── adnet-observability
              ├─▶ adnet-moderation
              ├─▶ adnet-dht
              ├─▶ adnet-relay
              ├─▶ adnet-ffi (Swift / Kotlin / Python / JS)
              └─▶ adnet-agent              ──▶ adnet-eliza-bridge
                                              ──▶ adnet-chain
                                              ──▶ adnet-wallet-evm
```

Strictly downward — no cycles, no horizontal coupling. Each crate is
either **leaf** (depends only on types / errors) or **layered** on top
of a stable base.

---

## Build

```bash
# Default workspace build
cargo build --workspace
cargo test  --workspace

# Iroh backend (NAT traversal, DERP, Bao verification)
cargo build --workspace --features iroh
cargo test  --workspace --features iroh

# Single crate examples
cargo run -p adnet-cli -- --help
cargo run -p adnet-tui  --example tui_demo
cargo run -p adnet-agent --example chat_loop

# Run the iroh-interop sidecar
cargo run -p adnet-iroh-interop -- sidecar --help

# Toolchain
rustup toolchain install 1.91   # pinned by rust-toolchain.toml
```

`rust-toolchain.toml` pins Rust **1.91** + edition **2024** (required by
`iroh 1.0.x`). If you build with `--features iroh`, expect the first
cold compile to take 5–10 minutes; the default build stays under a
minute on a modern laptop.

---

## CLI features

The `adnet` binary (`adnet-cli`) is the canonical demo of the workspace.

### Core commands

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

### Daemon & control plane

```bash
adnet daemon                  # background daemon mode (Unix socket)
adnet daemon start|stop|status
adnet doctor                  # health-check the local node
adnet control <subcommand>     # control-plane operations
```

### MFS (Mutable File System)

```bash
adnet files mkdir /my-dir
adnet files mkdir /my-dir/subdir --parents
adnet files ls /
adnet files cp /source/path /dest/path
adnet files mv /old/path /new/path
adnet files write /my-dir/hello.txt "Hello World"
adnet files read /my-dir/hello.txt
adnet files rm /my-dir/file.txt
adnet files rm /my-dir --recursive
adnet files stat /my-dir/hello.txt
```

### Pubsub (Publish/Subscribe)

```bash
adnet pubsub ls
adnet pubsub peers my-topic
adnet pubsub sub my-topic
adnet pubsub pub my-topic "Hello, world!"
```

### Key management (IPNS)

```bash
adnet key gen my-key
adnet key list
adnet key rm my-key
adnet key rename old-name new-name
adnet key export my-key --output my-key.json
adnet key import my-key --input my-key.json
adnet name publish /ipfs/Qm...
adnet name resolve /ipns/...
adnet name local
```

### Reputation system

```bash
adnet reputation show
adnet reputation get <peer-id>
adnet reputation adjust <peer-id> 10.0
adnet reputation reset <peer-id>
adnet reputation stats
```

### Content moderation

```bash
adnet moderation block <cid> --reason copyright
adnet moderation erase <cid> --reason csam
adnet moderation list --active
adnet moderation defend-on
adnet moderation defend-off
```

### Device pairing

```bash
adnet pair create --wallet-private /path/to/wallet.key
adnet pair list
adnet pair revoke <credential-id>
```

### Agent (chat.v1 loop)

```bash
adnet agent run --provider hermes --prompt "summarise this room"
adnet agent tool mail.send --to alice --subject "hi"
adnet agent audit --last 50
```

### Discovery / network

```bash
adnet bootstrap ls|add|rm
adnet discover [--mdns]
adnet dns publish|resolve
adnet relay status|reset
adnet swarm peers|connect|disconnect
```

### Observability

```bash
adnet stats                       # Prometheus scrape
adnet log tail [--filter adnet]
adnet bench                       # criterion harness launcher
```

### Additional commands

| Command       | Description                                                  |
|---------------|--------------------------------------------------------------|
| `bitswap`     | Bitswap protocol operations (want, ls, stat, ledger, cancel) |
| `channel`     | Gossip-based information channels                            |
| `dht`         | DHT peer lookups                                             |
| `routing`     | DHT routing operations                                       |
| `swarm`       | libp2p-style connection management                           |
| `bootstrap`   | Bootstrap peer list management                               |
| `storage`     | Storage scope management                                     |
| `profile`     | Node profile management                                      |
| `roster`      | Contact directory                                            |
| `user`        | User profile management                                      |
| `news`        | News feed operations                                         |
| `moments`     | Moments/stories operations                                   |
| `mdns`        | mDNS discovery                                               |
| `webhook`     | Webhook endpoint management                                  |
| `invite`      | Email invitation rendering                                   |
| `qr`          | QR payload handling                                          |
| `mesh`        | Closed-mesh admission                                        |
| `webrtc`      | WebRTC peer offer/answer                                     |
| `webtransport`| WebTransport session lifecycle                               |
| `video`       | Real-time video stream (H.264/VP8/VP9)                       |
| `ssh`         | SSH-over-P2P server / client                                 |
| `webdav`      | WebDAV mount / proxy                                         |
| `wallet`      | EVM wallet (sign / send / balance)                           |
| `chain`       | On-chain state read                                          |
| `smart`       | SmartHome hub operations                                     |

---

## a3chat: encrypted chat service

The `a3chat-*` family is the first **product** built on the ADNet P2P
plumbing. Four crates, strictly layered:

```
┌──────────────────────────────────────────────────────────────┐
│ a3chat-app          Service layer: Chat / Contact / Group /  │
│                     Sync / Presence + SQLite persistence     │
├──────────────────────────────────────────────────────────────┤
│ a3chat-rpc          JSON-RPC 2.0 server (axum) + SSE push   │
├──────────────────────────────────────────────────────────────┤
│ a3chat-core         Domain types, JSON-Schema export, RPC    │
│                     method constants, validation             │
├──────────────────────────────────────────────────────────────┤
│ a3chat-crypto       Noise_XX DM sessions + Signal-style      │
│                     Sender-Key group crypto + Argon2id KEK   │
└──────────────────────────────────────────────────────────────┘
              │           │                │
              ▼           ▼                ▼
       adnet-chatstore  adnet-crypto    adnet-types
```

### Endpoints (a3chat-rpc)

| Method                        | Purpose                                  |
|-------------------------------|------------------------------------------|
| `chat.conversation.list`      | List DM + group conversations            |
| `chat.conversation.open`      | Open or create a conversation            |
| `chat.message.send`           | Send a (possibly E2E encrypted) message  |
| `chat.message.recall`         | Recall a sent message within the window  |
| `chat.message.ack`            | Acknowledge a received message           |
| `contact.list` / `.add_request` / `.accept_request` / `.block` / `.unblock` | Contact graph |
| `group.create` / `.invite` / `.member.add` / `.member.remove` / `.member.role` | Group management |
| `chat.sync.snapshot` / `.delta` / `.compressed` | Multi-device sync |
| `presence.publish` / `.subscribe` | Online / Away / Offline / Invisible      |

Server-Sent Events: `GET /rpc/stream` multiplexes
`chat.message.received`, `presence.changed`, `group.member.*` from the
internal `NotificationBus` to authenticated owners.

### Crypto properties

- **DMs**: Noise_XX handshake → 32-byte session key → ChaCha20-Poly1305
  AEAD per message (independent nonce).
- **Groups**: Signal-style **Sender Keys** (one chain key per group).
  When a member leaves, the owner rotates the chain.
- **Cross-device**: Argon2id derives a KEK → ChaCha20-Poly1305
  encrypts the private key + Sender-Key chain.
- **Zero on drop**: all key material implements `zeroize::ZeroizeOnDrop`.

### Cross-platform

The same JSON-Schema export drives:

- `crates/a3chat-rpc`           → Rust server (axum)
- `crates/adnet-tauri`          → Tauri desktop shell
- `mobile/`                     → Flutter mobile client

The protocol is **shape-stable**: `snake_case` fields, `chrono::DateTime<Utc>`
timestamps, opaque ciphertext blobs (`algorithm + nonce + ciphertext + tag`).

---

## adnet-agent: AI agent runtime

`adnet-agent` is a chat-style agent loop with tool calling, an audit log,
and a pluggable provider system.

### Providers

| Provider    | Status     | Notes                                                    |
|-------------|------------|----------------------------------------------------------|
| `hermes`    | Stable     | Local Hermes HTTP endpoint (self-hosted LLM)             |
| `mock`      | Stable     | Deterministic provider for tests                         |

### Built-in tools

- `mail.send` / `mail.read` — `adnet-mail` operations
- `roster.search` — `adnet-roster` lookup
- `chat.history` — `a3chat-app` query
- `fs.read` / `fs.write` — sandboxed file access
- `wallet.balance` — `adnet-wallet-evm` read
- `chain.read` — `adnet-chain` indexer query

### Loop

```
user ──▶ agent.run ──▶ provider.complete ──▶ tool.execute* ──▶ provider.complete ──▶ final
                          │                       │
                          ▼                       ▼
                      audit log            audit log (with redaction)
```

Every tool call goes through `audit.rs` which records the prompt,
tool name, input/output, and timestamps in a tamper-evident log.

---

## FFI / Mobile / Desktop surfaces

| Surface      | Crate                | Targets                                       |
|--------------|----------------------|-----------------------------------------------|
| UniFFI C-ABI | `adnet-ffi`          | Swift (iOS / macOS), Kotlin (Android), Python |
| WASM         | `adnet-ffi-js`       | Browser / Node.js                             |
| Tauri        | `adnet-tauri`        | Desktop (Windows / macOS / Linux)             |
| Flutter      | `mobile/`            | iOS / Android (uses `a3chat-core` schema)     |
| TUI          | `adnet-tui`          | Terminal (ratatui)                            |
| iroh-interop | `adnet-iroh-interop` | Sidecar for iroh-only toolchains              |

All FFI bindings are generated from the same `a3chat-core` JSON
Schema, so adding a field on the server automatically propagates to
every client after regeneration.

---

## Aerospace-grade engineering

ADNet targets **aerospace-grade** reliability, modelled on DO-178C
guidance for safety-critical software.

- **DO-178C traceability**: key functions carry traceability markers
  in source comments linking back to design docs (`docs/`).
- **Fault tolerance**: pre-GC snapshots, idempotent operations,
  retry + circuit breaker on every remote call.
- **Type safety**: `unsafe_code = "forbid"` workspace-wide,
  comprehensive use of the type system (`NewType`, validated
  builders, sum-type state machines).
- **Error handling**: typed errors via `thiserror`; every public
  function returns `Result<T, TypedError>` with stable variants.
- **Cancellation scope** (`adnet-resilience::CancellationScope`):
  every long-running background task observes a token, and
  `Node::shutdown()` performs a bounded join + force-abort.
- **Resource limiter** (`adnet-resilience::ResourceLimiter`):
  per-peer / per-room / per-tag concurrency caps to keep one bad
  peer from saturating the node.

### Resilience primitives (`adnet-resilience`)

| Primitive        | Purpose                                                    |
|------------------|------------------------------------------------------------|
| `RetryPolicy`    | `None` / `Conservative` / `Aggressive` presets             |
| `retry_with_backoff` | Exponential backoff with jitter + error classification  |
| `CircuitBreaker` | 3-state (`Closed` / `Open` / `HalfOpen`) per failure rate  |
| `CancellationScope` | Coordinated shutdown of background tasks               |
| `ResourceLimiter<K>` | Global + per-key concurrency caps                       |
| `ResilientHttpClient` | All of the above for `reqwest::Client`                 |

### Observability

- **Tracing** via `tracing` + `tracing-subscriber` (env-filter).
- **OpenTelemetry** OTLP exporter (tonic / http-proto) for
  distributed traces.
- **Prometheus** scrapable metrics endpoint (`adnet stats`).
- **Structured panic** handler (`adnet-observability::panic`).

---

## What works today (v0.3 milestone)

### P2P / transport

- **Real native QUIC** in `adnet-transport` (quinn + rustls + rcgen).
  Ephemeral self-signed certs, length-prefixed [`Frame`] exchanges
  behind the `Transport` trait.
- **HTTP `Range:` support** in `adnet-mesh` (single + multi-range,
  multipart/byteranges, parallel chunk fetcher).
- **Range-aware tickets** in `adnet-types` (`BlobTicket` +
  `RangeSpec`).
- **NodeAddr routing** mirrors iroh's printable form
  (`<id> direct=<host:port> relay=<url>`).
- **iroh backend** (`--features iroh`): NAT traversal, DERP relay,
  Bao-verified blobs, ALPN multiplexing.

### Storage & data

- **BLAKE3 chunked blob store** with range reads + per-blob
  metadata-only `BaoTree`.
- **iroh-docs backed message sync** for `adnet-chatstore`
  (Phase 5a).
- **SQLite persistence** for `a3chat-app::ChatStorage`
  (conversations, messages, group state).

### Security & identity

- **Global peer reputation** (`adnet-reputation`): events from
  Bitswap / Gossipsub / Pairing / Chat, decay loop, JSONL log,
  Prometheus metrics.
- **Content moderation**: hash blocklist, takedown service,
  defend mode, audit trail.
- **Device pairing**: trusted credentials + revocation.
- **Long-term identity** (`adnet-identity`): `NodeIdentityCard`,
  pkarr publishing.
- **Capability tokens** (`adnet-token`): billing & authz.

### Application services

- **MFS** (IPFS-compatible mutable file system).
- **Pubsub** messaging over the gossip layer.
- **Contacts / Roster / Profile / User / News / Moments**.
- **Social feed** (likes, comments, reactions).
- **Mail** (SMTP / IMAP + P2P mail bridge).
- **WebDAV** server over the mesh.
- **SSH-over-P2P** server + client.
- **SmartHome** hub + drivers.
- **TUN mesh** + **exit-node** policy.
- **WebRTC** + **WebTransport** (feature-gated).
- **VLESS** / Xray-compatible client.

### Encrypted chat (a3chat)

- Noise_XX DM sessions + Signal-style Sender Keys for groups.
- Argon2id KEK for cross-device key wrapping.
- JSON-RPC 2.0 server + SSE notifications.
- Sync service for multi-device delta + snapshot.

### AI agent runtime

- Chat.v1 loop with tool calling + audit log.
- Hermes (local) + mock providers.
- Cross-surface: Tauri desktop, CLI, FFI, Flutter mobile.

---

## Where iroh plugs in

| Backup crate                                       | New crate                                       | iroh integration point                                              |
|----------------------------------------------------|-------------------------------------------------|---------------------------------------------------------------------|
| `src-tauri/src/p2p_cdn`                            | `adnet-blobstore`, `adnet-mesh`, `adnet-transport` | `iroh-blobs::BlobTicket` is parsed by [`adnet_types::BlobTicket`]    |
| `src-tauri/src/p2p_cdn/gossip_bridge.rs`           | `adnet-gossip::bridge`                          | Replaced by `iroh-gossip` behind [`adnet_gossip::GossipTransport`]   |
| `src-tauri/src/microservice/p2p_gossip_service.rs` | `adnet-gossip::InProcessGossip`                 | Future `iroh-net::Gossip` behind the same trait                       |
| `src-tauri/src/p2p_cdn/iroh_adapter.rs`            | `adnet-transport::iroh::IrohTransport`          | Feature-gated `iroh` backend (see `crates/adnet-transport/src/iroh.rs`) |
| `src-tauri/src/chatstore`                          | `adnet-chatstore`                               | `iroh-docs` provides the message sync substrate (Phase 5a)             |

---

## Test count

`cargo test --workspace` runs **3,500+ tests** across 74 crates:

```
adnet-types              unit + property tests
adnet-blobstore          unit + proptest (bitswap invariants)
adnet-gossip             unit + chaos tests
adnet-mesh               unit + integration
adnet-transport          unit + iroh e2e (feature-gated)
adnet-node               unit + multi-transport integration
adnet-dht                unit + aerospace-grade chaos
adnet-namespace          unit + comprehensive
adnet-relay              unit + ActiveSessionGuard tests
adnet-ipc-adapter        unit + integration (noise handshake)
adnet-resilience         unit (retry / breaker / cancellation / limiter)
adnet-observability      unit + structured panic
adnet-agent              unit + provider mock
adnet-cli                unit + daemon smoke + ipc-client tests
adnet-roster             unit
adnet-chatstore          unit + iroh-docs sync
adnet-reputation         unit + decay loop
adnet-moderation         unit + integration
a3chat-core              unit + JSON-Schema export
a3chat-crypto            unit + Noise_XX round-trip + Sender-Key chain
a3chat-rpc               unit + dispatch + SSE
a3chat-app               unit + service-level
…
```

`cargo test --workspace --features iroh` additionally runs the iroh
backend test suite (transport + blobs + gossip + docs).

---

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

# a3chat — end-to-end encrypted DM round-trip
cargo run -p a3chat-app --example dm_roundtrip

# adnet-agent — provider + tool call
cargo run -p adnet-agent --example chat_loop

# adnet-tui — terminal UI demo
cargo run -p adnet-tui --example tui_demo
```

---

## License

Dual-licensed under **MIT OR Apache-2.0**, at your option.

See `LICENSE-MIT` and `LICENSE-APACHE` at the repository root, or
[choose at your option](https://opensource.org/licenses/MIT).