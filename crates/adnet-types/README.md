# adnet-types

> Core type definitions shared across the ADNet workspace — `NodeId`, `ContentHash`, `Announcement`, `WalletAddress`, `BlobTicket`, plus wire-format invariants for the gossip / RPC / FFI layers.

## 概览 (Overview)

`adnet-types` is the **wire-format bedrock** of ADNet. Every other crate in the workspace depends on it for the stable, low-level types that flow over gossip, RPC, and the FFI boundary: 32-byte `NodeId` hex identifiers, BLAKE3 `ContentHash` digests, `Announcement` payloads that peers broadcast into rooms, tickets that pin a node id to a network address and a content hash, and the configured limits (`MAX_*_LEN`) that the gossip validator enforces.

The crate is **deliberately crypto-free**. The `secp256k1` / `x25519-dalek` / `aes-gcm` dependencies live in `adnet-identity` so that this crate stays cheap to compile and can be depended on by `adnet-blobstore`, `adnet-rpc`, `adnet-ffi`, and any other leaf that should not pull in a full crypto stack. Anything that needs to *sign* or *verify* an `Announcement` reaches into `adnet-identity`.

Two design tensions are worth knowing about:

1. **Stability over flexibility.** `Announcement`, `BlobTicket`, and the address structs are the wire format. Adding a field is cheap; removing or renaming one is a breaking change that touches every transcoder in the workspace.
2. **Determinism over display.** `BTreeMap` is used where the JSON shape needs to be reproducible (e.g. peer-source ordering), `#[serde(rename_all = "camelCase")]` is forced on announcement payloads, and the canonical JSON preimage that signs gossip is built by hand so a `serde_json` upgrade can never change the digest.

## 特性 (Features)

- **`NodeId`** — 32 random bytes, hex-encoded, with `from_bytes` / `from_hex` / `random` / `short` / `xor_distance` helpers.
- **`ContentHash`** — lowercase hex BLAKE3 digest of arbitrary bytes, with streaming `from_reader` support.
- **`Announcement`** — JSON-friendly gossip payload that carries `(room_id, content_hash, node_id, title, kind, size_bytes, mime_type, source_url, ticket, timestamp, …)`. Includes `validate()`, `signing_preimage()`, and `from_ai_recommendation()` for snake_case ingest.
- **`AnnouncementPayload`** — the serialised appearance of an announcement, plus `Display` / `TryFrom<AnnouncementPayload>` for round-tripping.
- **`BlobTicket` / `PeerTicket` / `NodeAddrTicket`** — `adnet-blob://`, `adnet-peer://`, `adnet-addr://` URLs with `encode()` / `parse()` and a `http_base()` helper for the mesh fallback.
- **`RoomId` / `RoomAsset`** — room identifiers and the per-asset metadata that drives `feed` / `subscribe_room`.
- **`Topic`** — typed pubsub topic with a `topic_name()` helper.
- **`WalletAddress`** — 20-byte EVM-style address (no crypto, just byte storage and hex conversion; crypto lives in `adnet-identity`).
- **`MeshNetworkId` / `MeshPolicy` / `MeshMember` / `MeshMembership` / `InviteCode`** — mesh-policy primitives.
- **`GroupChat` / `DirectChat` / `MessageEnvelope` / `Attachment` / `Validate`** — chat payload / validation helpers.
- **`SocialPost` / `SocialComment` / `SocialReaction` / `FollowRelationship`** — social feed types.
- **`BulletinItem` / `BulletinCategory` / `BulletinKind` / `BulletinSeverity`** — bulletin board model.
- **`ByteRange` / `RangeSpec`** — HTTP Range-style byte requests; converts to / from the `Range:` header.
- **`PeerSource` / `PeerMap`** — `Hash`-keyed source map with caps (`MAX_PEER_SOURCES_PER_HASH`, `MAX_TRACKED_HASHES`) and tracing-friendly invariants.
- **`DagCodec` / `DagCodecRegistry` / `DagLinkRef`** — codec registry for IPLD-style DAG traversal.
- **`MAX_*_LEN` / `MAX_*` invariants** — feed-limit, name-length, tag-count, member-count, etc. The gossip validator uses these to reject malformed payloads before they reach the application.
- **`feature = "protobuf"`** — exposes `proptest` / `chrono` plumbing for protobuf-backed announcements (used by `adnet-types` itself in tests, opt-in by default).

