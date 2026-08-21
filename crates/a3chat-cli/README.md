# a3chat-cli

Operator command-line client for the **a3chat** distributed communication
stack. Built to aerospace-grade (DO-178C) standards.

```
$ a3chat --help
Operator CLI for the a3chat distributed communication stack.

Commands:
  whoami        Print the configured local owner identity
  doctor        Probe the running daemon and report its health
  conversation  Conversation commands
  message       Message commands
  sync          Sync (multi-device) commands
  profile       Profile / public-key / device commands
  chat          Interactive multi-turn conversation session (slash commands + SSE)
  contact       Contact commands (friend / blocklist / QR-invite)
  group         Group commands (create / invite / membership / mute / nickname)
  moments       Moments commands (朋友圈 — post / comment / reaction / follow)
  link          Link bookmark commands (favorites / folders / search)
  media         Media commands (blob upload / download / health)
  moderation    Moderation commands (content / attachment policy gate)
  presence      Presence commands (publish / subscribe)
  bundle        Bundle commands (export / import E2E state bundle)
  stream        Stream commands (subscribe / unsubscribe / list event streams)
  trace         Subscribe to the daemon SSE event stream
  rpc           Raw JSON-RPC fallback — call any a3chat.* method
  audit         Static / live audit of the a3chat API surface
  config        Config introspection
  repl          Interactive REPL
  completions   Generate shell completion script
```

---

## Why this exists

`a3chat` ships a Tauri desktop shell, a Flutter mobile client, and an HTTP
JSON-RPC daemon — but no scripted way to drive it from CI or operator
terminals. `a3chat-cli` fills that gap with:

1. **Audit** — offline static audit of the API surface, used in CI to
   detect schema drift before a release.
2. **Doctor** — three-call health probe (conversation.list, with retry)
   that surfaces the daemon's error class.
3. **Conversation / Message / Sync / Profile** — operator-friendly wrappers for
   every `a3chat.chat.*` / `a3chat.profile.*` RPC method.
4. **Config** — TOML-driven, deterministic, env-overridable.

## DO-178C mappings

| Principle | Where it lives |
|---|---|
| **Traceability (§5.2)** | `X-A3Chat-Request-Id` header on every call; mirrored in `tracing` logs |
| **Determinism (§6.1)** | All output formatters produce byte-stable output (sorted keys, stable column widths); `audit` is byte-stable across runs |
| **Fail-safe (§6.3)** | Exit codes map to `ErrorClass`: `EX_TEMPFAIL` (75) for `Transient`, `EX_SOFTWARE` (70) for `Internal`, `EX_CANTCREAT` (73) for `Io` |
| **Reproducibility (§7.2)** | `sync snapshot` writes SHA-256 sidecars (`<file>.sha256`) next to every output |
| **Defensive programming (§8)** | `--owner` is validated as a 64-char hex string before any request is sent |

## Subcommand reference

### `whoami`
Print the configured local owner identity and the daemon URL it's
pointing at. Exits non-zero (visible in `a3chat doctor`) if the owner
is the all-zeros placeholder.

### `doctor`
Probes `/rpc/health`-equivalent endpoints (currently any successful
`conversation.list` is enough to prove the daemon is listening).
Reports each probe's outcome as `ok` / `transient` / `fail`.

### `conversation`
- `conversation list` — `a3chat.chat.conversation.list`
- `conversation open --conversation-id <id>` — `a3chat.chat.conversation.open`

### `message`
| Subcommand | RPC method | Notes |
|---|---|---|
| `message send` | `a3chat.chat.message.send` | Supports `--dry-run` |
| `message ack` | `a3chat.chat.message.ack` | Idempotent |
| `message recall` | `a3chat.chat.message.recall` | Sender-only |
| `message edit` | `a3chat.chat.message.edit` | Sender-only |
| `message delete` | `a3chat.chat.message.delete` | Local-only |
| `message search` | `a3chat.chat.search` | Auto-skips encrypted bodies |
| `message typing` | `a3chat.chat.typing` | Best-effort |

### `sync`
- `sync snapshot [--out file] [--sidecar]` — dumps the local snapshot
  and writes a SHA-256 sidecar next to it.
- `sync delta --cursors '<json>' [--out file]` — incremental delta.
- `sync compressed --out file` — base64-decoded zstd snapshot.

