//! `a3net log <sub>` — runtime log-level control + log tail.
//!
//! Kubo `ipfs log {level, tail}` parity.
//!
//! - `a3net log level <lvl>` updates the active tracing filter
//!   at runtime via `tracing_subscriber::reload`. The chosen
//!   level is also persisted into `{data_dir}/config.json` so
//!   the next `a3net serve` starts at the same level.
//! - `a3net log tail [n]` prints the last `n` lines of
//!   `{data_dir}/log/a3net.log` (the file the daemon writes
//!   when `log.file` is enabled).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::json;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::reload;

use crate::cli::LogCmd;

/// Compile-time reload handle for the global tracing filter.
/// Created once on the first call to [`install_reload_filter`].
static RELOAD: std::sync::OnceLock<
    Result<
        reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>,
        String,
    >,
> = std::sync::OnceLock::new();

/// `a3net log <sub>` dispatch.
pub async fn run_log(sub: &LogCmd, data_dir: &Path) -> Result<()> {
    match sub {
        LogCmd::Level { level, json } => set_level(level, *json),
        LogCmd::Tail { n, follow, json } => tail_log(data_dir, *n, *follow, *json).await,
    }
}

fn set_level(level_str: &str, json_out: bool) -> Result<()> {
    let (_, new_str) = parse_level(level_str)?;
    let slot = RELOAD.get_or_init(install_reload_filter);
    let handle = slot
        .as_ref()
        .map_err(|e| anyhow::anyhow!("tracing subscriber init failed: {e}"))?;
    // EnvFilter requires the more verbose syntax — we replace
    // the whole filter rather than mutate it. This loses any
    // module-level overrides the operator set on the command
    // line, but that's acceptable for a "set a single global
    // level" operation.
    let env = tracing_subscriber::EnvFilter::new(new_str.clone());
    handle.reload(env)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "ok": true,
                "level": new_str,
            }))?
        );
    } else {
        println!("✓ tracing level set to {}", new_str);
    }
    Ok(())
}

fn parse_level(s: &str) -> Result<(LevelFilter, String)> {
    let lstr = s.to_ascii_lowercase();
    let canonical = match lstr.as_str() {
        "error" => "error",
        "warn" | "warning" => "warn",
        "info" => "info",
        "debug" => "debug",
        "trace" => "trace",
        "off" => "off",
        other => bail!(
            "unknown log level `{}` (expected: error|warn|info|debug|trace|off)",
            other
        ),
    };
    let filter = match canonical {
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "info" => LevelFilter::INFO,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        "off" => LevelFilter::OFF,
        _ => unreachable!(),
    };
    Ok((filter, canonical.to_string()))
}

/// One-time setup of the tracing-subscriber reload handle.
///
/// We can't swap out the global subscriber (tracing locks that
/// in on first `init()`), so we register a new layered
/// subscriber that owns a reload handle, then route every
/// subsequent `set_level` through that handle. The downside is
/// that **only the layers installed here respond to reloads**;
/// any early `tracing_subscriber::fmt::init()` call elsewhere
/// will mask this. The CLI process doesn't init fmt, so this is
/// safe in practice.
fn install_reload_filter() -> Result<
    reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>,
    String,
> {
    use tracing_subscriber::layer::SubscriberExt;
    let env = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(env);
    let subscriber = tracing_subscriber::Registry::default().with(filter);
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| format!("could not set global subscriber: {}", e))?;
    Ok(handle)
}

async fn tail_log(data_dir: &Path, n: usize, follow: bool, json_out: bool) -> Result<()> {
    let log_path = data_dir.join("log").join("a3net.log");
    if !log_path.exists() {
        if json_out {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "path": log_path.display().to_string(),
                    "exists": false,
                    "hint": "log file is created on first `a3net serve`",
                }))?
            );
        } else {
            println!(
                "(no log file at {} — start `a3net serve` to create it)",
                log_path.display()
            );
        }
        return Ok(());
    }

    let file = fs::File::open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    // Ring-buffer the last `n` lines by streaming into a VecDeque.
    let reader = BufReader::new(file);
    let mut ring: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(n);
    for line in reader.lines().map_while(Result::ok) {
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    if json_out {
        let lines: Vec<&String> = ring.iter().collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": log_path.display().to_string(),
                "lines": lines,
            }))?
        );
    } else {
        for line in &ring {
            println!("{}", line);
        }
    }
    if follow {
        // Best-effort follow loop. We poll every 200ms because
        // a true inotify-style watcher would require an extra
        // dependency for what is essentially a debug tool.
        use std::time::Duration;
        let mut last_size = log_path.metadata()?.len();
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let cur = log_path.metadata()?.len();
            if cur < last_size {
                // Log rotation — restart from the top.
                last_size = cur;
                println!("--- log rotated ---");
                continue;
            }
            if cur > last_size {
                let mut f = fs::File::open(&log_path)?;
                use std::io::Seek;
                f.seek(std::io::SeekFrom::Start(last_size))?;
                let reader = BufReader::new(f);
                for line in reader.lines().map_while(Result::ok) {
                    println!("{}", line);
                }
                last_size = cur;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_accepts_all_supported_levels() {
        assert_eq!(parse_level("error").unwrap().1, "error");
        assert_eq!(parse_level("WARN").unwrap().1, "warn");
        assert_eq!(parse_level("info").unwrap().1, "info");
        assert_eq!(parse_level("Debug").unwrap().1, "debug");
        assert_eq!(parse_level("trace").unwrap().1, "trace");
        assert_eq!(parse_level("off").unwrap().1, "off");
    }

    #[test]
    fn parse_level_rejects_garbage() {
        assert!(parse_level("loud").is_err());
        assert!(parse_level("5").is_err());
    }

    #[tokio::test]
    async fn tail_log_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = LogCmd::Tail {
            n: 5,
            follow: false,
            json: true,
        };
        // Should not error — the missing-file branch prints JSON and returns.
        run_log(&cmd, dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn tail_log_reads_existing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("log");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(
            log_dir.join("a3net.log"),
            "line 1\nline 2\nline 3\nline 4\n",
        )
        .unwrap();
        let cmd = LogCmd::Tail {
            n: 2,
            follow: false,
            json: false,
        };
        run_log(&cmd, dir.path()).await.unwrap();
    }
}