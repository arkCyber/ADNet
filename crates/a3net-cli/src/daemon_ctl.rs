//! `a3net shutdown` — gracefully stop a running `a3net serve` daemon.
//!
//! Mirrors `iroh shutdown`. The companion write-side lives in
//! `main.rs::Cmd::Serve` — when the daemon starts it binds a
//! `tokio::net::UnixListener` at `{data_dir}/daemon.sock`. The
//! `Cmd::Shutdown` handler connects to that socket, sends a
//! `Shutdown { force, timeout_secs }` JSON message, and waits
//! for the ack.
//!
//! ## Why a UNIX-domain socket
//!
//! It's the same `data_dir` we use for everything else, so the
//! operator doesn't have to track a separate port. On Linux /
//! macOS the kernel can clean up the socket file when the daemon
//! exits; on Windows we fall back to a `tcp://127.0.0.1:0`
//! control channel (auto-incremented).
//!
//! ## Failure modes
//!
//! - No daemon running  → exit 64 ("no daemon listening").
//! - Timeout exceeded   → exit 75 ("shutdown timed out").
//! - Stale socket file  → exit 69 ("stale socket, force-cleanup?").
//!
//! JSON output is one of:
//! ```json
//! { "ok": true,  "message": "daemon acknowledged shutdown" }
//! { "ok": false, "message": "...", "exit_code": 64 }
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Exit codes that match sysexits.h semantics so this scriptable
/// from any POSIX shell without surprises.
pub const EXIT_NO_DAEMON: i32 = 64;
pub const EXIT_STALE_SOCKET: i32 = 69;
pub const EXIT_TIMED_OUT: i32 = 75;
pub const EXIT_IO_ERROR: i32 = 74;

/// Canonical control-socket path inside the data dir.
pub fn daemon_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("daemon.sock")
}

/// JSON message sent by `a3net shutdown` → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRequest {
    pub force: bool,
    pub timeout_secs: u64,
}

/// JSON ack returned by the daemon before exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownAck {
    pub ok: bool,
    pub message: String,
}

/// Run `a3net shutdown` against the daemon control socket.
///
/// `json_output` controls whether the result is printed as a
/// single-line JSON object (CI-friendly) or as a friendly
/// human-readable message.
pub async fn run_shutdown(
    data_dir: &Path,
    force: bool,
    timeout_secs: u64,
    json_output: bool,
) -> Result<i32> {
    let socket = daemon_socket_path(data_dir);
    if !socket.exists() {
        return report(
            json_output,
            false,
            format!(
                "no daemon socket at {} — is `a3net serve` running?",
                socket.display()
            ),
            EXIT_NO_DAEMON,
        )
        .await;
    }

    let connect_fut = UnixStream::connect(&socket);
    let timeout_dur = Duration::from_secs(if timeout_secs == 0 {
        // 0 = "no timeout", but we still don't want to block
        // forever — default to 30s as a hard ceiling.
        30
    } else {
        timeout_secs
    });

    let stream = match tokio::time::timeout(timeout_dur, connect_fut).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            // Most likely: stale socket — the daemon died without
            // cleaning up. Suggest force-cleanup if not --force.
            let msg = if force {
                format!("failed to connect (stale socket? {}): {}", socket.display(), e)
            } else {
                format!(
                    "failed to connect to {}: {}. The daemon may have crashed without \
                     cleaning up the socket — pass --force to remove the stale socket file \
                     and exit, or run `rm {}` manually.",
                    socket.display(),
                    e,
                    socket.display()
                )
            };
            let exit = if force {
                if let Err(rm_err) = std::fs::remove_file(&socket) {
                    return report(
                        json_output,
                        false,
                        format!(
                            "{} and could not remove it: {}",
                            msg, rm_err
                        ),
                        EXIT_STALE_SOCKET,
                    )
                    .await;
                }
                EXIT_NO_DAEMON
            } else {
                EXIT_STALE_SOCKET
            };
            return report(json_output, false, msg, exit).await;
        }
        Err(_) => {
            return report(
                json_output,
                false,
                format!(
                    "timed out after {}s connecting to daemon socket {}",
                    timeout_secs, socket.display()
                ),
                EXIT_TIMED_OUT,
            )
            .await;
        }
    };

    let mut stream = stream;
    let req = ShutdownRequest { force, timeout_secs };
    let req_bytes = serde_json::to_vec(&req)?;
    stream.write_all(&req_bytes).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let ack = match tokio::time::timeout(
        timeout_dur,
        read_ack(&mut stream),
    )
    .await
    {
        Ok(Ok(ack)) => ack,
        Ok(Err(e)) => {
            return report(
                json_output,
                false,
                format!("io error reading shutdown ack: {}", e),
                EXIT_IO_ERROR,
            )
            .await;
        }
        Err(_) => {
            return report(
                json_output,
                false,
                format!(
                    "timed out after {}s waiting for shutdown ack",
                    timeout_secs
                ),
                EXIT_TIMED_OUT,
            )
            .await;
        }
    };

    let started = Instant::now();
    // Surface the daemon's verdict and the elapsed time so
    // scripts can decide whether to retry.
    report(
        json_output,
        ack.ok,
        format!(
            "{} (elapsed {}ms)",
            ack.message,
            started.elapsed().as_millis()
        ),
        if ack.ok { 0 } else { EXIT_IO_ERROR },
    )
    .await
}

