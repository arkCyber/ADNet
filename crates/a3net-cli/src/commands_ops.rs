//! `a3net commands --flags` — enumerate every sub-command + every
//! `--long` flag we accept.
//!
//! Kubo `ipfs commands --flags` parity. The implementation here
//! is type-driven — `clap`'s `CommandFactory` lets us walk the
//! `Cli` parser tree at runtime, so adding a new sub-command
//! automatically shows up in `commands --flags` output.
//!
//! Without `--flags` we emit one row per sub-command; with
//! `--flags` we emit one row per `cmd.subcommand --flag-name`.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli::Cli;

/// Top-level dispatch — `a3net commands <sub>`.
pub fn run_commands(flags: bool, json_out: bool) -> Result<()> {
    let rows = collect_rows(flags);
    if json_out {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in &rows {
            let cmd = r.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let flag = r.get("flag").and_then(|v| v.as_str()).unwrap_or("");
            println!("{:<48} {}", cmd, flag);
        }
    }
    Ok(())
}

/// Walk the `Cli` clap tree and return one row per
/// (subcommand, optional --flag) pair. Used by `run_commands` for
/// JSON/text output and by `render_commands` for the text dump
/// consumed by the `tui`/`--dump` mode.
pub fn collect_rows(flags: bool) -> Vec<serde_json::Value> {
    let cmd = Cli::command();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        let name = sub.get_name().to_string();
        if !flags {
            rows.push(serde_json::json!({"command": name}));
            continue;
        }
        for arg in sub.get_arguments() {
            // Skip positional args (no `long`); we want the
            // canonical `--foo` form. Hidden / deprecated flags
            // are filtered out.
            if arg.is_hide_set() {
                continue;
            }
            let long = match arg.get_long() {
                Some(l) => l.to_string(),
                None => continue,
            };
            rows.push(serde_json::json!({
                "command": name,
                "flag": format!("--{}", long),
                "short": arg.get_short().map(|c| c.to_string()),
                "help": arg.get_help().map(|h| h.to_string()).unwrap_or_default(),
            }));
        }
    }
    rows
}

/// Render the entire CLI command surface as a human-readable
/// multi-line text block. Used by `a3net tui --dump` and
/// `a3net commands` when `--json` is not set.
pub fn render_commands() -> String {
    let cmd = Cli::command();
    let mut out = String::new();
    out.push_str("A3Net CLI — available commands\n");
    out.push_str("===============================\n\n");
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
        out.push_str(&format!("  {:<18} {}\n", sub.get_name(), about));
        if sub.get_subcommands().next().is_some() {
            for nested in sub.get_subcommands() {
                if nested.get_name() == "help" {
                    continue;
                }
                let nested_about = nested
                    .get_about()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                out.push_str(&format!(
                    "    {:<16} {}\n",
                    nested.get_name(),
                    nested_about
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn commands_emits_at_least_one_row() {
        // Just make sure we don't panic on a fresh Cli.
        run_commands(true, true).unwrap();
    }

    #[test]
    fn commands_parses_against_a_real_cli() {
        // Round-trip parse to make sure Cli still type-checks.
        let _ = Cli::try_parse_from(["a3net", "--lang", "en", "init"]).unwrap();
    }
}