# A3Net

**A3Net** is a Rust workspace that re-implements the iroh-flavoured P2P CDN
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
- [a3net-agent: AI agent runtime](#a3net-agent-ai-agent-runtime)
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
│   ├── a3net-types            # NodeId, NodeAddr, ContentHash, RangeSpec, Ticket, Topic, Announcement, NodeIdentity
│   ├── a3net-error            # Workspace-wide error taxonomy
│   ├── a3net-crypto           # AEAD, KDF, key store (used by a3chat)
│   ├── a3net-identity         # Long-term identity + NodeIdentityCard
│   ├── a3net-token            # Capability tokens (billing / wallet)
│   └── a3net-observability    # Tracing + Prometheus + OpenTelemetry
│
├── ── P2P transport & storage ──────────────────────────────────────
│   ├── a3net-blobstore        # BLAKE3 chunked blob store (iroh-blobs layout)
│   ├── a3net-gossip           # Topic-based pub/sub (iroh-gossip parity)
│   ├── a3net-dht              # Kademlia-style DHT + namespace records
│   ├── a3net-namespace        # Mutable namespace records on top of DHT
│   ├── a3net-mesh             # HTTP fallback transport (Range:, parallel chunks)
│   ├── a3net-mesh-coordinator # Closed-mesh admission + coordination
│   ├── a3net-mesh-firewall    # Mesh-side policy enforcement
│   ├── a3net-transport        # QUIC backend (quinn) + iroh adapter (feature)
│   ├── a3net-relay            # DERP-style relay + session guards
│   ├── a3net-nat-traversal    # STUN / NAT-PMP / PCP / pkarr discovery
│   ├── a3net-magicdns         # MagicDNS name resolution
│   └── a3net-dns-server       # Authoritative DNS + pkarr publish
│
├── ── Node runtime & IPC ───────────────────────────────────────────
│   ├── a3net-node             # Node orchestration (store + bus + transport + mesh)
│   ├── a3net-ipc              # IPC core (irpc + axum)
│   ├── a3net-ipc-adapter      # IPC bridge (Unix socket, HTTP-RPC, noise)
│   ├── a3net-rpc              # JSON-RPC dispatcher for a3net-cli
│   ├── a3net-rpc-irpc         # irpc-style RPC codegen + runtime
│   ├── a3net-resilience       # Retry / CircuitBreaker / Cancellation / ResourceLimiter
│   ├── a3net-workspace        # Multi-crate virtual workspace glue
│   ├── a3net-gateway          # HTTP gateway (CDN edge)
│   └── a3net-security         # Capability / ACL / audit primitives
│
├── ── Application domain crates ─────────────────────────────────────
│   ├── a3net-roster           # Contact directory management
│   ├── a3net-chatstore        # iroh-docs backed encrypted message store
│   ├── a3net-share            # P2P file sharing via tickets
│   ├── a3net-socialfeed       # Social feed (likes / comments / reactions)
│   ├── a3net-news             # News feed + subscriptions
│   ├── a3net-userstore        # User profile store
│   ├── a3net-mail             # SMTP / IMAP + P2P mail bridge
│   ├── a3net-pairing          # Device pairing + trusted credentials
│   ├── a3net-moderation       # Blocklist / takedown / defend-mode
│   ├── a3net-reputation       # Global peer reputation (PeerScore)
│   ├── a3net-model-catalog    # Model catalog + provider discovery
│   ├── a3net-webhook          # Webhook endpoint management
│   ├── a3net-invite           # Email invitation rendering
│   ├── a3net-qr               # QR payload handling
│   ├── a3net-news             # News feed
│   └── a3net-media            # Media metadata + thumbnails
│
├── ── AI + Web3 product stack ──────────────────────────────────────
│   ├── a3net-agent            # Agent runtime (chat.v1), providers, tools
│   ├── a3net-eliza-bridge     # ElizaOS-compatible bridge
│   ├── a3net-chain            # On-chain state + indexer
│   ├── a3net-wallet-evm       # EVM wallet (ethers-rs)
│   └── a3net-token            # Capability / payment tokens
│
├── ── Networking surfaces ──────────────────────────────────────────
│   ├── a3net-webrtc           # webrtc-rs P2P transport (SCTP DataChannel)
│   ├── a3net-webtransport     # HTTP/3 WebTransport (wtransport)
│   ├── a3net-tun              # TUN device mesh
│   ├── a3net-exit-node        # Exit-node policy + traffic relay
│   ├── a3net-ssh              # SSH-over-P2P server / client
│   ├── a3net-webdav           # WebDAV server over the mesh
│   ├── a3net-smarthome        # SmartHome HUB + device drivers
│   └── a3net-vless-client     # VLESS / Xray-compatible client
│
├── ── Encrypted chat family (a3chat) ───────────────────────────────
│   ├── a3chat-core            # Domain types + JSON-Schema export
│   ├── a3chat-crypto          # Noise_XX DMs + Sender-Key groups (Signal-style)
│   ├── a3chat-rpc             # JSON-RPC 2.0 server + SSE notifications
│   └── a3chat-app             # Service layer: chat / contact / group / sync / presence
│
├── ── Frontends & bindings ─────────────────────────────────────────
│   ├── a3net-cli              # `a3net` command-line daemon
│   ├── a3net-tui              # Terminal UI (ratatui)
│   ├── a3net-tauri            # Tauri desktop shell
│   ├── a3net-ffi              # UniFFI C-ABI surface (Swift / Kotlin / Python)
│   ├── a3net-ffi-js           # WASM / JS bindings via wasm-bindgen
│   └── a3net-iroh-interop     # Interop sidecar for iroh-only tools
│
└── ── Testing / operations ─────────────────────────────────────────
    ├── a3net-bench                # Criterion + per-crate benches
    ├── a3net-simulator            # Network simulator (latency / loss)
    ├── a3net-fuzz                 # cargo-fuzz harnesses
    ├── a3net-integration-tests    # Cross-crate integration tests
    ├── a3net-chaos                # Chaos engineering (failover / partition)
    ├── a3net-database             # SQLite migrations + connection pool
    └── a3net-verify               # Invariant / property test exports
```

---

## iroh backend (opt-in, `--features iroh`)

A3Net ships two parallel backends for transport / gossip / blob storage:

| Layer       | Default backend                       | `--features iroh` backend                                       |
|-------------|---------------------------------------|-----------------------------------------------------------------|
| Transport   | `quinn` + `rustls` (`QuicTransport`)  | `iroh::Endpoint` (`IrohTransport`) — QUIC w/ NAT traversal + DERP relay |
| Gossip      | In-process broadcast                  | `iroh-gossip` (`IrohGossipTransport`) — HyParView + PlumTree    |
| Blob store  | Disk-backed directory layout          | `iroh-blobs::store::fs::FsStore` — Bao-verified streams         |
| Runtime     | `a3net-node::Node`                    | `a3net-node::IrohRuntime` — wires `iroh::Router` (blobs + gossip ALPN) |

The `iroh` feature cascades: `a3net-node --features iroh` turns it on
in `a3net-transport`, `a3net-gossip`, and `a3net-blobstore`. You can
also enable it on a single crate (e.g. `cargo test -p a3net-transport
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
- **Test compile**: `cargo test --features iroh -p a3net-transport`
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
cargo check -p a3net-node --features iroh
```

---

## Crate dependency graph

```
a3net-cli ──▶ a3net-node
              │
              ├─▶ a3net-types              ◀── a3net-identity
              ├─▶ a3net-blobstore          ◀── a3chat-app
              ├─▶ a3net-gossip                (encrypted chat store)
              ├─▶ a3net-mesh               ◀── a3net-crypto
              ├─▶ a3net-transport             ◀── a3chat-crypto
              ├─▶ a3net-reputation         ◀── a3net-token
              ├─▶ a3net-pairing            ◀── a3net-resilience
              ├─▶ a3net-chatstore          ◀── a3net-observability
              ├─▶ a3net-moderation
              ├─▶ a3net-dht
              ├─▶ a3net-relay
              ├─▶ a3net-ffi (Swift / Kotlin / Python / JS)
              └─▶ a3net-agent              ──▶ a3net-eliza-bridge
                                              ──▶ a3net-chain
                                              ──▶ a3net-wallet-evm
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
cargo run -p a3net-cli -- --help
cargo run -p a3net-tui  --example tui_demo
cargo run -p a3net-agent --example chat_loop

# Run the iroh-interop sidecar
cargo run -p a3net-iroh-interop -- sidecar --help

# Toolchain
rustup toolchain install 1.91   # pinned by rust-toolchain.toml
```

`rust-toolchain.toml` pins Rust **1.91** + edition **2024** (required by
`iroh 1.0.x`). If you build with `--features iroh`, expect the first
cold compile to take 5–10 minutes; the default build stays under a
minute on a modern laptop.

---

## CLI features

The `a3net` binary (`a3net-cli`) is the canonical demo of the workspace.

### Core commands

```bash
# Generate a node id and data dir
a3net --data-dir /tmp/a3net-demo init

# Start the mesh HTTP server in the foreground
a3net --data-dir /tmp/a3net-demo serve

# Import a local file and announce it into "lobby"
a3net --data-dir /tmp/a3net-demo announce \
    --room lobby \
    --file ./README.md \
    --title "A3Net README" \
    --kind article

# Print the room feed (assets + peer sources)
a3net --data-dir /tmp/a3net-demo feed --room lobby
```

### Daemon & control plane

```bash
a3net daemon                  # background daemon mode (Unix socket)
a3net daemon start|stop|status
a3net doctor                  # health-check the local node
a3net control <subcommand>     # control-plane operations
```

### MFS (Mutable File System)

```bash
a3net files mkdir /my-dir
a3net files mkdir /my-dir/subdir --parents
a3net files ls /
a3net files cp /source/path /dest/path
a3net files mv /old/path /new/path
a3net files write /my-dir/hello.txt "Hello World"
a3net files read /my-dir/hello.txt
a3net files rm /my-dir/file.txt
a3net files rm /my-dir --recursive
a3net files stat /my-dir/hello.txt
```

### Pubsub (Publish/Subscribe)

```bash
a3net pubsub ls
a3net pubsub peers my-topic
a3net pubsub sub my-topic
a3net pubsub pub my-topic "Hello, world!"
```

### Key management (IPNS)

```bash
a3net key gen my-key
a3net key list
a3net key rm my-key
a3net key rename old-name new-name
a3net key export my-key --output my-key.json
a3net key import my-key --input my-key.json
a3net name publish /ipfs/Qm...
a3net name resolve /ipns/...
a3net name local
```

### Reputation system

```bash
a3net reputation show
a3net reputation get <peer-id>
a3net reputation adjust <peer-id> 10.0
a3net reputation reset <peer-id>
a3net reputation stats
```

### Content moderation

```bash
a3net moderation block <cid> --reason copyright
a3net moderation erase <cid> --reason csam
a3net moderation list --active
a3net moderation defend-on
a3net moderation defend-off
```

### Device pairing

```bash
a3net pair create --wallet-private /path/to/wallet.key
a3net pair list
a3net pair revoke <credential-id>
```

### Agent (chat.v1 loop)

```bash
a3net agent run --provider hermes --prompt "summarise this room"
a3net agent tool mail.send --to alice --subject "hi"
a3net agent audit --last 50
```

### Discovery / network

```bash
a3net bootstrap ls|add|rm
a3net discover [--mdns]
a3net dns publish|resolve
a3net relay status|reset
a3net swarm peers|connect|disconnect
```

### Observability

```bash
a3net stats                       # Prometheus scrape
a3net log tail [--filter a3net]
a3net bench                       # criterion harness launcher
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

The `a3chat-*` family is the first **product** built on the A3Net P2P
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
       a3net-chatstore  a3net-crypto    a3net-types
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
- `crates/a3net-tauri`          → Tauri desktop shell
- `mobile/`                     → Flutter mobile client

The protocol is **shape-stable**: `snake_case` fields, `chrono::DateTime<Utc>`
timestamps, opaque ciphertext blobs (`algorithm + nonce + ciphertext + tag`).

---

## a3net-agent: AI agent runtime

`a3net-agent` is a chat-style agent loop with tool calling, an audit log,
and a pluggable provider system.

### Providers

| Provider    | Status     | Notes                                                    |
|-------------|------------|----------------------------------------------------------|
| `hermes`    | Stable     | Local Hermes HTTP endpoint (self-hosted LLM)             |
| `mock`      | Stable     | Deterministic provider for tests                         |

### Built-in tools

- `mail.send` / `mail.read` — `a3net-mail` operations
- `roster.search` — `a3net-roster` lookup
- `chat.history` — `a3chat-app` query
- `fs.read` / `fs.write` — sandboxed file access
- `wallet.balance` — `a3net-wallet-evm` read
- `chain.read` — `a3net-chain` indexer query

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
| UniFFI C-ABI | `a3net-ffi`          | Swift (iOS / macOS), Kotlin (Android), Python |
| WASM         | `a3net-ffi-js`       | Browser / Node.js                             |
| Tauri        | `a3net-tauri`        | Desktop (Windows / macOS / Linux)             |
| Flutter      | `mobile/`            | iOS / Android (uses `a3chat-core` schema)     |
| TUI          | `a3net-tui`          | Terminal (ratatui)                            |
| iroh-interop | `a3net-iroh-interop` | Sidecar for iroh-only toolchains              |

All FFI bindings are generated from the same `a3chat-core` JSON
Schema, so adding a field on the server automatically propagates to
every client after regeneration.

---

## Aerospace-grade engineering

A3Net targets **aerospace-grade** reliability, modelled on DO-178C
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
- **Cancellation scope** (`a3net-resilience::CancellationScope`):
  every long-running background task observes a token, and
  `Node::shutdown()` performs a bounded join + force-abort.
- **Resource limiter** (`a3net-resilience::ResourceLimiter`):
  per-peer / per-room / per-tag concurrency caps to keep one bad
  peer from saturating the node.

### Resilience primitives (`a3net-resilience`)

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
- **Prometheus** scrapable metrics endpoint (`a3net stats`).
- **Structured panic** handler (`a3net-observability::panic`).

---

## What works today (v0.3 milestone)

### P2P / transport

- **Real native QUIC** in `a3net-transport` (quinn + rustls + rcgen).
  Ephemeral self-signed certs, length-prefixed [`Frame`] exchanges
  behind the `Transport` trait.
- **HTTP `Range:` support** in `a3net-mesh` (single + multi-range,
  multipart/byteranges, parallel chunk fetcher).
- **Range-aware tickets** in `a3net-types` (`BlobTicket` +
  `RangeSpec`).
- **NodeAddr routing** mirrors iroh's printable form
  (`<id> direct=<host:port> relay=<url>`).
- **iroh backend** (`--features iroh`): NAT traversal, DERP relay,
  Bao-verified blobs, ALPN multiplexing.

### Storage & data

- **BLAKE3 chunked blob store** with range reads + per-blob
  metadata-only `BaoTree`.
- **iroh-docs backed message sync** for `a3net-chatstore`
  (Phase 5a).
- **SQLite persistence** for `a3chat-app::ChatStorage`
  (conversations, messages, group state).

### Security & identity

- **Global peer reputation** (`a3net-reputation`): events from
  Bitswap / Gossipsub / Pairing / Chat, decay loop, JSONL log,
  Prometheus metrics.
- **Content moderation**: hash blocklist, takedown service,
  defend mode, audit trail.
- **Device pairing**: trusted credentials + revocation.
- **Long-term identity** (`a3net-identity`): `NodeIdentityCard`,
  pkarr publishing.
- **Capability tokens** (`a3net-token`): billing & authz.

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
| `src-tauri/src/p2p_cdn`                            | `a3net-blobstore`, `a3net-mesh`, `a3net-transport` | `iroh-blobs::BlobTicket` is parsed by [`a3net_types::BlobTicket`]    |
| `src-tauri/src/p2p_cdn/gossip_bridge.rs`           | `a3net-gossip::bridge`                          | Replaced by `iroh-gossip` behind [`a3net_gossip::GossipTransport`]   |
| `src-tauri/src/microservice/p2p_gossip_service.rs` | `a3net-gossip::InProcessGossip`                 | Future `iroh-net::Gossip` behind the same trait                       |
| `src-tauri/src/p2p_cdn/iroh_adapter.rs`            | `a3net-transport::iroh::IrohTransport`          | Feature-gated `iroh` backend (see `crates/a3net-transport/src/iroh.rs`) |
| `src-tauri/src/chatstore`                          | `a3net-chatstore`                               | `iroh-docs` provides the message sync substrate (Phase 5a)             |

---

## Test count

`cargo test --workspace` runs **3,500+ tests** across 74 crates:

```
a3net-types              unit + property tests
a3net-blobstore          unit + proptest (bitswap invariants)
a3net-gossip             unit + chaos tests
a3net-mesh               unit + integration
a3net-transport          unit + iroh e2e (feature-gated)
a3net-node               unit + multi-transport integration
a3net-dht                unit + aerospace-grade chaos
a3net-namespace          unit + comprehensive
a3net-relay              unit + ActiveSessionGuard tests
a3net-ipc-adapter        unit + integration (noise handshake)
a3net-resilience         unit (retry / breaker / cancellation / limiter)
a3net-observability      unit + structured panic
a3net-agent              unit + provider mock
a3net-cli                unit + daemon smoke + ipc-client tests
a3net-roster             unit
a3net-chatstore          unit + iroh-docs sync
a3net-reputation         unit + decay loop
a3net-moderation         unit + integration
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
# a3net-types — identity / ticket / announcement wire formats
cargo run -p a3net-types --example node_id_roundtrip
cargo run -p a3net-types --example blob_ticket_demo
cargo run -p a3net-types --example announcement_demo

# a3net-blobstore — chunked round-trip with range reads
cargo run -p a3net-blobstore --example round_trip

# a3net-gossip — two-node publish/subscribe over InProcessGossip
cargo run -p a3net-gossip --example two_node_publish

# a3net-mesh — HTTP mesh server + client (whole, Range:, multi-range)
cargo run -p a3net-mesh --example server_and_client

# a3net-transport — native QUIC roundtrip with framed messages
cargo run -p a3net-transport --example quic_roundtrip

# a3net-node — multi-node blob sharing / gossip echo
cargo run -p a3net-node --example blob_share
cargo run -p a3net-node --example two_node_echo

# a3net-cli — CLI parser and all commands
cargo run -p a3net-cli --example cli_parser

# a3chat — end-to-end encrypted DM round-trip
cargo run -p a3chat-app --example dm_roundtrip

# a3net-agent — provider + tool call
cargo run -p a3net-agent --example chat_loop

# a3net-tui — terminal UI demo
cargo run -p a3net-tui --example tui_demo
```

---

## License

Dual-licensed under **MIT OR Apache-2.0**, at your option.

See `LICENSE-MIT` and `LICENSE-APACHE` at the repository root, or
[choose at your option](https://opensource.org/licenses/MIT).