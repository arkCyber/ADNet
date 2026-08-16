# A3Net ↔ iroh-go sidecar

This directory hosts the **reference sidecar** implementations
that the `a3net-iroh-interop` harness drives over HTTP/JSON.

There are two reference sidecars, in two different languages:

1. **`iroh-rs-sidecar/`** — a Rust binary that uses the same
   `iroh = "1.0.3"` crates A3Net's transport already pulls in.
   Runs locally with zero external dependencies. This is the
   sidecar the integration tests use, and the one that lands
   in CI first.

2. **`iroh-go-sidecar/`** — a Go binary that uses
   `github.com/n0-computer/iroh` (the iroh-go SDK). This is
   the sidecar that exercises **the actual cross-language wire
   protocol** A3Net↔iroh-go. It runs in the nightly CI
   matrix, not in PR CI (PR CI uses the Rust reference
   sidecar to keep build times sane).

The wire protocol is identical for both — see
`crates/a3net-iroh-interop/src/wire.rs` for the contract and
`PROTOCOL.md` for the human-readable spec.

## Why two sidecars?

The PR smoke subset needs:

* A sidecar that builds in < 1 minute (the Rust one does; the
  Go one needs `go mod download` + `cargo` for the C FFI
  bridge, ~5 minutes cold).
* A sidecar whose source we can vendor as a workspace member
  (the Rust one is a path dep; the Go one would need a Go
  toolchain in CI).

The nightly comprehensive subset needs:

* A sidecar that proves the **actual** cross-language wire
  protocol works — that's iroh-go. The Rust reference sidecar
  only proves "the wire protocol *we wrote* is internally
  consistent" — useful, but not the same thing.

## Directory layout

```
sidecar/
├── README.md                  # this file
├── PROTOCOL.md                # wire-protocol spec (single source of truth)
├── iroh-rs-sidecar/           # Rust reference sidecar (PR smoke)
│   ├── Cargo.toml
│   └── src/main.rs
└── iroh-go-sidecar/           # iroh-go sidecar (nightly)
    ├── README.md              # build instructions
    ├── go.mod
    └── main.go
```

## Running the harness against the Rust reference sidecar

```bash
# Terminal 1: start the sidecar
cargo run --release -p iroh-rs-sidecar

# Terminal 2: drive the harness
cargo run -p a3net-iroh-interop --example smoke -- \
    --sidecar $(pwd)/target/release/iroh-rs-sidecar \
    --scenario sidecar_publish_a3net_subscribes \
    --topic interop-room-42 \
    --payload "hello from iroh-go (or our reference sidecar)"
```

The smoke runner prints `OK <scenario>` or `FAIL <scenario>` and
exits 0/1 accordingly.
