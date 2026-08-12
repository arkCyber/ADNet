# Release notes — `adnet-ssh`

## 0.1.0 — initial vendored port

This is the first release of `adnet-ssh`, a vendored port of
[`iroh-ssh`](https://github.com/rustonbsd/iroh-ssh) into the
ADNet workspace. The crate is intentionally feature-gated on
`iroh` so the default build remains free of the iroh dep tree
and can be used by docs builds, lint-only CI, or downstream
consumers that don't need the tunnel runtime.

### What it does

- Exposes an iroh `Endpoint` that speaks an SSH-tunnel ALPN
  (`adnet/ssh-tunnel/1`, distinct from `adnet/frame/1` and
  from the upstream `iroh-ssh` ALPN) and proxies each
  incoming QUIC stream to the local SSH daemon.
- Connects to a remote endpoint id and turns the QUIC stream
  into bytes that the system `ssh` client can consume via
  `ProxyCommand` (see `adnet_ssh::client::proxy`).
- Reuses the durable Ed25519 identity minted by
  `adnet_transport::iroh::IrohIdentity` so the SSH endpoint
  id is the same value as the ADNet node id and doesn't
  drift across restarts.

### What's wired

- `adnet-cli` REPL: `/ssh info`, `/ssh server [port]`,
  `/ssh connect <user>@<endpoint-id>` (gated on the `ssh`
  feature).
- `adnet-observability` metrics:
  - `adnet_ssh_tunnel_connections_accepted_total`
  - `adnet_ssh_tunnel_connections_failed_total`
  - `adnet_ssh_client_bridges_started_total`
  - `adnet_ssh_client_bridges_completed_total`
- Tests: 23 with `iroh`, 16 without (`smoke` + `lib` +
  `integration_tunnel`).

### What's intentionally out of scope

- No systemd `install` / `uninstall` path. Operator-facing
  service mode runs through `adnet-ssh server` from the
  REPL.
- No bundled SSH client. The `proxy` mode expects a system
  `ssh(1)` on `PATH`.
- No mDNS / DERP / pkarr discovery wiring. The `client::connect`
  variant takes an `EndpointId` and delegates resolution to
  iroh's configured discovery layer; the `client::connect_with_addr`
  variant takes a fully-resolved `EndpointAddr` for hermetic
  tests.

### Differences from upstream iroh-ssh

| Upstream                              | ADNet port                                  |
| ------------------------------------- | ------------------------------------------- |
| `iroh 0.94`                           | `iroh 1.0.3` (matches the rest of ADNet)    |
| Two key files (`~/.ssh/irohssh_*`)    | One file (`<data-dir>/iroh_secret_key`)      |
| `iroh-ssh` ALPN                       | `adnet/ssh-tunnel/1` ALPN                   |
| `Result<(), anyhow::Error>` API       | `Result<_, SshError>` typed enum            |
| No metrics                            | `adnet-observability` counters              |
| Async spawn in `accept`               | Inline await + `connection.closed().await`  |
| `connect(endpoint_id)`                | `connect(endpoint_id)` + `connect_with_addr`|
| `tokio::process::Command` abort all   | Half-close + `tokio::join!`                 |

### Audit findings (fixed in this release)

| #   | File                             | Issue                                                                   |
| --- | -------------------------------- | ----------------------------------------------------------------------- |
| 1   | `src/builder.rs`                 | Unused `IrohIdentity`, `Connection`, `SendStream`, `RecvStream` imports |
| 2   | `src/server.rs`                  | `use crate::error::*` was feature-gated and broke `probe_local_ssh`     |
| 3   | `src/server.rs`                  | `accept` returned immediately after `tokio::spawn` — connection closed early |
| 4   | `src/server.rs`                  | `proxy_bidirectional` no half-close — deadlocked on client FIN          |
| 5   | `src/client.rs`                  | `connect(endpoint_id)` attached the client's *own* IP addresses         |
| 6   | `src/client/proxy.rs`            | `proxy_bridge` aborted the survivor task — could drop a half-sent echo |
| 7   | `src/client/proxy.rs`            | Test fixture was 65 chars (one too many) — iroh rejected it             |
| 8   | `src/client/proxy.rs`            | `InvalidInvite` test asserted against `parse_invite` accepting all-zeros |
| 9   | `tests/smoke.rs`                 | `probe_local_ssh` test used a hard-coded port 19999                     |
| 10  | `tests/smoke.rs`                 | New stub tests expected `IrohSshBuilder` even when `iroh` is off        |
| 11  | `Cargo.toml`                     | `regex` dep was declared but never used                                 |
| 12  | `Cargo.toml`                     | `adnet-observability` dep was declared but never used → wired metrics   |
| 13  | `src/lib.rs`                     | `IrohSshBuilder` re-export only present when `iroh` is on              |
| 14  | `src/info.rs`                    | `info.rs` produced a warning when `iroh` is off                        |
| 15  | `src/server.rs`                  | `accept` API killed the connection on `Ok(())` return — added `connection.closed().await` |
