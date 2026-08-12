//! Real-world example — assemble a full CLI status screen from
//! `adnet-tui` primitives, mimicking what `adnet-cli` prints when
//! the operator runs `adnet status`.
//!
//! Run with:
//!   cargo run -p adnet-tui --example tui_app

use adnet_tui::i18n::{self, Locale, t};
use adnet_tui::widget::{alert_widget, help_text, section_header, status_widget, Table};
use adnet_tui::{Box, Color, ProgressBar};

fn main() {
    // Switch to Chinese so the same code path exercises the i18n
    // table. Comment out the next line to print English instead.
    i18n::set_locale(Locale::ZhCn);

    println!("{}", section_header(t("status.title").as_str()));
    println!();

    // ─── Identity panel ───────────────────────────────────────────
    let identity = Box::with_title(t("status.title"))
        .add_field(t("status.node_id").as_str(), "12D3KooWABCDEF…XYZ")
        .add_field(t("status.data_dir").as_str(), "/var/lib/adnet")
        .add_field(
            t("status.status").as_str(),
            Color::Green.paint(t("status.online")),
        )
        .add_field(t("status.peer_count").as_str(), "42");

    println!("{identity}");
    println!();

    // ─── Storage usage ────────────────────────────────────────────
    let used: u64 = 12 * 1024 * 1024 * 1024;
    let total: u64 = 100 * 1024 * 1024 * 1024;
    let storage = ProgressBar::with_total(total)
        .current(used)
        .prefix(t("storage.title"))
        .width(30);
    println!("{storage}");
    println!();

    // ─── Peer table ───────────────────────────────────────────────
    let mut peers = Table::with_headers(["Peer", "Role", "Status"]);
    peers.add_row(["12D3KooW…alice", "relay", status_widget("online").plain_text()]);
    peers.add_row(["12D3KooW…bob", "storage", status_widget("offline").plain_text()]);
    peers.add_row(["12D3KooW…carol", "peer", status_widget("warn").plain_text()]);
    peers.add_row(["12D3KooW…dave", "exit", status_widget("running").plain_text()]);

    println!("{peers}");
    println!();

    // ─── Alerts ───────────────────────────────────────────────────
    println!("{}", alert_widget("warn", "Storage usage above 70%"));
    println!("{}", alert_widget("critical", "Peer bob has been offline for 10 minutes"));
    println!("{}", alert_widget("info", "Node upgraded to v0.4.0"));
    println!();

    // ─── Help text ────────────────────────────────────────────────
    let commands = [
        ("adnet status", "Show node status"),
        ("adnet peers", "List connected peers"),
        ("adnet storage", "Show storage usage"),
        ("adnet logs", "Tail structured logs"),
    ];
    println!("{}", help_text(&commands));
}
