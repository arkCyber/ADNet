# `adnet-tun`

> Virtual-NIC / userspace-TUN driver for ADNet mesh networking.
> Provides a `UserspaceTun` that the mesh and firewall crates
> can read/write IPv4/IPv6 packets through, without requiring
> root privileges or kernel TUN support on macOS / Linux.

## Modules

| module      | purpose                                                       |
|-------------|---------------------------------------------------------------|
| `lib`       | public re-exports + crate-level constants                     |
| `device`    | trait-level abstractions (`TunDevice`, `TunPacketSink`)       |
| `packet`    | IPv4/IPv6 header parsing and serialization                    |
| `userspace` | cross-platform userspace TUN (loopback-only on CI)            |
| `native`    | optional native FFI bridge to libutun / OS-specific drivers   |
| `error`     | typed [`error::TunError`]                                     |

## Quick start

```rust,no_run
use adnet_tun::{UserspaceTun, TunConfig};

let mut tun = UserspaceTun::open(TunConfig::default())?;
while let Some(packet) = tun.recv().await {
    // parse + hand to firewall engine …
}
```

## Testing

```bash
cargo test -p adnet-tun   # 31 tests across unit + integration
```

Coverage includes:

- oversized-packet rejection
- many-packets round-trip in order
- parallel send/recv tasks
- info reflects lifecycle
- send-after-shutdown errors
- recv returns `None` after shutdown

## License

Same as the workspace root.