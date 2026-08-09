# ADNet Workspace — Transport Crate

This crate provides:

- [`Transport`](src/traits.rs) — abstract transport contract
- [`Frame`](src/frame.rs) — length-prefixed codec shared by every backend
- [`QuicTransport`](src/quic.rs) — native `quinn` backend
- [`IrohTransport`](src/iroh.rs) — placeholder for a future `iroh-net` backend

The iroh backend is feature-gated behind `cargo build --features iroh`. Until
then the QUIC backend serves as the production transport; the mesh HTTP layer
in `adnet-mesh` is the always-on fallback.
