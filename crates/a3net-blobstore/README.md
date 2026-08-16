# `a3net-blobstore`

> Disk-backed content-addressed blob store and NAS-style logical namespace for A3Net. BLAKE3-chunked storage with iroh-blobs layout parity, an optional iroh-backed Bao-verified transport, and a DO-178C DAL-A WebDAV surface.

## 概览 (Overview)

`a3net-blobstore` is the storage layer of the A3Net workspace. It provides:

- **`BlobStore`** — a per-blob on-disk layout (`<hash>/meta.json + chunks/* + complete`) whose files mirror the iroh-blobs `flat` form so a downstream iroh node can ingest the same files after a small rename.
- **`AsyncBlockStore`** / **`DagBlockStore`** — async + DAG-block-friendly adapters layered on top of the raw chunked store.
- **`Nas`** — a logical path-keyed namespace on top of `BlobStore` with an atomic-swap manifest, append-only audit log, traversal-rejecting `PathSegments`, and quota hooks used by the WebDAV gateway.
- **Bao tree + swarm download** — Merkle-aware helpers (`BaoTree`, `BaoLeaf`, `BaoProof`) and a multi-peer swarm chunk fetcher (`SwarmDownloader`, `SwarmDownloadService`).
- **Optional features** — `iroh` (adds `IrohBlobStore` on iroh-blobs Bao-verified fs store), `bitswap` (Bitswap engine + wantlist + codec), `car` (CAR writer / reader), `scope` (storage topology + quota policy).

The crate sits between the chat/feed layers and the WebDAV/iroh transport layers; every byte A3Net stores under a content hash ultimately flows through here.

## 特性 (Features)

- `BlobStore::new(data_dir)` — open or create a chunked content store.
- `BlobStore::import_file_sync(path) -> (ContentHash, u64)` — stream-import a local file in `CHUNK_SIZE` (16 KiB) chunks, BLAKE3 hash computed on the fly.
- `BlobStore::put_bytes_sync(&[u8]) -> (ContentHash, u64)` — ingest an in-memory buffer in one chunk.
- `BlobReader` trait with `read_all`, `read_range`, `read_chunk`, `size`, `chunk_count`, `has`.
- `NamespaceRead` / `NamespaceWrite` traits on `Nas`: `lookup`, `read_file`, `snapshot`, `put`, `delete`, `mkcol`, `rename`, `copy`.
- `Nas::write_bytes(&[u8]) -> (ContentHash, u64)` followed by `NamespaceWrite::put` — the canonical write path.
- `BaoTree`, `BaoLeaf`, `BaoProof`, `BaoTreeBuilder` — Merkle proof structures for verified streaming.
- `SwarmDownloader` / `SwarmDownloadService` — multi-peer, end-game-aware chunk fetcher with metrics.
- `safe_filename(&str) -> String` — sanitise user-supplied names against a configurable `MAX_FILENAME_LEN`.
- `filename::MAX_FILENAME_LEN`, `chunked::CHUNK_SIZE`, `namespace::MAX_DEPTH`, `MAX_CHILDREN_PER_DIR`, `MAX_PATH_RAW_LEN` — bounded-resource constants surfaced as public.

## 安装 (Installation)

The crate is a workspace path-dependency; reference it from any A3Net member with:

```toml
[dependencies]
a3net-blobstore = { workspace = true }
```

Then in source:

```rust
use a3net_blobstore::{BlobStore, BlobReader};
use a3net_blobstore::namespace::{Nas, NamespaceRead, NamespaceWrite};
use a3net_blobstore::filename::safe_filename;
```

The optional `iroh`, `bitswap`, `car`, and `scope` features are enabled by adding the relevant `features = [...]` entries to the consumer `Cargo.toml`.

## 使用 (Usage)

### 1. Round-trip a small payload through the chunked store

```rust
use a3net_blobstore::{BlobReader, BlobStore, CHUNK_SIZE};
use a3net_types::ContentHash;

# async fn demo() -> std::io::Result<()> {
let dir = tempfile::tempdir()?;
let store = BlobStore::new(dir.path())?;

// put_bytes_sync writes one chunk regardless of payload size
let payload = vec![0u8; 4096];
let (hash, size) = store.put_bytes_sync(&payload)?;
assert_eq!(size, 4096);

let full = BlobReader::read_all(&store, &hash).await?;
assert_eq!(full, payload);
# Ok(()) }
```

### 2. Stream-import a file from disk

```rust
use a3net_blobstore::BlobStore;

let dir = tempfile::tempdir()?;
let store = BlobStore::new(dir.path())?;
let (hash, size) = store.import_file_sync(std::path::Path::new("photo.png"))?;
println!("stored {size} bytes as {hash}");
```

### 3. Read a sub-range via the typed `RangeSpec`

```rust
use a3net_blobstore::{BlobReader, BlobStore};
use a3net_types::{ByteRange, RangeSpec};

# async fn demo() -> std::io::Result<()> {
let store = BlobStore::new(tempfile::tempdir()?.path())?;
let (hash, _) = store.put_bytes_sync(b"hello, world")?;
let slice = BlobReader::read_range(&store, &hash,
    RangeSpec::Single(ByteRange::new(0, 5).unwrap())).await?;
assert_eq!(slice, b"hello");
# Ok(()) }
```

### 4. Use the NAS namespace (logical paths on top of `BlobStore`)

```rust
use a3net_blobstore::namespace::{
    AuditContext, Nas, NamespaceRead, NamespaceWrite, NoopQuota, SystemClock,
};
use a3net_blobstore::PathSegments;

let dir = tempfile::tempdir()?;
let nas = Nas::open(dir.path())?;
let path = PathSegments::decode_http("/photos/sunset.png").unwrap();

let (hash, size) = nas.write_bytes(b"PNG bytes")?;
nas.put(
    &path, hash, size,
    &AuditContext::default(),
    &SystemClock,
    &NoopQuota,
)?;

let bytes = nas.read_file(&path)?;
assert_eq!(bytes, b"PNG bytes");
```

## 应用案例 (Use Cases / Examples)

1. **Family NAS — WebDAV mount of home storage.** `WebdavServer` consumes `Nas` directly; PUT stores bytes via `BlobStore::put_bytes_sync`, MKCOL walks through `NamespaceWrite::mkcol`, and every state-changing verb writes an `AuditRecord` before acknowledging. The `examples/nas_tiering_demo.rs` walk-through illustrates a two-tier (hot/cold) topology.
2. **P2P file sharing via Bao-verified chunks.** `a3net-share` calls `walk_import` to build a `Collection` manifest, sends the `ShareTicket`, and the receiver reconstructs content via `BaoTree`/`BaoProof`. The `examples/round_trip.rs` example demonstrates the byte-for-byte invariants.
3. **Aerospace compliance test fixtures.** `MockClock`, `NoopQuota`, and the atomic `Arc::swap` of `Manifest` give compliance suites a deterministic, replayable audit log; the `examples/aerospace_bao_swarm.rs` integration covers the DAL-A SR-13/15/17/19 paths end-to-end.

## 许可

MIT OR Apache-2.0
