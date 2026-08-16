//! `a3net status` command — convenience wrapper around
//! `storage::run_status` exposed as its own top-level
//! subcommand so operators can type `a3net status`
//! without going through `a3net storage …`.

use std::path::Path;

use a3net_tui::{
    box_drawing::{alert_box, status_panel, Box},
    color::{Color, StyledStr},
    progress::human_bytes,
    widget::{alert_widget, section_header},
};
use anyhow::{Context, Result};

use crate::storage::run_status as storage_run_status;

/// Dispatch `a3net status [--json]`. Equivalent to
/// `a3net storage info --json` plus replication counters
/// from the global metric registry. Offline-only — does
/// not start the node.
pub fn run_status(data_dir: &Path, json: bool) -> Result<()> {
    storage_run_status(data_dir, json)
        .with_context(|| format!("status snapshot for {}", data_dir.display()))
}

/// Run status with rich TUI output (human-readable).
pub fn run_status_rich(data_dir: &Path) -> Result<()> {
    use crate::storage::{build_dashboard, open_topology};

    let topo = open_topology(data_dir)
        .with_context(|| format!("open storage topology at {}", data_dir.display()))?;
    let dash = build_dashboard(&topo, 3, 300);

    // Title
    println!();
    println!(
        "{}",
        Color::Cyan
            .paint("╔═══════════════════════════════════════════════════════════════════╗")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("║             A3Net Node Status                                     ║")
            .bold()
    );
    println!(
        "{}",
        Color::Cyan
            .paint("╚═══════════════════════════════════════════════════════════════════╝")
            .bold()
    );
    println!();

    // Data directory
    println!(
        "{}  {}",
        Color::Yellow.paint("📁").bold(),
        Color::White.paint("Data Directory:").bold()
    );
    println!("    {}", data_dir.display());

    // Storage section
    println!();
    println!("{}", section_header("Storage"));

    let private_pct = if dash.storage.private_hard_cap_bytes > 0 {
        dash.storage.private_used_bytes as f64 / dash.storage.private_hard_cap_bytes as f64
    } else {
        0.0
    };
    let shared_pct = if dash.storage.shared_hard_cap_bytes > 0 {
        dash.storage.shared_used_bytes as f64 / dash.storage.shared_hard_cap_bytes as f64
    } else {
        0.0
    };

    let private_bar = progress_bar(private_pct);
    let shared_bar = progress_bar(shared_pct);

    println!(
        "  Private: {}",
        human_bytes(dash.storage.private_used_bytes)
    );
    println!(
        "           {}/{}  {}",
        human_bytes(dash.storage.private_hard_cap_bytes),
        private_bar,
        format!("{:.0}%", private_pct * 100.0)
    );
    println!("           {} blobs", dash.storage.private_blobs);
    println!();
    println!("  Shared:  {}", human_bytes(dash.storage.shared_used_bytes));
    println!(
        "           {}/{}  {}",
        human_bytes(dash.storage.shared_hard_cap_bytes),
        shared_bar,
        format!("{:.0}%", shared_pct * 100.0)
    );
    println!("           {} blobs", dash.storage.shared_blobs);

    // Replication section
    println!();
    println!("{}", section_header("Replication"));
    println!(
        "  Factor: {}   Sweeps: {}   Blocks: {}",
        dash.replication.factor,
        dash.replication.sweeps_total,
        dash.replication.blocks_pushed_total
    );
    println!(
        "  Errors: {}   Under-replicated: {}   Fully-replicated: {}",
        dash.replication.push_errors_total,
        dash.replication.under_replicated_blocks,
        dash.replication.fully_replicated_blocks
    );

    // Alerts section
    println!();
    if dash.alerts.is_empty() {
        println!("{}", Color::Green.paint("✓ No alerts").bold());
    } else {
        println!("{}", section_header("Alerts"));
        for alert in &dash.alerts {
            let level_str = match alert.level {
                a3net_observability::dashboard::AlertLevel::Critical => "CRITICAL",
                a3net_observability::dashboard::AlertLevel::Warn => "WARNING",
                a3net_observability::dashboard::AlertLevel::Info => "INFO",
            };
            let icon = match alert.level {
                a3net_observability::dashboard::AlertLevel::Critical => "⛔",
                a3net_observability::dashboard::AlertLevel::Warn => "⚠",
                a3net_observability::dashboard::AlertLevel::Info => "ℹ",
            };
            let color = match alert.level {
                a3net_observability::dashboard::AlertLevel::Critical => Color::Red,
                a3net_observability::dashboard::AlertLevel::Warn => Color::Yellow,
                a3net_observability::dashboard::AlertLevel::Info => Color::Cyan,
            };
            println!(
                "  {} [{}] {}",
                color.paint(icon).bold(),
                color.paint(level_str),
                alert.message
            );
        }
    }

    println!();
    println!(
        "  {}  Updated: {}",
        Color::Dim.paint("ℹ"),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!();

    Ok(())
}

/// Run status with compact single-line output.
pub fn run_status_compact(data_dir: &Path) -> Result<()> {
    use crate::storage::{build_dashboard, open_topology};

    let topo = open_topology(data_dir)
        .with_context(|| format!("open storage topology at {}", data_dir.display()))?;
    let dash = build_dashboard(&topo, 3, 300);

    let private_pct = if dash.storage.private_hard_cap_bytes > 0 {
        dash.storage.private_used_bytes as f64 / dash.storage.private_hard_cap_bytes as f64
    } else {
        0.0
    };
    let shared_pct = if dash.storage.shared_hard_cap_bytes > 0 {
        dash.storage.shared_used_bytes as f64 / dash.storage.shared_hard_cap_bytes as f64
    } else {
        0.0
    };

    let alerts = dash.alerts.len();
    let alert_str = if alerts == 0 {
        Color::Green.paint("OK").to_string()
    } else {
        Color::Yellow
            .paint(format!("{} alerts", alerts))
            .to_string()
    };

    let time = chrono::Utc::now().format("%H:%M:%S").to_string();

    println!(
        "[{}] P:{}/{}({:.0}%) S:{}/{}({:.0}%) | {} | {}",
        time,
        human_bytes(dash.storage.private_used_bytes),
        human_bytes(dash.storage.private_hard_cap_bytes),
        private_pct * 100.0,
        human_bytes(dash.storage.shared_used_bytes),
        human_bytes(dash.storage.shared_hard_cap_bytes),
        shared_pct * 100.0,
        alert_str,
        dash.replication.factor
    );

    Ok(())
}

/// Generate a simple progress bar using ASCII characters.
fn progress_bar(ratio: f64) -> String {
    let width = 20;
    let filled = (ratio.clamp(0.0, 1.0) * width as f64) as usize;
    let empty = width - filled;

    let bar: StyledStr =
        StyledStr::plain(&format!("[{}{}]", "█".repeat(filled), "░".repeat(empty)));

    if ratio >= 0.9 {
        Color::Red.paint(bar.ansi())
    } else if ratio >= 0.7 {
        Color::Yellow.paint(bar.ansi())
    } else {
        Color::Green.paint(bar.ansi())
    }
    .to_string()
}
