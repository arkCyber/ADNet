# A3Net C-ABI & Swift / Kotlin Bindings (Gap §5)

This crate provides two complementary surfaces that mobile
(iOS / Android) and WASM embedders consume:

| Component | Path | Purpose |
|-----------|------|---------|
| Rust FFI (C-ABI) | `src/lib.rs` | `extern "C"` functions, JSON-encoded results, owned tokio runtime per handle |
| C header | `include/a3net_ffi.h` | Hand-written counterpart of `cbindgen` output. Stable ABI for iOS / Android consumers |
| **Rust FFI (uniffi)** | `src/uniffi_surface.rs` + `src/a3net.udl` | `uniffi`-driven surface producing typed Swift / Kotlin / Python bindings |
| Swift wrapper (C-ABI) | `examples/A3NetFFI.swift` | Reference Swift API mirroring the iroh-ffi shape |
| Kotlin wrapper (C-ABI) | `examples/A3NetFfi.kt` | Reference Kotlin API mirroring the iroh-ffi shape |
| Swift wrapper (uniffi) | `examples/A3NetUniffiDemo.swift` | SwiftUI demo + `AdnetHandle` driver against the uniffi bindings |
| Kotlin wrapper (uniffi) | `examples/A3NetUniffiDemo.kt` | Coroutine-driven ViewModel + driver against the uniffi bindings |

## Building

```bash
# Default build — native QUIC only, no iroh dependency.
cargo build -p a3net-ffi --release

# Mobile build — pulls in iroh for NAT-traversal / DERP relay.
cargo build -p a3net-ffi --release --features iroh

# Build the uniffi surface (typed errors, callback interfaces).
cargo build -p a3net-ffi --features uniffi

# Generate the Swift / Kotlin bindings.
cargo run -p a3net-ffi --features uniffi -- uniffi generate \
    src/a3net.udl --language swift --out-dir bindings/swift
cargo run -p a3net-ffi --features uniffi -- uniffi generate \
    src/a3net.udl --language kotlin --out-dir bindings/kotlin
```

Output (release): `target/release/liba3net_ffi.a` (or `.dylib` /
`.so` depending on platform). The header is consumed by both
Swift and Kotlin native bridges.

## iOS (Swift)

```swift
import A3NetFFI

let node = try AdnetNode(dataDir: "/var/mobile/.../a3net")
print("local NodeId: \(try node.nodeId())")
```

The Swift class wraps the opaque handle and frees it on `deinit`
(via `a3net_ffi_node_destroy`). All calls block the calling
thread; wrap them in `Task` / a dedicated `DispatchQueue` if you
need async semantics.

## Android (Kotlin)

```kotlin
import a3net.ffi.A3NetFfi

val node = A3NetFfi.nodeCreate(dataDir = "/data/data/.../files/a3net")
println("local NodeId: ${A3NetFfi.nodeId(node)}")
```

The Kotlin object loads `liba3net_ffi.so` at class-init time;
the JVM side never dereferences the opaque handle, it only
forwards it back into Rust.

## ABI contract

- Functions return `int32_t` status codes (0 = OK, negatives
  enumerate error categories).
- Output buffers are heap-allocated by Rust and **must** be
  released with `a3net_ffi_free(buf)`.
- On error, the output buffer (if non-NULL) carries a JSON
  `FfiResult<()>` with a human-readable `error` field.
- Each handle owns one tokio runtime; do not share a handle
  across threads.

## Test coverage

The unit tests in `src/lib.rs` pin down:

- `NULL` / empty / invalid UTF-8 inputs produce stable error
  codes.
- JSON result shapes (`FfiResult::ok` / `err` / `unit_ok`).
- `a3net_ffi_version()` is non-zero and matches the constant
  in `version.rs`.
- `a3net_ffi_node_destroy(NULL)` is a no-op.

These run under `cargo test -p a3net-ffi --lib` and
`cargo test -p a3net-ffi --features iroh --lib`.

## Why both C-ABI and uniffi?

The C-ABI is the **minimum** every embedder needs; we ship
it on every build because C is the universal FFI. The uniffi
surface is a **super-set** that adds:

- typed `AdnetError` → Swift `Error`, Kotlin `Throwable`
  (no string-parsing on the embedder side);
- automatic `Option<T>` ↔ Swift `Optional<T>` / Kotlin `T?`;
- typed records (no manual JSON decode for `BlobPutInfo` etc);
- (future) callback interfaces for gossip subscribe.

Operators that want a 50 KB Swift framework can use the uniffi
surface; operators that want to talk to us from a low-level C
engine can use the C ABI. Both share the same Rust
implementation; the uniffi surface wraps it in
`uniffi_surface.rs`.

## uniffi v0.1 limitations

The v0.1 uniffi surface is **deliberately conservative** —
`Node::put_blob` / `Node::ipns_publish` / `Node::metrics` do
not exist on the v0.1 `Node` (see Gap §5). Calls succeed and
return well-formed records, but `put_bytes` hashes the bytes
locally and emits a placeholder ticket; `ipns_publish`
returns the supplied name unchanged; `metrics()` returns zero
counters. A follow-up PR wires these against the real
`a3net-node` APIs once those land.

This is intentional: the demo lets mobile engineers iterate
on the binding shape today, and the back-end lands as the
underlying API stabilises.
