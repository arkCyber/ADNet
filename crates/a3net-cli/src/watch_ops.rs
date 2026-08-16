//! `a3net watch <secs> -- <command>` — re-run any offline `a3net`
//! sub-command on a fixed interval (Kubo `ipfs watch` parity).
//!
//! The forwarded command runs in a child process; we re-spawn the
//! currently-running `a3net` binary with the supplied args. This
//! sidesteps the borrow checker (we can't easily share an `a3net_node::Node`
//! across a `tokio::select!` loop while also consuming `std::process::Command`)
//! and gives us "real" command behaviour including config / env handling.
//!
//! Stdin is currently `Stdio::null()` on every child — the parent's
//! stdin never reaches the children. See the inline note in
//! [`run_watch`] for the upgrade path.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Top-level dispatch — `a3net watch <secs> -- <args>`.
///
/// Returns immediately on a malformed interval or empty argument
/// list. Otherwise it loops forever, spawning one child per tick,
/// until the process receives SIGINT / SIGTERM.
pub async fn run_watch(interval_secs: u64, args: &[String]) -> Result<()> {
    if !(1..=3600).contains(&interval_secs) {
        anyhow::bail!("watch: interval_secs must be in 1..=3600 (got {interval_secs})");
    }
    if args.is_empty() {
        anyhow::bail!("watch: missing subcommand (use `a3net watch 5 -- <cmd>`)");
    }
    eprintln!(
        "watch: re-running `a3net {}` every {interval_secs}s (Ctrl-C to stop)…",
        args.join(" ")
    );

    let exe = std::env::current_exe().context("watch: cannot resolve own executable path")?;
    let interval = Duration::from_secs(interval_secs);

    // Stdin strategy: each child gets `Stdio::null()` so a piped parent
    // (e.g. `echo hello | a3net watch 5 -- cat -`) doesn't block on
    // the child's stdin read. This means stdin from the watcher
    // invocation never reaches the child. A future enhancement could
    // replay the parent's stdin (or a tee'd snapshot) into the
    // child via `Stdio::piped()` + async copy — see the
    // `tokio::io::copy` hint in the module header.
    loop {
        let start = Instant::now();
        let mut child = Command::new(&exe)
            .args(args)
            .env("ADNET_WATCH_ITER", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("watch: failed to spawn {}", exe.display()))?;

        if let Some(stdout) = child.stdout.take() {
            // Stream the child's stdout line-by-line so the operator
            // sees live output, then wait for completion.
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                println!("{line}");
            }
        }

        let status = child.wait().context("watch: failed to wait on child")?;
        if !status.success() {
            eprintln!(
                "watch: child exited with code {:?} (continuing in {}s)",
                status.code(),
                interval_secs
            );
        }

        // Sleep the remainder of the interval, accounting for child runtime.
        let elapsed = start.elapsed();
        if elapsed < interval {
            tokio::time::sleep(interval - elapsed).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watch_rejects_bad_interval() {
        // 0 is too low.
        assert!(run_watch(0, &["status".into()]).await.is_err());
        // 3601 is too high.
        assert!(run_watch(3601, &["status".into()]).await.is_err());
    }

    #[tokio::test]
    async fn watch_rejects_empty_args() {
        assert!(run_watch(1, &[]).await.is_err());
    }
}
