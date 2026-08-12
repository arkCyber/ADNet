# adnet-tui

> Zero-dependency terminal UI primitives for ADNet CLI — ASCII boxes, progress bars, colors and i18n.

## 概览 (Overview)

`adnet-tui` is the rendering layer that every ADNet command-line tool
ultimately prints through. It deliberately avoids the heavyweight TUIs
(`crossterm`, `ratatui`, `tui-rs`) — instead it is a **pure-Rust ANSI
printer** that produces strings you can `println!`, redirect to a file,
or pipe into `less`. The crate is split into four independent modules:

- **`box_drawing`** — bordered panels with titles and labelled
  key/value fields. Supports `Single` (─ │ ┌ ┐), `Double` (═ ║ ╔ ╗),
  `Simple` (`+`/`-`/`|`) and `None` border styles.
- **`color`** — ANSI 16-color palette with automatic TTY detection
  (`NO_COLOR`, `TERM=dumb`, non-tty stdout). Falls back to plain text
  when the consumer is a pipe or file.
- **`progress`** — percentage progress bars and spinners with
  human-readable byte counters (`KiB` / `MiB` / `GiB`).
- **`i18n`** — embedded translations for English and Simplified Chinese
  with `{0}`/`{1}` placeholder substitution.

The crate is `forbid(unsafe_code)` and exposes a single
`use adnet_tui::*;` import for the common widgets.

## 特性 (Features)

- **Bordered panels** (`Box::with_title`, `Box::add_field`)
  - Single, double, simple and borderless variants
  - Coloured title and per-field `StyledStr` values
- **Tabular output** (`Table::with_headers`, `Table::add_row`)
  - Auto-sized columns, zebra striping toggle, cyan headers
- **Color helpers** (`Color::Red`, `Color::Green`, …, `Style::success()`)
  - `paint()`, `on()`, `bold()`, `dim()`, `italic()`, `underline()`
  - `is_enabled()` reports the active TTY detection result
- **Progress indicators** (`ProgressBar`, `Spinner`)
  - Colour shifts: green < 70 %, yellow < 90 %, red ≥ 90 %
  - `human_bytes(2_457_600) → "2.34 MiB"`
  - `format_number(1_000_000) → "1,000,000"`
- **Internationalisation** (`t("status.title")`, `t_with_args`)
  - `Locale::En` and `Locale::ZhCn`
  - `set_locale(Locale::ZhCn)` switches at runtime
- **Pre-built widgets** (`widget::status_widget`, `widget::alert_widget`,
  `widget::help_text`, `widget::metrics_summary`)

## 安装 (Installation)

`adnet-tui` is a workspace-internal crate. It is consumed by
`adnet-cli` (the user-facing CLI). To depend on it from a new
workspace crate:

```toml
[dependencies]
adnet-tui = { workspace = true }
```

## 使用 (Usage)

Render a status panel:

```rust
use adnet_tui::{Box, Color, t};

let panel = Box::with_title(t("status.title"))
    .add_field(t("status.node_id"), "12D3KooWABCDEF…")
    .add_field(t("status.status"), Color::Green.paint(t("status.online")));

println!("{panel}");
```

Render a progress bar:

```rust
use adnet_tui::ProgressBar;

let bar = ProgressBar::with_total(1_073_741_824)
    .current(536_870_912)
    .prefix("Syncing blob")
    .width(30);

println!("{bar}");
// Syncing blob [███████████████░░░░░░░░░░░░░░░]  50.0%   512.00 MiB / 1.00 GiB
```

Render a colour-coded table with zebra stripes:

```rust
use adnet_tui::{Table, widget::status_widget};

let mut table = Table::with_headers(["Peer", "Status"]);
table.add_row(["12D3KooW…alice", status_widget("online").plain_text()]);
table.add_row(["12D3KooW…bob",   status_widget("offline").plain_text()]);
table.add_row(["12D3KooW…carol", status_widget("warn").plain_text()]);

println!("{table}");
```

Switch language at runtime:

```rust
use adnet_tui::i18n::{set_locale, Locale, t};

set_locale(Locale::ZhCn);
println!("{}", t("status.title"));     // "ADNet 节点状态"
println!("{}", t("wizard.save_success")); // "配置已保存到 {0}"
```

## 应用案例 (Use Cases / Examples)

- **`adnet-cli` status screen** — `Box::with_title` + `add_field` renders
  the persistent node status panel that surfaces data directory, peer
  count and replication health.
- **Long-running import / export** — wrap `ProgressBar` around the blob
  import loop so the operator sees a percentage and byte count instead
  of a spinning cursor.
- **Bilingual error messages** — when a node falls back from English to
  Chinese, the CLI calls `set_locale(Locale::ZhCn)` and every
  subsequent `t("…")` resolves to the right translation without
  recompiling.
- **CI logs** — because `color::is_enabled()` returns `false` when
  stdout is not a TTY, the same `println!("{panel}")` produces plain
  text in CI logs without any `--no-color` flag plumbing.
- **Alert banners** — `widget::alert_widget("warn", "Storage nearly full")`
  gives the CLI a single helper that picks the right icon (⚠ / ❌ / ⛔)
  and color (yellow / red) for every severity level.

## 许可

MIT OR Apache-2.0
