# adnet-cli

> ADNet 命令行入口 — CLI front-end for the ADNet NAS runtime.
> The `adnet` binary is the canonical user-facing tool for inspecting a node,
> publishing / fetching blobs, listing room feeds, and driving the interactive
> REPL.

## 概览 (Overview)

`adnet-cli` wires the `adnet` binary together. It exposes a `clap`-derived
subcommand tree, parses every global flag (`--data-dir`, `--config`, `--lang`,
`--log-filter`, …), and dispatches each invocation to the matching subsystem
across the workspace — gossip, blob store, namespace, share, social feed,
news, smarthome, etc.

The crate is intentionally split into two surfaces:

- **Public library** (`pub mod …`) — every command module is reachable
  so embedders and integration tests can `use adnet_cli::{Cli, Cmd, …}`
  and re-use the same parser without spawning a child process.
- **Binary** (`src/main.rs`) — the `adnet` command line proper; maps a
  parsed `Cli` to the right `adnet_node::Node` runtime call.

Internally the CLI never touches the network directly. It calls the
`adnet-node` runtime, which in turn talks to `adnet-blobstore`,
`adnet-gossip`, `adnet-namespace`, `adnet-share`, `adnet-roster`,
`adnet-news`, `adnet-tui`, etc. The CLI is the place where user-facing
verbs (`announce`, `feed`, `pin`, `share`, `send`) get mapped to typed
runtime APIs.

## 特性 (Features)

- Kubo-compatible verbs: `add`, `get`, `cat`, `ls`, `pin`, `repo`, `swarm`,
  `dht`, `routing`, `bitswap`, `name`, `keygen`, `channel`.
- ADNet verbs: `announce`, `feed`, `echo`, `status`, `diagnostics`,
  `bandwidth`, `profile`, `share`, `roster`, `news`, `moments`, `mdns`.
- P2P social: `news`, `moments`, `roster`, `social feed`, `userstore`.
- Configuration management: `config show | set | reset | edit | wizard`.
- i18n: `--lang en | zh-CN` switches the in-REPL language.
- Interactive REPL: `adnet run` drops into a `/cmd` shell that remembers
  history and supports `/help`, `/connect`, `/ssh server`, etc.
- Diagnostics: `adnet diagnostics --json` produces a structured snapshot
  for ops/integration tests.

## 安装 (Installation)

The crate is a workspace member and is exposed as a path dependency:

```toml
[dependencies]
adnet-cli = { workspace = true }
```

Run the binary with:

```bash
cargo run -p adnet-cli -- <flags>
```

Each example is also a runnable binary:

```bash
cargo run -p adnet-cli --example programmatic
cargo run -p adnet-cli --example app_run_workflow
```

## 使用 (Usage)

### Programmatic parsing

```rust
use adnet_cli::{Cli, Cmd};
use clap::Parser;

let cli = Cli::try_parse_from(["adnet", "init"])?;
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
use adnet_types::{BlobTicket, NodeAddr, NodeId, ContentHash};

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
use adnet_cli::feed_view::feed_for_humans;
use adnet_node::RoomFeed;

fn pretty_print(feed: &RoomFeed) -> String {
    serde_json::to_string_pretty(&feed_for_humans(feed)).unwrap()
}
```

### Run the REPL

```bash
adnet run --data-dir ./.adnet-data
# inside the REPL:
# /help
# /connect <node-id>
# /status
```

## 应用案例 (Use Cases / Examples)

1. **Photo room for a family** — `adnet announce --room home-photos
   --file ./photos/holiday.jpg --title "Holiday 2026" --kind image` from
   each family member's NAS; the rest of the household reads the feed with
   `adnet feed --room home-photos` and pulls individual assets with
   `adnet get <cid> -o ./holiday.jpg`. Demonstrated in
   `examples/app_run_workflow.rs`.

2. **CI/CD artifact distribution** — `adnet init` once on the build
   agent, then `adnet announce --room releases --file ./dist/app.tar.gz
   --title "v1.4.0" --kind generic_file` after every green build.
   Downstream consumers hit `adnet feed --room releases` from their
   runners; resolved blobs are pulled by `adnet get <cid>`. The CLI's
   `/cfg` command and `adnet-cli::config` module cover the
   configuration side of the same pipeline.

3. **Self-hosted AI model hub** — `adnet announce --room llms
   --file ./models/llama3-8b-q4.gguf --kind ai_model` from the
   operator's NAS; consumers list with `adnet feed --room llms` and
   download via `adnet get <cid>`. The `adnet-model-catalog` crate
   wraps the same idea with a per-asset metadata index and a built-in
   web UI; the CLI is the lightweight alternative.

## 许可

MIT OR Apache-2.0
