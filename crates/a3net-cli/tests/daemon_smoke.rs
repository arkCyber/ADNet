//! End-to-end smoke test for `a3net daemon`.
//!
//! Spawns the real `a3net` binary as a child process on a temp
//! data dir, waits for the JSON-RPC Unix socket to appear, fires
//! an `info` RPC, then signals a clean shutdown via the legacy
//! `{data_dir}/daemon.sock` and reaps the process.
//!
//! Run with:
//! ```bash
//! cargo test -p a3net-cli --test daemon_smoke -- --include-ignored --nocapture
//! ```
//!
//! Both tests are marked `#[ignore]` so they don't run in the
//! default `cargo test` invocation — they spawn a long-lived
//! child process and bind sockets. CI should opt in with
//! `--include-ignored`.

#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

const ADNET_BIN_ENV: &str = "ADNET_BIN";

/// Locate the `a3net` binary under test. Honours `ADNET_BIN` (CI
/// override) or falls back to the workspace's `target/debug/a3net`.
fn a3net_bin() -> PathBuf {
    if let Ok(p) = std::env::var(ADNET_BIN_ENV) {
        return PathBuf::from(p);
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("target").join("debug").join("a3net");
        if candidate.exists() {
            return candidate;
        }
        let candidate_release = ancestor.join("target").join("release").join("a3net");
        if candidate_release.exists() {
            return candidate_release;
        }
    }
    let workspace_root = manifest_dir
        .ancestors()
        .nth(1)
        .unwrap_or(&manifest_dir)
        .to_path_buf();
    workspace_root.join("target").join("debug").join("a3net")
}

/// Wait until `connect(path)` succeeds, polling every 50ms.
fn wait_for_connect(path: &PathBuf, timeout: Duration) -> Result<UnixStream> {
    let start = Instant::now();
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                bail!("connect {} after {:?}: {e}", path.display(), timeout);
            }
        }
    }
}

/// Send a single JSON-RPC request over `stream` and read one
/// newline-delimited response.
fn rpc_round_trip(stream: &mut UnixStream, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    use std::io::{Read, Write};
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut buf = serde_json::to_vec(&req)?;
    buf.push(b'\n');
    stream.write_all(&buf)?;
    stream.flush()?;

    let mut resp = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("eof reading response");
        }
        resp.extend_from_slice(&chunk[..n]);
        if resp.contains(&b'\n') {
            break;
        }
    }
    let line: Vec<u8> = resp.into_iter().take_while(|b| *b != b'\n').collect();
    let v: serde_json::Value = serde_json::from_slice(&line)?;
    Ok(v)
}

/// Spawn `a3net daemon` with stdout/stderr redirected to files
/// in the data dir. Pipes are dangerous — a child that writes
/// more than the OS buffer (typically 64 KiB) will block forever
/// waiting for the parent to drain them, and the parent is busy
/// with JSON-RPC.
fn spawn_daemon(data_dir: &PathBuf, rpc_socket: &PathBuf, auto_join: &[&str]) -> Result<Child> {
    let bin = a3net_bin();
    if !bin.exists() {
        bail!(
            "a3net binary not found at {} — run `cargo build -p a3net-cli` first",
            bin.display()
        );
    }
    let stdout_path = data_dir.join("daemon.stdout.log");
    let stderr_path = data_dir.join("daemon.stderr.log");
    let stdout = std::fs::File::create(&stdout_path)
        .with_context(|| format!("create stdout log {}", stdout_path.display()))?;
    let stderr = std::fs::File::create(&stderr_path)
        .with_context(|| format!("create stderr log {}", stderr_path.display()))?;
    let mut cmd = Command::new(&bin);
    cmd.env_remove("RUST_LOG");
    cmd.arg("daemon")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--rpc-socket")
        .arg(rpc_socket)
        .arg("--metrics-addr")
        .arg("")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for room in auto_join {
        cmd.arg("--auto-join").arg(room);
    }
    let child = cmd.spawn().context("spawn a3net daemon")?;
    Ok(child)
}

/// Wait for the daemon to exit on its own. If it doesn't, SIGKILL
/// after 8s and reap. Returns the exit status.
fn reap(mut child: Child) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if start.elapsed() > Duration::from_secs(8) => {
                let _ = child.kill();
                // Block on wait — the child is dying.
                return child.wait().expect("wait after kill");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return child.wait().expect("wait on err"),
        }
    }
}

