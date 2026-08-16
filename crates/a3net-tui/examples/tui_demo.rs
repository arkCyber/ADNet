//! End-to-end showcase of the `a3net-tui` public surface.
//!
//! This example demonstrates every widget exposed by the crate
//! (`Box`, `ProgressBar`, `Table`, color palette, section headers,
//! alert widget, i18n, status indicators) using only the current
//! API. Older revisions of this file referenced an animation
//! module, an interactive input helper, and an alt-screen guard
//! that were removed when the crate was scoped down to its
//! zero-dependency string-printer model.
//!
//! Run with:
//!   cargo run -p a3net-tui --example tui_demo

use a3net_tui::widget::{alert_widget, help_text, section_header, status_widget, Table};
use a3net_tui::{Box, Color, ProgressBar, t};
use a3net_tui::i18n::{self, Locale};

fn main() {
    // ── Section 1: status panel ────────────────────────────────────
    println!("{}", section_header(t("status.title").as_str()));
    println!();

    let panel = Box::with_title(t("diagnostics.title"))
        .add_field(t("status.node_id").as_str(), "12D3KooWDemoNode123456789ABCDEF")
        .add_field(t("status.status").as_str(), Color::Green.paint(t("status.online")))
        .add_field(t("status.peer_count").as_str(), "42")
        .add_field(t("storage.private").as_str(), "1.5 GiB");
    println!("{panel}");
    println!();

    // ── Section 2: progress bar (zero-dependency snapshot) ─────────
    println!("{}", section_header("Progress Bar"));
    println!();

    let mut pb = ProgressBar::with_total(1_073_741_824)
        .current(536_870_912)
        .prefix("Blob sync")
        .width(40);
    println!("{pb}");
    pb.set(1_000_000_000);
    println!("Updated: {pb}");
    println!();

    // ── Section 3: peer table with status widget colors ────────────
    println!("{}", section_header("Peer Status"));
    println!();

    let peers = [
        ("12D3KooW-alice", "relay", "online", "12ms"),
        ("12D3KooW-bob", "storage", "online", "8ms"),
        ("12D3KooW-carol", "peer", "warn", "45ms"),
        ("12D3KooW-dave", "exit", "offline", "N/A"),
    ];
    let mut table = Table::with_headers(["Peer ID", "Role", "Status", "Latency"]).zebra_stripe(true);
    for (peer, role, status, latency) in peers {
        // `status_widget` returns a `StyledStr`; `.plain_text()`
        // is used here so the table cell stays a plain `String`
        // and the zebra-striping stays readable in CI logs.
        table.add_row([peer, role, status_widget(status).plain_text(), latency]);
    }
    println!("{table}");
    println!();

    // ── Section 4: color palette (10 base colors + modifiers) ─────
    println!("{}", section_header("Color Palette"));
    println!();
    for (name, color) in [
        ("black", Color::Black),
        ("red", Color::Red),
        ("green", Color::Green),
        ("yellow", Color::Yellow),
        ("blue", Color::Blue),
        ("magenta", Color::Magenta),
        ("cyan", Color::Cyan),
        ("white", Color::White),
        ("bright", Color::Bright),
        ("dim", Color::Dim),
    ] {
        println!("  {}", color.paint(name));
    }
    println!();
    println!("  styled: {}", Color::Green.paint("bold").bold());
    println!();

    // ── Section 5: alert widget levels ─────────────────────────────
    println!("{}", section_header("Alerts"));
    println!();
    println!("{}", alert_widget("info", "snapshot completed"));
    println!("{}", alert_widget("warn", "peer 12D3KooW-carol high latency"));
    println!("{}", alert_widget("error", "blob fetch failed: timeout"));
    println!();

    // ── Section 6: help text (table-backed) ───────────────────────
    println!("{}", section_header("Help"));
    println!();
    println!("{}", help_text(&[
        ("1-4", "Run a peer snapshot"),
        ("r", "Refresh peer list"),
        ("q", "Quit"),
    ]));
    println!();

    // ── Section 7: i18n toggle (round-trip) ────────────────────────
    println!("{}", section_header("i18n"));
    println!();
    for loc in [Locale::En, Locale::ZhCn] {
        i18n::set_locale(loc);
        println!("[{:?}] {}", loc, t("status.title"));
    }
}