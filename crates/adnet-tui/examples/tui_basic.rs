//! Minimal example — render a node status panel and a progress bar.
//!
//! Run with:
//!   cargo run -p adnet-tui --example tui_basic

use adnet_tui::{Box, Color, ProgressBar, t};

fn main() {
    let panel = Box::with_title(t("status.title"))
        .add_field(t("status.node_id").as_str(), "12D3KooWABCDEF1234567890ABCDEF")
        .add_field(t("status.status").as_str(), Color::Green.paint(t("status.online")))
        .add_field(t("status.peer_count").as_str(), "42");

    println!("{panel}");
    println!();

    let bar = ProgressBar::with_total(1_073_741_824)
        .current(536_870_912)
        .prefix("Syncing blob")
        .width(30);

    println!("{bar}");
}