/// Snapshot the daemon's stdout/stderr log files for diagnostics.
fn dump_daemon_logs(data_dir: &PathBuf) -> String {
    let stderr_log =
        std::fs::read_to_string(data_dir.join("daemon.stderr.log")).unwrap_or_default();
    let stdout_log =
        std::fs::read_to_string(data_dir.join("daemon.stdout.log")).unwrap_or_default();
    format!(
        "\n--- daemon stdout ---\n{stdout_log}\n--- daemon stderr ---\n{stderr_log}\n--- end ---"
    )
}

/// Drive the daemon through one full lifecycle: boot, JSON-RPC
/// round-trip, auto-join observed, clean shutdown via the legacy
/// `daemon.sock`. Runs end-to-end against the real `a3net` binary.
#[test]
#[ignore = "spawns a3net child process; run with --include-ignored"]
fn daemon_full_lifecycle() -> Result<()> {
    let dir = TempDir::new()?;
    let data_dir = dir.path().to_path_buf();
    let rpc_socket = data_dir.join("ipc.sock");
    let legacy_sock = data_dir.join("daemon.sock");

    let child = spawn_daemon(&data_dir, &rpc_socket, &["lobby"])?;

    // 1) JSON-RPC round trip + auto-join visible in info/list_rooms.
    let mut stream = wait_for_connect(&rpc_socket, Duration::from_secs(15))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let info = rpc_round_trip(&mut stream, "info", serde_json::json!({}))?["result"].clone();
    assert!(!info["nodeId"].as_str().unwrap_or("").is_empty(), "nodeId should be present: {info}");
    assert_eq!(info["joinedRooms"].as_array().map(|a| a.len()), Some(1));

    let rooms = rpc_round_trip(&mut stream, "list_rooms", serde_json::json!({}))?["result"].clone();
    let rooms = rooms.as_array().context("list_rooms not an array")?;
    assert_eq!(rooms.len(), 1, "expected one auto-joined room, got {rooms:?}");
    assert_eq!(rooms[0], "lobby");

    drop(stream);

    // 2) Legacy shutdown protocol — write the request by hand
    // (we can't recurse into `a3net shutdown` in its own test
    // tree).
    let mut ls = wait_for_connect(&legacy_sock, Duration::from_secs(15))?;
    ls.set_read_timeout(Some(Duration::from_secs(5)))?;
    ls.set_write_timeout(Some(Duration::from_secs(5)))?;
    use std::io::Write;
    let mut buf = serde_json::to_vec(&serde_json::json!({
        "force": false,
        "timeout_secs": 5,
    }))?;
    buf.push(b'\n');
    ls.write_all(&buf)?;
    ls.flush()?;

    // Read the ack. Give the daemon up to 5s — if the ack never
    // arrives, dump logs and bail.
    use std::io::Read;
    let mut ack = Vec::new();
    let read_start = Instant::now();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = ls.read(&mut chunk) {
        if n == 0 {
            break;
        }
        ack.extend_from_slice(&chunk[..n]);
        if ack.contains(&b'\n') {
            break;
        }
        if read_start.elapsed() > Duration::from_secs(5) {
            bail!(
                "timeout reading shutdown ack (got: {:?})\n{}",
                String::from_utf8_lossy(&ack),
                dump_daemon_logs(&data_dir),
            );
        }
    }
    let ack_str = String::from_utf8_lossy(&ack);
    let ack_json: serde_json::Value = serde_json::from_str(ack_str.trim())
        .with_context(|| format!("parse shutdown ack: {ack_str}\n{}", dump_daemon_logs(&data_dir)))?;
    assert_eq!(ack_json["ok"], true, "shutdown ack should be ok: {ack_json}");

    // 3) Daemon should exit on its own within a couple seconds.
    let status = reap(child);
    eprintln!("[test] daemon exited: status={status:?}");
    eprintln!("[test] daemon logs:\n{}", dump_daemon_logs(&data_dir));
    assert!(status.success(), "daemon did not exit cleanly: {status:?}");
    Ok(())
}
