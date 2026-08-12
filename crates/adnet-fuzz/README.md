# adnet-fuzz

> Coverage-guided fuzzing for ADNet's wire-format parsers — `cargo fuzz` harness with six ready-to-run targets.

## 概览 (Overview)

`adnet-fuzz` is the security/perimeter testing layer for ADNet. Every
boundary that takes bytes from the network (DHT wire messages,
Bitswap, GraphSync, CID/NodeId/Announcement parsers) is exercised
against millions of mutated inputs by [`cargo-fuzz`] backed by
[libFuzzer]. The crate ships **six ready-to-run fuzz targets** plus a
thin support library that lets the workspace `cargo check` /
`cargo test` against the fuzz entrypoint without trying to link a
`#![no_main]` binary.

The crate is intentionally structured so the fuzz entrypoint
(`src/fuzz_main.rs`) is `#![no_main]` — the real targets live in
`fuzz_targets/<name>.rs` and are linked into separate binaries
configured in `fuzz.toml`. A regular library target
(`src/support.rs`, exposed as `adnet_fuzz_support`) keeps the
workspace build green.

[`cargo-fuzz`]: https://github.com/rust-fuzz/cargo-fuzz
[libFuzzer]: https://llvm.org/docs/LibFuzzer.html

## 特性 (Features)

- **Six fuzz targets** — one per protocol surface:
  - `parse_announcement` — JSON, CBOR, MessagePack, postcard
    deserialisation + round-trip
  - `parse_cid` — CID string + binary formats, multihash / BLAKE3
    operations
  - `parse_node_id` — NodeId from bytes / hex / Ed25519 derived id
    + signature verification round-trip
  - `parse_dht_message` — protobuf + postcard DHT wire messages +
    routing-table operations
  - `parse_graphsync` — GraphSync JSON request/response/block
    validation
  - `parse_bitswap` — protobuf Bitswap messages + custom
    `BitswapCodec` + `WantlistManager` stress
- **Multiple format coverage** — JSON, postcard, CBOR, MessagePack,
  protobuf and `read_bytes` are exercised side-by-side so a parser
  bug in one encoding cannot hide behind success in another.
- **Sanitised** — `fuzz.toml` enables the **AddressSanitizer** for
  x86_64/aarch64 Linux and Apple-Silicon/Darwin.
- **Compile-time hardening** — `[profile.release]` for the fuzz
  binary uses `lto = true` and `codegen-units = 1` to maximise
  inlining / coverage.
- **Library stub** — `adnet_fuzz_support` (the `[lib]` target)
  re-exports nothing today; it exists so the workspace keeps a
  buildable, non-`no_main` library target next to the fuzz
  entrypoint.

## 安装 (Installation)

`adnet-fuzz` is a workspace-internal crate — it is **not** published.
You only need it if you want to run fuzz targets locally:

```bash
cargo install cargo-fuzz   # nightly toolchain required
```

The fuzz entrypoint is configured via `fuzz.toml`. Add the following
to `~/.cargo/config.toml` if you haven't already:

```toml
[build]
rustflags = ["-C", "debuginfo=2"]
```

## 使用 (Usage)

List the targets:

```bash
cargo fuzz list
```

Run a specific target (max total time bounded):

```bash
cargo fuzz run parse_announcement -- -max_total_time=300
```

Run with a saved corpus (seeds from a previous run):

```bash
cargo fuzz run parse_bitswap fuzz_corpus/parse_bitswap
```

Re-build all targets without running them:

```bash
cargo fuzz build
```

Render a coverage report (requires `cargo cov`):

```bash
cargo fuzz coverage parse_cid
```

**Note:** these targets are not built as workspace `examples`. They
are compiled by `cargo-fuzz` into the `target/<triple>/coverage/`
directory. If you want to drive the same parsers from a regular
binary, see the doctest in `src/support.rs` — the parsers are
re-exported via the workspace crates directly.

## 应用案例 (Use Cases / Examples)

- **Pre-release security audit** — run every target for 1 hour
  (`-max_total_time=3600`) on a release branch. Any ASan/UBSan hit
  blocks the tag.
- **Regression-driven fuzzing** — keep the seeds produced by a
  crash in `fuzz_corpus/<target>/crash-<sha>.bin` checked in (in
  a private fork) so a CI job can replay the exact input.
- **Parser coverage** — combine `cargo fuzz coverage` output with
  the `adnet-types` and `adnet-blobstore` source map to find
  branches that no test currently exercises.
- **Onboarding** — new contributors can `cargo fuzz run
  parse_node_id -- -max_total_time=60` to see a working fuzz
  harness before they wire their own surface.

## 许可

MIT OR Apache-2.0