### `profile`
| Subcommand | RPC method | Notes |
|---|---|---|
| `profile get` | `a3chat.profile.get` | `--dry-run` prints the envelope |
| `profile digit` | `a3chat.profile.digit_get` | 12-digit ID; exits non-zero on malformed reply |
| `profile keys` | `a3chat.profile.public_key_list` | — |
| `profile devices` | `a3chat.profile.device_list` | — |
| `profile set-avatar --blob-hash <hex> --mime <m> --size <n>` | `a3chat.profile.avatar_set` | Validates `blob_hash` (1..=128 hex) + `size != 0` && `size <= 10 MiB` |

> `profile` is the only wrapper surface that includes explicit input
> validation; the daemon trusts the operator.

### `chat` — interactive conversation session

```bash
a3chat chat --conversation-id dm:alice:bob         # join an existing DM
a3chat chat --to <user_id>                         # open or create the DM
a3chat chat --history 100                          # replay 100 msgs on open
a3chat chat --idle-timeout-secs 60                 # auto-quit after 60s idle
a3chat chat --dry-run                              # echo resolved options, no RPC
```

Inside the session, stdin is read line-by-line; non-empty lines
become `a3chat.chat.message.send`. Lines beginning with `/` are
slash-commands:

| Slash command            | Effect                                          |
|--------------------------|-------------------------------------------------|
| `/help`                  | print the in-session command list               |
| `/quit`, `/exit`         | leave the session                               |
| `/history [n]`           | re-play the last `n` (default 50) messages     |
| `/recall <msg-id>`       | recall a message you sent                       |
| `/ack <msg-id>`          | acknowledge a received message                  |
| `/edit <msg-id> <txt>`   | edit a message body                             |
| `/delete <msg-id>`       | delete a message locally                        |
| `/search <needle>`       | search the conversation                         |
| `/typing`                | emit a typing indicator to the peer             |
| `/status`                | print session stats (msgs sent/received)        |

### `contact`

| Subcommand                              | RPC method                              | Notes |
|-----------------------------------------|-----------------------------------------|-------|
| `contact list`                          | `a3chat.contact.list`                   | — |
| `contact add --to <id> --message "…"`   | `a3chat.contact.add_request`            | message ≤ 256 chars |
| `contact add-direct --user-id … --display-name …` | `a3chat.contact.add`            | — |
| `contact accept --request-id …`         | `a3chat.contact.accept_request`         | — |
| `contact block --user-id …`             | `a3chat.contact.block`                  | — |
| `contact unblock --user-id …`           | `a3chat.contact.unblock`                | — |
| `contact remove --user-id …`            | `a3chat.contact.remove`                 | — |
| `contact get --user-id …`               | `a3chat.contact.get`                    | — |
| `contact search --query …`              | `a3chat.contact.search`                 | — |
| `contact toggle-favorite --user-id …`   | `a3chat.contact.toggle_favorite`        | — |
| `contact update --user-id … --display-name …` | `a3chat.contact.update`          | — |
| `contact qr-invite`                     | `a3chat.contact.qr_invite`              | returns base64 payload |
| `contact qr-invite-render [--output qr.svg] [--caption "…"]` | `a3chat.contact.qr_invite` | writes SVG to `--output` |

### `group`

29 subcommands covering lifecycle, membership, invitation, mute,
nickname, and mention-parsing. Full dispatch in
`crates/a3chat-cli/src/cmd/group.rs`. Examples:

```bash
a3chat group create --name team-a --description "core team" --is-private=true
a3chat group invite --conversation-id <cid> --invitee-id <user> --group-name team-a --inviter-name alice
a3chat group members --conversation-id <cid>
a3chat group role --conversation-id <cid> --user-id <u> --role admin
a3chat group mute-member --conversation-id <cid> --user-id <u> --indefinite
a3chat group mention-parse --body "ping @alice" --nicknames "<alice_uid>:alice"
```

### `moments`

15 subcommands for the 朋友圈 surface:

```bash
a3chat moments node-info
a3chat moments post --text "Hello world" --visibility public
a3chat moments posts-by --user-id <u>
a3chat moments timeline --limit 50
a3chat moments comment --post-id <p> --text "nice!"
a3chat moments react --target-id <p> --reaction-type like
a3chat moments follow --who <u>
a3chat moments verify-post --post-id <p>
```

### `link`

14 bookmark / favorite subcommands:

```bash
a3chat link add https://example.com --title "Example" --tags rust,docs
a3chat link list --folder work --limit 100
a3chat link search "rust async"
a3chat link pin <bookmark_id>
a3chat link touch <bookmark_id>
```

