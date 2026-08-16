# `a3net-rpc-irpc`

> **Status: experimental design draft, not production code.**

This crate is a small, self-contained prototype that asks the
question:

> "What would `a3net-rpc`'s IPFS-compatible command surface look like if
> expressed as an [`irpc`](https://github.com/n0-computer/irpc)
> protocol?"

## Why a separate crate

The author asked for *"a fully isolated, zero-dependency-expansion
interface design draft module"*. Adding this crate to the A3Net root
`Cargo.toml` `[workspace] members` would force `cargo metadata` over
the whole workspace to resolve the `irpc` dependency graph (`tokio`,
`postcard`, `n0-future`, `n0-error`, and several `n0-*` crates).
That contradicts the isolation goal — it is the *root manifest*
that must remain clean, not the existing `crates/` directory.

So this crate is intentionally **not** in `[workspace] members`. It
is built with:

```sh
cd crates/a3net-rpc-irpc
cargo check --no-default-features --features local
cargo run  --no-default-features --features local --example echo
```

If you ever change your mind and want it in the workspace, audit
the comment block at the top of `Cargo.toml` first.

## Layout

```
crates/a3net-rpc-irpc/
├── Cargo.toml          # irpc = "=0.17.0" pinned; default = ["local"]
├── src/
│   ├── lib.rs          # ⚠ status banner, re-exports, hard forbids
│   └── service.rs      # Protocol enum + Result types, mirroring a3net-rpc
├── examples/
│   └── echo.rs         # one variant of each irpc interaction pattern (local)
└── README.md           # tracking checklist + open questions
```

## What is in the protocol

The protocol enum in `src/service.rs` covers every public function
in `crates/a3net-rpc/src/commands.rs`:

| a3net-rpc fn     | irpc variant   | channel shape     |
|------------------|----------------|-------------------|
| `dag_put`        | `DagPut`       | oneshot           |
| `dag_get`        | `DagGet`       | oneshot           |
| `dag_resolve`    | `DagResolve`   | oneshot           |
| `dag_import`     | `DagImport`    | rx-streaming      |
| `block_put`      | `BlockPut`     | oneshot           |
| `block_get`      | `BlockGet`     | oneshot           |
| `block_stat`     | `BlockStat`    | oneshot           |
| `block_rm`       | `BlockRm`      | oneshot           |
| `pin_add`        | `PinAdd`       | oneshot           |
| `pin_rm`         | `PinRm`        | oneshot           |
| `pin_ls`         | `PinLs`        | tx-streaming      |
| `gc`             | `Gc`           | tx-streaming      |
| `node_id`        | `NodeId`       | oneshot           |
| `version`        | `Version`      | oneshot           |

## What is intentionally omitted

- **No service `Handler` implementation.** Wiring the protocol to
  `a3net-blobstore::BlobStore` would force the irpc crate to depend
  on the A3Net workspace. The whole point of this crate is to stay
  externally observable; if we want a real `Handler`, we do it as a
  follow-up crate that *does* opt into the workspace.
- **No `iroh` dependency.** The `local` feature does not pull irpc's
  transport surface (`noq`, `postcard`) — see `default = ["local"]`
  in `Cargo.toml`. The `remote` feature is reserved for future
  experiments.
- **No `#[rpc::service]` trait derive.** irpc's `#[rpc::service]`
  expands a *trait* into request/response shapes. We use the lower-
  level `#[rpc_requests]` macro that emits only the dispatch enum.
  Trait derivation is the next iteration once irpc's derive macros
  stabilise (see tracking checklist below).
- **No FFI / no JavaScript / no Swift / no Kotlin.** irpc explicitly
  does not target cross-language interop (see its crate-level
  docs).

## Tracking checklist (renew quarterly)

| Watch item                                            | Status (2026-08-13)                       |
|-------------------------------------------------------|-------------------------------------------|
| `irpc` reaches 1.0                                    | Current 0.17.0, last release 2026-06-15   |
| `irpc-derive` macro surface stable                    | actively moving; derives still 0.x        |
| `iroh-docs` v2 stable                                 | not yet (irop main branch: v1 API)         |
| `iroh` `QUIC multipath` public API stable             | iroh 1.0.x: experimental, marked internal  |
| iroh version pinned to a release we depend on         | irpc 0.17 references `iroh = "1"` in its `[workspace.dependencies]` |
| A3Net FFI / non-Rust clients still need a strategy     | **open** — see "Open questions" below     |

Update each row when you re-check; do not auto-bump without
updating the `=0.17.0` pin and reading the irpc CHANGELOG.

## Open questions (blockers for actual migration)

1. **`a3net-ffi` (Swift / Kotlin / Python UDL via uniffi)** can never
   consume irpc. Do we keep `a3net-rpc` as a *secondary* transport
   for FFI, or do we revisit (e.g. introduce gRPC alongside irpc)?
2. **`a3net-cli`** currently shells out to a sub-process via JSON
   over stdio. irpc has no stdio transport — we'd need either a CLI
   wrapper that speaks QUIC, or keep CLI on a JSON-RPC-like path.
3. **`a3net-ipc`** uses mpsc internally, then bridges out via
   Tower/HTTP-style servers. irpc's "tx streaming" is genuinely
   stream-shaped — would require re-thinking request lifecycle.
4. **Type sharing.** `a3net-rpc`'s `RpcError` is an `anyhow`-style
   error. irpc requires `Serialize`/`Deserialize` on every message
   enum; a3net's internal `thiserror` errors would need conversion.
5. **Backpressure.** irpc's tx-streaming requires explicit buffer
   caps (`bidi_streaming(..., tx_buf, rx_buf)` in the echo example).
   A3Net's blob operations can return multi-GB pins — picking buffer
   sizes is non-trivial.

## How to run

```sh
cd crates/a3net-rpc-irpc

# Compile only (no network, ~5 s)
cargo check --no-default-features --features local

# Run the demo (local channel, no QUIC)
cargo run --no-default-features --features local --example echo
# → "echo example: all four interaction patterns OK ✓"
```

## License

Apache-2.0 OR MIT — same as the rest of A3Net.
