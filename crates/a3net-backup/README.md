# `a3net-backup`

Snapshot / restore for an A3Net data directory. Packages blobstore,
gossip spools, DHT store, pair records and identity material into a
single self-describing `.a3net-snap` file.

## What ships

| Item | Purpose |
|---|---|
| `snapshot(source_dir, out_path)` | Tar+zstd the directory tree into a self-describing file. |
| `restore(snap_path, dest_dir)` | Extract into a fresh directory, verifying every file's BLAKE3 hash against the manifest. |
| `verify(snap_path)` | Read-only integrity check — recomputes every hash without writing anything. |
| `SnapshotManifest` | Top-level metadata (version, timestamp, per-file size + BLAKE3). |
| `describe(manifest)` | One-line human-readable summary. |

## File format

```
+-------------------------+
| magic "ADNET-SNAP-v01\0\0"   (16 bytes)
+-------------------------+
| zstd-compressed tar stream  │
|   ├─ <rel-path-1>           │
|   ├─ <rel-path-2>           │
|   ├─ …                      │
|   └─ __manifest__.json      │  (last entry — JSON)
+-------------------------+
```

The first 16 bytes are a magic header so the restore side can sanity
check before paying for a zstd pass. The manifest is written last
and contains every entry's size + BLAKE3 checksum; restore refuses
to silently complete a partial extract.

## Usage

```rust,no_run
use a3net_backup::{snapshot, restore, describe};

// Capture
let manifest = snapshot("/var/lib/a3net", "/tmp/snap.a3net-snap")?;
println!("{}", describe(&manifest));

// Verify (e.g. on the receiving end before doing anything)
let _ = a3net_backup::verify("/tmp/snap.a3net-snap")?;

// Restore
let restored = restore("/tmp/snap.a3net-snap", "/var/lib/a3net-new")?;
```

## Scope

- File-based snapshot of a directory tree.
- BLAKE3 manifest checksum per file.
- Round-trip restore into a fresh directory.

## What's NOT included

- Streaming / incremental snapshots — every snapshot is full.
- Remote upload (S3 / rsync / IPFS) — the crate produces a local
  file; callers push it anywhere.
- Encryption — operators with at-rest needs should wrap the file
  with `age` / `gpg`.

## Tests

```
cargo test -p a3net-backup
```

5 tests cover round-trip, empty source, manifest format, magic
header rejection and `describe()`. The `backup_smoke` example
runs the full snapshot → verify → restore path on a fake
`data_dir`.