### `media`

```bash
a3chat media health
TOKEN=$(a3chat media upload-init --mime image/png | jq -r .token)
a3chat media upload-chunk --token "$TOKEN" --file chunk1.bin
a3chat media upload-finalize --token "$TOKEN" --filename photo.png
a3chat media download-get --hash <blake3_hex> --out ./photo.png
```

### `moderation`

```bash
a3chat moderation check-content --text "<utf-8 text>"
a3chat moderation check-attachment --hash <blake3_hex>
a3chat moderation list-blocked
a3chat moderation set-deny-default --on=true
a3chat moderation stats
```

### `presence`

```bash
a3chat presence publish --status online --message "at desk"
a3chat presence subscribe --peers <uid1>,<uid2>
```

### `bundle`

```bash
a3chat bundle export --out backup.a3b          # AEAD-encrypted state bundle
a3chat bundle import --in backup.a3b           # decrypt + merge on this node
```

### `stream`

```bash
a3chat stream subscribe --topic "*"            # acquire a handle
a3chat stream list                             # show every active subscription
a3chat stream unsubscribe --handle-id <id>     # release
```

### `audit`
Pure offline report. Outputs:

```json
{
  "summary": {
    "total_methods": 39,
    "total_errors": 8,
    "total_invariants": 7,
    "passed": 7,
    "failed": 0,
    "cli_supported": 12,
    "cli_unsupported": 27
  },
  "method_inventory": [...],
  "error_inventory": [...],
  "schema_invariants": [...]
}
```

Exits non-zero if any schema invariant fails — wire this into CI.

`audit` has three modes:

| Mode | Daemon required? | What it does |
|---|---|---|
| `audit static` | No | Pure compile-time check of the API surface, schema invariants, and wire-code uniqueness. |
| `audit live`  | Yes | Probes every `a3chat.*` method and classifies each as `implemented` / `method_not_found` / `stub_no_handler` / `transient` / `internal`. |
| `audit full`  | Yes | Combines both reports in a single JSON document. |

Example output (`audit live`):

```json
{
  "daemon_url": "http://127.0.0.1:53421",
  "passed": 12,
  "failed": 28,
  "errors": 0,
  "methods": [
    {"method": "a3chat.chat.conversation.list", "outcome": "implemented", "detail": ""},
    {"method": "a3chat.media.upload_init",      "outcome": "stub_no_handler", "detail": "..."}
  ]
}
```

### `rpc <method>` — raw JSON-RPC fallback

Every subcommand above is a wrapper around `rpc`. For methods
that don't yet have a dedicated subcommand (`contact.*`, `group.*`,
`presence.*`, `media.*`, `e2e.*`) call them directly:

```bash
a3chat rpc a3chat.contact.list                  '{}'
a3chat rpc a3chat.contact.add_request           '{"to_user_id":"…","message":"hi"}'
a3chat rpc a3chat.group.create                  '{"name":"team-a","members":[…]}'
a3chat rpc a3chat.presence.publish              '{"state":"online"}'
a3chat rpc a3chat.media.upload_init             '{}'
```

The method name is validated against `A3chatRpcMethod::ALL` so
a typo returns an error without ever reaching the daemon. Use
`a3chat rpc methods` to list every known name, grouped by
namespace.

### `trace` — SSE event subscription

Subscribe to the daemon's Server-Sent Events stream and print
each notification as it arrives:

```bash
a3chat trace follow                                # follow forever
a3chat trace follow --max-events 10                # stop after 10 events
a3chat trace follow --filter a3chat.chat.message   # only message events
a3chat trace follow --idle-timeout-secs 60         # abort after 60s of silence
a3chat trace follow --max-duration-secs 300        # wall-clock bound
a3chat trace follow --compact | jq                # one JSON object per line
```

`trace events` lists every notification kind the daemon may
emit. Useful for sanity-checking what the server's vocabulary
covers.

### `repl` — interactive shell

```bash
$ a3chat repl
a3chat 0.1.0 — type `help` for commands, `exit` to quit.
daemon: http://127.0.0.1:53421  owner: 01234567…cdef
a3chat> methods
a3chat.chat.conversation.list
a3chat.chat.conversation.open
…
a3chat> a3chat.chat.conversation.list {}
[]
a3chat> a3chat.chat.typing {"conversation_id":"dm:a:b"}
"typing"
a3chat> exit
```