## 安装 (Installation)

`adnet-types` is a workspace crate, so any other ADNet crate can simply `use adnet_types::...`. No `Cargo.toml` change is required.

```toml
# In a new crate that depends on adnet-types:
[dependencies]
adnet-types = { workspace = true }
```

## 使用 (Usage)

### Build a `ContentHash` from bytes and verify

```rust
use adnet_types::ContentHash;

let hash = ContentHash::from_bytes(b"hello adnet");
assert_eq!(hash.as_hex().len(), 64);
let short = hash.short(); // "181cd..". first 8 chars
println!("hash: {hash} (short: {short})");
```

### Generate a `NodeId`, attach it to a `NodeAddr`, encode a `BlobTicket`

```rust
use adnet_types::{
    BlobTicket, ContentHash, Endpoint, NodeAddr, NodeId, RelayUrl,
};

let me = NodeId::random();
let addr = NodeAddr::new(me.clone())
    .with_direct(Endpoint::new("10.0.0.42", 9000))
    .with_relay(RelayUrl::new("https://relay.example.com"));
let hash = ContentHash::from_bytes(b"document.pdf");

let ticket = BlobTicket::whole(&me, &addr, &hash);
let url = ticket.encode();           // adnet-blob://<id>@10.0.0.42:9000/<hash>
let back = BlobTicket::parse(&url).expect("round-trip");
assert_eq!(back, ticket);
```

### Build and validate an `Announcement`

```rust
use adnet_types::{Announcement, CdnContentKind, ContentHash, NodeId, RoomId};
use chrono::Utc;

let ann = Announcement {
    room_id: RoomId::new("lobby"),
    content_hash: ContentHash::from_bytes(b"payload"),
    node_id: NodeId::random(),
    title: "My report".into(),
    kind: CdnContentKind::Article,
    size_bytes: 12_345,
    mime_type: Some("application/pdf".into()),
    source_url: None,
    ticket: None,
    timestamp: Utc::now(),
    message_id: None,
    ttl_secs: None,
    signer: None,
    signature: None,
};

ann.validate().expect("malformed announcement");
```

### Walk a byte range

```rust
use adnet_types::{ByteRange, RangeSpec};

let header = RangeSpec::Single(ByteRange::new(0, 1024).unwrap()).to_http_header();
// header == Some("bytes=0-1023")
let parsed = RangeSpec::from_http_header("bytes=0-1023", 10_000).unwrap();
```

## 应用案例 (Use Cases / Examples)

- **ADNet gossip bus** — every JSON gossip frame is an `Announcement` (or a `PeerSource` map). The gossip validator runs `Announcement::validate()` against the `MAX_*_LEN` constants before forwarding.
- **`adnet-blobstore`** — uses `ContentHash` to key blocks, `BlobTicket` to publish download URLs, and `RangeSpec` to honour HTTP `Range:` requests.
- **`adnet-rpc` / `adnet-ffi`** — exposes typed commands that take / return `ContentHash`, `NodeId`, and `BlobTicket`, so the IPC layer never has to parse raw strings.
- **`adnet-identity`** — wraps `PlainWalletAddress` with EVM-style signing primitives; the two types interop via the `From` impls in `adnet-identity`.
- **AI ingestion** — `Announcement::from_ai_recommendation("lobby", &node_id, &json)` ingests a loose snake_case JSON payload from an upstream model and converts it into a properly-shaped `Announcement`.

## 许可

MIT OR Apache-2.0
