# a3net-cli

> A3Net 命令行入口 — CLI front-end for the A3Net NAS runtime.
> The `a3net` binary is the canonical user-facing tool for inspecting a node,
> publishing / fetching blobs, listing room feeds, and driving the interactive
> REPL.

## 概览 (Overview)

`a3net-cli` wires the `a3net` binary together. It exposes a `clap`-derived
subcommand tree, parses every global flag (`--data-dir`, `--config`, `--lang`,
`--log-filter`, …), and dispatches each invocation to the matching subsystem
across the workspace — gossip, blob store, namespace, share, social feed,
news, smarthome, etc.

The crate is intentionally split into two surfaces:

- **Public library** (`pub mod …`) — every command module is reachable
  so embedders and integration tests can `use a3net_cli::{Cli, Cmd, …}`
  and re-use the same parser without spawning a child process.
- **Binary** (`src/main.rs`) — the `a3net` command line proper; maps a
  parsed `Cli` to the right `a3net_node::Node` runtime call.

Internally the CLI never touches the network directly. It calls the
`a3net-node` runtime, which in turn talks to `a3net-blobstore`,
`a3net-gossip`, `a3net-namespace`, `a3net-share`, `a3net-roster`,
`a3net-news`, `a3net-tui`, etc. The CLI is the place where user-facing
verbs (`announce`, `feed`, `pin`, `share`, `send`) get mapped to typed
runtime APIs.

## 特性 (Features)

- Kubo-compatible verbs: `add`, `get`, `cat`, `ls`, `pin`, `repo`, `swarm`,
  `dht`, `routing`, `bitswap`, `name`, `keygen`, `channel`.
- A3Net verbs: `announce`, `feed`, `echo`, `status`, `diagnostics`,
  `bandwidth`, `profile`, `share`, `roster`, `news`, `moments`, `mdns`.
- P2P social: `news`, `moments`, `roster`, `social feed`, `userstore`.
- Configuration management: `config show | set | reset | edit | wizard`.
- i18n: `--lang en | zh-CN` switches the in-REPL language.
- Interactive REPL: `a3net run` drops into a `/cmd` shell that remembers
  history and supports `/help`, `/connect`, `/ssh server`, etc.
- Diagnostics: `a3net diagnostics --json` produces a structured snapshot
  for ops/integration tests.

## 安装 (Installation)

The crate is a workspace member and is exposed as a path dependency:

```toml
[dependencies]
a3net-cli = { workspace = true }
```

Run the binary with:

```bash
cargo run -p a3net-cli -- <flags>
```

Each example is also a runnable binary:

```bash
cargo run -p a3net-cli --example programmatic
cargo run -p a3net-cli --example app_run_workflow
```

## 使用 (Usage)

### Programmatic parsing

```rust
use a3net_cli::{Cli, Cmd};
use clap::Parser;

let cli = Cli::try_parse_from(["a3net", "init"])?;
match cli.cmd {
    Cmd::Init => println!("init: printing node id"),
    Cmd::Announce { room, file, title, kind } => {
        println!("announce {title} into {room} from {file} (kind={kind})");
    }
    _ => {}
}
```

### Build an ad-hoc blob ticket

```rust
use a3net_types::{BlobTicket, NodeAddr, NodeId, ContentHash};

let me = NodeId::random();
let addr = NodeAddr::new(me.clone());
let hash = ContentHash::from_bytes(b"hello");
let ticket = BlobTicket::whole(&me, &addr, &hash);
let raw = ticket.encode();
let parsed = BlobTicket::parse(&raw)?;
assert_eq!(parsed.node_id, me);
```

### Feed projection

```rust
use a3net_cli::feed_view::feed_for_humans;
use a3net_node::RoomFeed;

fn pretty_print(feed: &RoomFeed) -> String {
    serde_json::to_string_pretty(&feed_for_humans(feed)).unwrap()
}
```

### Run the REPL

```bash
a3net run --data-dir ./.a3net-data
# inside the REPL:
# /help
# /connect <node-id>
# /status
```

## 应用案例 (Use Cases / Examples)

1. **Photo room for a family** — `a3net announce --room home-photos
   --file ./photos/holiday.jpg --title "Holiday 2026" --kind image` from
   each family member's NAS; the rest of the household reads the feed with
   `a3net feed --room home-photos` and pulls individual assets with
   `a3net get <cid> -o ./holiday.jpg`. Demonstrated in
   `examples/app_run_workflow.rs`.

2. **CI/CD artifact distribution** — `a3net init` once on the build
   agent, then `a3net announce --room releases --file ./dist/app.tar.gz
   --title "v1.4.0" --kind generic_file` after every green build.
   Downstream consumers hit `a3net feed --room releases` from their
   runners; resolved blobs are pulled by `a3net get <cid>`. The CLI's
   `/cfg` command and `a3net-cli::config` module cover the
   configuration side of the same pipeline.

3. **Self-hosted AI model hub** — `a3net announce --room llms
   --file ./models/llama3-8b-q4.gguf --kind ai_model` from the
   operator's NAS; consumers list with `a3net feed --room llms` and
   download via `a3net get <cid>`. The `a3net-model-catalog` crate
   wraps the same idea with a per-asset metadata index and a built-in
   web UI; the CLI is the lightweight alternative.

## 许可

MIT OR Apache-2.0