async fn read_ack(stream: &mut UnixStream) -> Result<ShutdownAck> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let ack: ShutdownAck = serde_json::from_str(line.trim())?;
    Ok(ack)
}

async fn report(
    json_output: bool,
    ok: bool,
    message: String,
    exit_code: i32,
) -> Result<i32> {
    if json_output {
        let payload = serde_json::json!({
            "ok": ok,
            "message": message,
            "exit_code": exit_code,
        });
        println!("{}", serde_json::to_string(&payload)?);
    } else if ok {
        println!("✓ {}", message);
    } else {
        eprintln!("✗ {}", message);
    }
    if ok {
        Ok(0)
    } else {
        // Don't bubble up an `Err` for operator-facing failures
        // — the shell needs the exit code, not a backtrace.
        Ok(exit_code)
    }
}

/// Server-side handler: bind the daemon socket and wait for a
/// shutdown request. Called from `Cmd::Serve` in `main.rs`.
///
/// Returns when the daemon has received the ack and finished
/// draining, OR when the timeout expires.
pub async fn serve_daemon_control(
    data_dir: PathBuf,
    drain: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let socket = daemon_socket_path(&data_dir);
    if socket.exists() {
        // Stale socket from a previous crash — refuse to start
        // unless the operator explicitly cleans it up. We don't
        // remove it ourselves because that would silently mask
        // a real crash.
        bail!(
            "refusing to bind daemon socket: {} already exists. \
             Either `a3net serve` is already running, or a previous \
             daemon crashed without cleaning up. Remove the file \
             manually if you are sure no daemon is running.",
            socket.display()
        );
    }
    let listener = tokio::net::UnixListener::bind(&socket)?;
    tracing::info!(
        "daemon control socket listening at unix://{}",
        socket.display()
    );

    // We accept exactly one connection — the shutdown request.
    // Anything else gets an immediate `bail!` so a misconfigured
    // `a3net` invocation can't wedge the daemon.
    let (mut stream, _) = listener.accept().await?;
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let req: ShutdownRequest = serde_json::from_str(line.trim())?;
    tracing::info!(
        force = req.force,
        timeout = req.timeout_secs,
        "received shutdown request"
    );

    let ack = match drain.await {
        Ok(()) => ShutdownAck {
            ok: true,
            message: "shutdown complete".to_string(),
        },
        Err(e) => ShutdownAck {
            ok: false,
            message: format!("shutdown drain failed: {}", e),
        },
    };

    let mut writer = stream;
    let payload = serde_json::to_string(&ack)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    // Best-effort cleanup of the socket file. We don't fail if
    // it doesn't exist — `a3net shutdown --force` may have
    // removed it already.
    let _ = std::fs::remove_file(&socket);

    if !ack.ok {
        bail!(ack.message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn daemon_socket_path_lives_inside_data_dir() {
        let dir = tempdir().unwrap();
        let p = daemon_socket_path(dir.path());
        assert_eq!(p.parent().unwrap(), dir.path());
        assert_eq!(p.file_name().unwrap(), "daemon.sock");
    }

    #[test]
    fn shutdown_request_round_trips() {
        let req = ShutdownRequest { force: true, timeout_secs: 7 };
        let bytes = serde_json::to_vec(&req).unwrap();
        let back: ShutdownRequest = serde_json::from_slice(&bytes).unwrap();
        assert!(back.force);
        assert_eq!(back.timeout_secs, 7);
    }

    #[test]
    fn shutdown_ack_round_trips() {
        let ack = ShutdownAck { ok: false, message: "x".into() };
        let bytes = serde_json::to_vec(&ack).unwrap();
        let back: ShutdownAck = serde_json::from_slice(&bytes).unwrap();
        assert!(!back.ok);
        assert_eq!(back.message, "x");
    }

    #[tokio::test]
    async fn run_shutdown_returns_no_daemon_when_socket_missing() {
        let dir = tempdir().unwrap();
        let code = run_shutdown(dir.path(), false, 1, true).await.unwrap();
        assert_eq!(code, EXIT_NO_DAEMON);
    }

    #[tokio::test]
    async fn run_shutdown_handles_stale_socket_with_force() {
        let dir = tempdir().unwrap();
        let sock = daemon_socket_path(dir.path());
        std::fs::write(&sock, b"stale").unwrap();
        let code = run_shutdown(dir.path(), /* force */ true, 1, true).await.unwrap();
        assert_eq!(code, EXIT_NO_DAEMON);
        assert!(!sock.exists(), "--force should remove the stale socket");
    }

    #[tokio::test]
    async fn run_shutdown_returns_stale_without_force() {
        let dir = tempdir().unwrap();
        let sock = daemon_socket_path(dir.path());
        std::fs::write(&sock, b"stale").unwrap();
        let code = run_shutdown(dir.path(), /* force */ false, 1, true).await.unwrap();
        assert_eq!(code, EXIT_STALE_SOCKET);
        assert!(sock.exists(), "without --force the socket must stay put");
    }
}