REPL commands: `help`, `methods`, `version`, `exit` / `quit`
/ `:q`, and `<method> <json-args>`. Unknown methods and bad
JSON are reported but do **not** exit the loop.

### `completions` — shell completion

```bash
# bash
source <(a3chat completions bash)

# zsh
a3chat completions zsh > "${fpath[1]}/_a3chat"

# fish
a3chat completions fish | source
```

### `config`
- `config show` — print the resolved config as JSON.
- `config path` — print the platform-default config file location.

---

## Configuration

`a3chat` reads a TOML config from one of:

1. `--config <path>` flag
2. `$A3CHAT_CONFIG` env var
3. `${XDG_CONFIG_HOME:-~/.config}/a3chat/config.toml`

```toml
# ~/.config/a3chat/config.toml
daemon_url = "http://127.0.0.1:53421"
owner       = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
output      = "table"   # table | json | plain
retries     = 3
timeout_ms  = 30000
```

All fields are optional; CLI flags always win.

## Exit code mapping (DO-178C §6.3)

| Class | Exit | C value |
|---|---|---|
| `Usage` | 2 | EX_USAGE |
| `Rpc` (transient) | 75 | EX_TEMPFAIL |
| `Rpc` (other) | 1 | — |
| `Config` / `Internal` | 70 | EX_SOFTWARE |
| `Io` | 73 | EX_CANTCREAT |
| `Crypto` | 77 | — |

---

## Test summary

`a3chat-cli` itself runs **48 unit tests** across:

- `config` / `rpc_client` / `output` / `audit_report` / `error` /
  `repl` / `completions` (determinism, retry classification, code
  mapping, schema invariants, suggestion coverage).

End-to-end (`e2e_rpc` / `e2e_advanced`) and property suites
(`property_output` / `property_backoff` / `property_config`) live
alongside via `cargo test -p a3chat-cli`.

Across all `a3chat-*` crates:

| Crate | Tests |
|---|---|
| `a3chat-app` | 177 |
| `a3chat-rpc` | 73 |
| `a3chat-cli` | 48 |
| `a3chat-core` | 85 |
| `a3chat-crypto` | 42 |
| **Total** | **425** |

All pass, 0 failures (last verified: 2026-08-16).

---

## Error diagnostics (DO-178C §6.3)

Every error carries an actionable suggestion. Examples:

```
$ a3chat rpc a3chat.no.such.method '{}'
error: rpc: internal error: A3chatApp does not handle method a3chat.no.such.method
hint:  the daemon returned an unknown JSON-RPC error; verify the daemon version matches the CLI
```

```
$ a3chat --daemon-url http://127.0.0.1:0 doctor
error: rpc: network error: …
hint:  transient transport error — exit code is EX_TEMPFAIL; retry the command or check the daemon is running
```

Suggestions are unit-tested — every `A3chatError` variant has at
least one sentence of remediation.

---

## Audit report structure

`a3chat audit static` emits this JSON shape (output is deterministic
across runs):

```json
{
  "generated_at_unix": 1734329400,
  "method_inventory": [
    {
      "method": "a3chat.chat.conversation.list",
      "group": "conversation",
      "cli_support": "direct",
      "has_real_handler": true
    },
    {
      "method": "a3chat.media.upload_init",
      "group": "media",
      "cli_support": "stub",
      "has_real_handler": false
    }
  ],
  "error_inventory": [
    {
      "variant": "CryptoError",
      "class": "Security",
      "wire_code": -32103,
      "retryable": false
    }
  ],
  "schema_invariants": [
    {
      "name": "MAX_NAME_LEN",
      "value": "128",
      "ok": true,
      "note": "must be in [1, 1024]"
    }
  ],
  "workspace_invariants": [
    {
      "name": "methods_a3chat_prefix",
      "value": "39 of 39 methods prefixed",
      "ok": true,
      "note": "every method in A3chatRpcMethod::ALL must start with 'a3chat.'"
    }
  ],
  "summary": {
    "total_methods": 39,
    "total_errors": 8,
    "total_invariants": 7,
    "total_workspace_invariants": 6,
    "passed": 13,
    "failed": 0,
    "cli_supported": 12,
    "cli_unsupported": 27,
    "stub_methods": 7,
    "real_handlers": 32
  }
}
```

Stub methods are explicitly enumerated in
`audit_report::STUB_METHODS`. If you ship a handler for one of
them, remove it from that list to keep `cli_support` accurate.

## License

MIT OR Apache-2.0
