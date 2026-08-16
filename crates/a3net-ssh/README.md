# `a3net-ssh`

> **SSH-over-iroh tunnel** — vendored port of
> [`iroh-ssh`](https://github.com/rustonbsd/iroh-ssh). Lets any
> A3Net node be reached as `user@<endpoint-id>` without exposing
> ports, traversing NAT, or relying on a public IP.
>
> The crate ships three composable pieces:
>
> 1. A long-running **[`Server`]** that binds an iroh `Endpoint`,
>    speaks the `a3net/ssh-tunnel/1` ALPN, and proxies each
>    inbound QUIC stream to the local SSH daemon (default port 22).
> 2. A ProxyCommand-side **[`run`](client::proxy::run)** helper that
>    bridges a system `ssh(1)` to a remote endpoint, **and** a sibling
>    **[`run_sftp`](client::proxy::run_sftp)** helper for SFTP file
>    transfer over the same tunnel.
> 3. An opinionated **[`authorized_keys` / `trusted_peers`](keys)**
>    authentication layer that parses OpenSSH's `authorized_keys`
>    format and adds an A3Net-specific `from-a3net` option.

## Why

Upstream `iroh-ssh` proves the idea works: turn SSH into a
peer-to-peer service by tunneling the daemon's TCP stream through
an iroh QUIC connection, so any A3Net node can be addressed by its
endpoint id (`a3net-<short>`) without port forwarding, dynamic
DNS, or a public IP. This crate ports the same idea onto the
A3Net stack:

- reuses the durable Ed25519 identity A3Net already mints
  (`<data-dir>/iroh_secret_key`), so the SSH endpoint id equals
  the A3Net node id (no drift across restarts, no second identity
  file);
- advertises the namespaced `a3net/ssh-tunnel/1` ALPN, so a
  server here will never accidentally accept a connection from a
  generic `a3net/frame/1` peer (or vice-versa);
- ships an auth layer that understands both OpenSSH's standard
  `authorized_keys` file *and* an A3Net-specific
  `from-a3net=<endpoint-id>` option for endpoint-scoped allow-lists;
- sits behind the same `a3net-cli` REPL as the rest of the
  runtime, exposed via the `/ssh server`, `/ssh info`, and
  `/ssh connect` slash commands.

## How it fits

```
            ┌─────────────────────────────────────────────────┐
            │  peer A (alice@<endpoint-id-A>)                 │
            │   ssh(1) -o ProxyCommand="a3net-ssh proxy %h"  │
            └──────────────────────────┬──────────────────────┘
                                       │ stdin/stdout
                                       ▼
            ┌─────────────────────────────────────────────────┐
            │  a3net-ssh proxy (client::proxy::run)          │
            │   - parse "user@<endpoint-id>"                 │
            │   - iroh::Endpoint::connect(<id>,              │
            │                          a3net/ssh-tunnel/1)   │
            │   - bridge local ssh ⇄ QUIC stream             │
            └──────────────────────────┬──────────────────────┘
                                       │ QUIC / a3net/ssh-tunnel/1
                                       ▼  (relayed by DERP if peer is
                                       │   behind a NAT — no port
                                       │   forwarding required)
            ┌─────────────────────────────────────────────────┐
            │  peer B (this node)                             │
            │  a3net-ssh server::Server                       │
            │   ├─ Router::accept(SSH_TUNNEL_ALPN, …)        │
            │   └─ proxy_one_connection (bi-directional TCP  │
            │      ⇄ QUIC, half-close on EOF)                │
            └──────────────────────────┬──────────────────────┘
                                       │ 127.0.0.1:22
                                       ▼
                              ┌────────────────┐
                              │  local sshd    │
                              └────────────────┘
```

The same proxy path works for **SFTP** — the `client::proxy::run_sftp`
helper spawns the system `sftp(1)` and bridges it to the same
remote endpoint, falling back to `sftp-server` on the remote
sshd. No protocol changes are needed; SFTP happens to be a
subsystem of SSH.

## Crate layout

| module       | purpose                                                       |
|--------------|---------------------------------------------------------------|
| `error`      | typed [`error::SshError`]                                     |
| `keys`       | persistent-key resolution + `authorized_keys` / `trusted_peers` auth layer |
| `builder`    | [`builder::IrohSshBuilder`] — wires the SSH-tunnel ALPN       |
| `server`     | long-running [`server::Server`] task + `probe_local_ssh`      |
| `client`     | connect-side helpers                                          |
| `client::proxy` | `ProxyCommand` glue: `parse_invite`, `run`, `run_sftp`     |
| `info`       | the `info` command — pretty-print the invitation              |
| `metrics`    | Prometheus counters (`a3net_ssh_*` series)                    |

[`Server`]: crate::server::Server
[`error::SshError`]: crate::error::SshError
[`builder::IrohSshBuilder`]: crate::builder::IrohSshBuilder

## Quickstart

### 1. Server side — bind a tunnel

The server side loads the persistent A3Net identity from
`<data_dir>/iroh_secret_key` (creating it on first run), binds an
iroh `Endpoint`, and proxies every inbound QUIC stream to the
local sshd.

```rust,no_run
# #[cfg(feature = "iroh")] {
use a3net_ssh::{IrohSshBuilder, server};

# async fn demo() -> anyhow::Result<()> {
let ssh = IrohSshBuilder::new("./.a3net-data")
    .accept_incoming(true)
    .accept_port(22)
    .build()
    .await?;

let server = server::Server::start(ssh).await?;
println!("invite: a3net-ssh alice@{}", server.endpoint_id());
# Ok(()) }
# }
```

The `endpoint_id()` is what you share with friends in chat. The
identity file is the same one the rest of A3Net uses, so this
id also matches the `a3net` node id shown by `a3net info`.

`Server::shutdown()` is the matching tear-down — it flips the
per-connection watch channel and waits for the iroh `Router` to
close.

### 2. Client side — connect with system `ssh(1)`

`a3net-ssh` doesn't bundle an SSH client on purpose; it leans on
whatever SSH you already have installed. The crate provides the
ProxyCommand glue:

```text
# ~/.ssh/config
Host *.a3net.local
    ProxyCommand a3net-ssh proxy %h
```

With that in place:

```bash
$ ssh alice@<endpoint-id>  # or ssh alice@<endpoint-id>.a3net.local
```

Under the hood `a3net-ssh proxy <token>` resolves the
`user@<endpoint-id>` token, opens a QUIC stream to the ALPN
`a3net/ssh-tunnel/1`, spawns the system `ssh(1)` in proxy mode
(`-T -e none -o ProxyUseFdpass=no -o BatchMode=yes`), and
bidirectionally copies bytes until either side hangs up.

### 3. SFTP

The same tunnel carries SFTP — the SSH protocol's file-transfer
subsystem. The peer's sshd must have an `sftp-server`
`Subsystem` registered (this is the default for any
OpenSSH-derived server).

```rust,no_run
# #[cfg(feature = "iroh")] {
# async fn demo() -> anyhow::Result<()> {
use a3net_ssh::client::proxy::{run_sftp, SftpConfig};

let config = SftpConfig {
    binary: "sftp".into(),
    recursive: true,
    preserve: true,
    subsystem: "sftp".into(),
};
run_sftp(
    "alice@<endpoint-id>",
    std::path::Path::new("./.a3net-data"),
    &config,
    &["/remote/path", "/local/path"],
).await?;
# Ok(()) }
# }
```

The wrapper spawns the local `sftp` with
`-o ProxyUseFdpass=no -o BatchMode=yes` and bridges stdin/stdout
the same way `run` does for `ssh`.

### 4. Programmatic access — without spawning `ssh`

Embedded callers that want to talk the SSH protocol themselves
(no subprocess) can ask for the raw stream pair:

```rust,ignore
use a3net_ssh::client;

// High level: discover the peer via the configured discovery layer.
let (send, recv) = client::connect(&endpoint, target_id).await?;

// Even lower: when you have an `EndpointAddr` from a ticket and
// want to skip discovery entirely.
let (send, recv) = client::connect_with_addr(&endpoint, addr).await?;
```

## Persistent identity

The crate deliberately **does not** mint its own Ed25519 key; it
reuses what A3Net already has.

| file                                | contents                         | lifecycle                              |
|-------------------------------------|----------------------------------|----------------------------------------|
| `<data-dir>/iroh_secret_key`        | 32-byte Ed25519 secret key       | created on first `IrohSshBuilder::build`, reused forever |

The same key powers the `a3net/frame/1` ALPN, the gossip layer,
and the blob store — so the SSH endpoint id is *always* equal
to the A3Net node id printed by `a3net info`. There is no second
`irohssh_ed25519` file sitting next to it.

[`keys::persistent_identity`](keys::persistent_identity) is the
single place this resolution happens.

## Authentication

`a3net-ssh` ships two auth mechanisms out of the box. They live
under `keys::authorized_keys` (`AuthorizedKeys`,
`AuthorizedKeyEntry`) and `keys::authorized_keys::TrustedPeers`.

### `authorized_keys`

A standard OpenSSH `~/.ssh/authorized_keys` file with one A3Net
extension: the `from-a3net=<endpoint-id>` option.

```text
# Standard OpenSSH form (key blob authenticated by the SSH protocol)
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH… alice@laptop

# A3Net form: restrict to one A3Net peer regardless of IP.
from-a3net=38b7dc10df96005255c3beaeaeef6cfebd88344aa8c85e1dbfc1ad5e50f372ac \
    ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH… bob@phone

# Combine the standard OpenSSH `from=` IP restriction with the
# A3Net peer restriction.
from-a3net=38b7dc10…, from="10.0.0.0/8" \
    ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILiH… carol@home
```

`AuthorizedKeys::check(key_blob, peer_endpoint_id)` honours
`from-a3net` first (skip entries whose pinned id does not match
the connecting peer), then base64-decodes the stored blob and
compares it to the peer's public key blob. Quote / option parsing
is handled by a regex tokenizer that understands both
`name="quoted value"` and `name=value` forms.

For convenience, `AuthorizedKeys::add_peer(endpoint_id, key_blob,
comment)` appends a fresh `from-a3net=…` line; the `ssh/`
directory and the file are created on demand.

### `trusted_peers`

A lightweight friends-list: one `EndpointId` per line in
`<data-dir>/ssh/trusted_peers`. `#`-prefixed comments and blank
lines are ignored. The in-memory cache is reloadable via
`TrustedPeers::reload()` and is refreshed automatically every
30 seconds by `check()`.

```rust,ignore
use a3net_ssh::keys::authorized_keys::TrustedPeers;

let tp = TrustedPeers::new("./.a3net-data");
tp.add(peer_id)?;
assert!(tp.check(peer_id));
tp.remove(peer_id).ok();
```

This is the layer `a3net` would call from "add friend" / "remove
friend" UX without round-tripping through the user-editable
`authorized_keys` file.

## Features

| feature | default | what it gates                                                |
|---------|---------|--------------------------------------------------------------|
| `iroh`  | **off** | `IrohSshBuilder`, `Server`, `client::{connect,proxy}`, `authorized_keys` |

Without `iroh`, the crate compiles to identity types + error
types only; `IrohSshBuilder::build` returns
[`SshError::FeatureMissing`]. This is intentional — it lets the
default build stay free of the iroh dependency tree, while
keeping the documentation build (`cargo doc -p a3net-ssh`) green.

To use the runtime:

```bash
cargo build -p a3net-ssh --features iroh
cargo test  -p a3net-ssh --features iroh
```

## Errors

Every fallible API in the crate returns
[`SshResult<T> = Result<T, SshError>`](error::SshResult).

| variant                                                          | cause                                                                            |
|------------------------------------------------------------------|----------------------------------------------------------------------------------|
| [`SshError::Identity { path, source }`](error::SshError::Identity) | The persistent iroh identity file could not be loaded / created.                 |
| [`SshError::NoSshServer { port }`](error::SshError::NoSshServer)  | Local `sshd` is not listening on `port` within the probe timeout (default 10 s). |
| [`SshError::InvalidInvite { input, source }`](error::SshError::InvalidInvite) | `user@<endpoint>` token failed parsing (missing `@`, empty user, bad encoding). |
| [`SshError::SpawnSsh { binary, source }`](error::SshError::SpawnSsh) | Failed to spawn `ssh` (or `sftp`). Usually means the binary is missing from `PATH`. |
| [`SshError::Tunnel(String)`](error::SshError::Tunnel)             | A QUIC stream or ALPN handshake failed mid-flight.                                |
| [`SshError::FeatureMissing`](error::SshError::FeatureMissing)     | Operation requires the `iroh` feature but the crate was built without it.        |
| [`SshError::Other(String)`](error::SshError::Other)               | Catch-all for errors the crate deliberately does not translate.                  |

The variants are deliberately narrow — they map 1:1 to the
decisions a CLI/REPL has to make ("is sshd running?", "should
I tell the operator to enable the feature?", etc.).

## Testing

```bash
cargo test -p a3net-ssh                  # default: types + no-iroh stubs
cargo test -p a3net-ssh --features iroh  # full suite (incl. SSH-tunnel loopback)
```

Tests cover:

- **`SshError` Display strings** — locked in for log scrapers.
- **`keys::resolve_data_dir`** — `Some(_)`, `None`, `Some("")`.
- **`SSH_TUNNEL_ALPN` byte-string stability** — once this changes
  we lose compatibility with every deployed server.
- **`parse_invite`** — valid hex/z-base-32 forms, wrong encoding,
  wrong length, missing `@`, empty user.
- **`parse_one_authorized_key`** — quoted and unquoted options,
  `from-a3net`, multiple options, comments, blanks.
- **`AuthorizedKeys`** — empty-dir loads, `ensure()` creates
  `ssh/`, comment + blank skipping, `add_peer` writes a
  well-formed line.
- **`TrustedPeers`** — empty file, `add` then `check`, `list`,
  `remove` returning `Ok(true)` then `Ok(false)` for a second
  call.
- **`probe_local_ssh`** — refuses to start when sshd is absent.
- **ProxyCommand `run`/`run_with`** — `JoinHandle`-by-ownership
  via `tokio::join!`, symmetric in-flight gauge.

Hermetic `iroh`-feature integration smoke tests skip themselves
when no relay is reachable; the unit-test layer above is the
authoritative coverage.

## Design notes

### Why a separate ALPN (`a3net/ssh-tunnel/1`)?

The rest of the runtime speaks `a3net/frame/1`, the framed
transport. SSH tunnels want a raw byte stream, not the framed
multiplexer, and naming the SSH ALPN under `a3net/` rather than
overloading `frame/1` ensures:

- a generic A3Net peer can never accidentally `accept_bi()` an
  SSH-tunnel connection;
- the iroh router can run both ALPNs on the same `Endpoint`,
  which means the persistent identity is genuinely shared.

### Why an in-process auth layer instead of PAM / sshd config?

A3Net is a peer-to-peer overlay; the set of "trusted peers" is
the set of node ids in your mesh, not the set of Unix users on
your box. A dedicated auth layer that parses a familiar
`authorized_keys` format but adds `from-a3net=<id>` (peer id
allow-list) keeps operators in familiar OpenSSH territory while
matching A3Net's identity model. The `trusted_peers` list is the
ergonomic shortcut for "add this friend".

### Why `sftp` over `scp`?

`scp(1)` is deprecated upstream and its wire format is hostile
to proxies. SFTP is an SSH subsystem — meaning the existing
ProxyCommand bridge already wires it through to the remote's
`sftp-server`. `run_sftp` is a 50-line wrapper around the same
byte-copying primitive `run` uses.

### Why a separate `proxy_bridge` helper per surface?

`run` (SSH) and `run_sftp` (SFTP) differ only in the spawned
binary and its command-line flags. The byte-copying helper is
deliberately factored out so the two paths can evolve
independently without copy-paste drift; the in-flight gauge is
decremented symmetrically (success and failure) by a single
`proxy_bridge_inner` function.

## License

MIT OR Apache-2.0 — same as the workspace root
(`workspace.package.license`).
