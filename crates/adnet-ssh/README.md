# `adnet-ssh`

> SSH-over-iroh tunnel — vendored port of
> [`iroh-ssh`](https://github.com/rustonbsd/iroh-ssh). Lets any
> ADNet node be reached as `user@<endpoint-id>` without exposing
> ports, traversing NAT, or relying on a public IP.

## What it does

- Binds an iroh `Endpoint` that speaks the `adnet/ssh-tunnel/1`
  ALPN and proxies every incoming QUIC stream to the local SSH
  daemon (default port 22, configurable).
- Connects out to a remote endpoint id and turns the QUIC stream
  into a TCP-shaped byte pipe the system `ssh(1)` can consume
  via `ProxyCommand`.
- Reuses the persistent Ed25519 identity minted by
  [`adnet_transport::iroh::IrohIdentity`] so the SSH endpoint id
  matches the rest of the ADNet runtime (gossip, blob, node).

## Crate layout

| module        | purpose                                                    |
|---------------|------------------------------------------------------------|
| `error`       | typed [`error::SshError`]                                  |
| `keys`        | persistent-key resolution helpers                          |
| `builder`     | [`builder::IrohSshBuilder`]                                |
| `server`      | long-running [`server::Server`] task                       |
| `client`      | connect-side helpers + ProxyCommand parser                 |
| `info`        | the `info` command — print the invitation                  |
| `metrics`     | Prometheus counters for accepted/failed/in-flight bridges  |

## Quick start

```rust,no_run
# #[cfg(feature = "iroh")] {
use adnet_ssh::{IrohSshBuilder, server};

# async fn demo() -> anyhow::Result<()> {
let ssh = IrohSshBuilder::new("./.adnet-data")
    .accept_incoming(true)
    .accept_port(22)
    .build()
    .await?;
let server = server::Server::start(ssh).await?;
println!("invite: adnet-ssh alice@{}", server.endpoint_id());
# Ok(()) }
# }
```

## Features

| feature | default | what it gates                                     |
|---------|---------|---------------------------------------------------|
| `iroh`  | **off** | enables `IrohSshBuilder`, `Server`, and `client`  |

Without `iroh`, the crate compiles to identity types + error
types only; the `IrohSshBuilder::build` stub returns
[`SshError::FeatureMissing`].

## Testing

```bash
cargo test -p adnet-ssh                  # 15 tests (default build)
cargo test -p adnet-ssh --features iroh  # 26 tests total (iroh-only loopback + integration)
```

The smoke tests cover:

- every `SshError` variant's `Display` string (locked-in for log
  scrapers);
- `keys::resolve_data_dir` for `Some("…")`, `None`, and `Some("")`;
- `SSH_TUNNEL_ALPN` byte-string stability;
- `probe_local_ssh` on an unbound port (either timeout or
  connection-refused is acceptable);
- the no-`iroh` stub returns `SshError::FeatureMissing`.

## License

Same as the workspace root (`workspace.package.license`).