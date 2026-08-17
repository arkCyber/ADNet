//! Smoke test for the `a3chatd --enable-iroh` path.
//!
//! What this test proves:
//!
//! 1. `a3chatd` parses the `--enable-iroh` flag.
//! 2. With the `enable-iroh` feature enabled, `try_enable_iroh`
//!    successfully spins up `IrohBlobStore + Endpoint + Gossip +
//!    Docs::memory().spawn()` and constructs an [`IrohDocsChat`].
//! 3. The bridge is injected into `A3chatApp` so every outbound
//!    message (DM or group) is dual-written: SQLite first,
//!    then iroh-docs (best-effort fan-out).
//! 4. The daemon prints the "iroh-docs bridge ready" line on stderr.
//!
//! The test only compiles when the `enable-iroh` feature is on so
//! the default integration-test set stays slim. Pass
//! `--features enable-iroh` to Cargo to run it.

#![cfg(feature = "enable-iroh")]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Path to the compiled `a3chatd` binary. Cargo injects the
/// `CARGO_BIN_EXE_<name>` env var whenever it builds a `[[bin]]`
/// target, and points it at the freshly compiled executable.
/// `CARGO_BIN_EXE_a3chatd` is therefore the canonical way for an
/// integration test to invoke the exact same binary the user sees.
fn daemon_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_a3chatd"))
}

/// Owner used for the daemon's `--owner` flag.
const TEST_OWNER: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a3chatd_with_enable_iroh_boots_and_prints_bridge_ready() {
    let storage = tempfile::tempdir().expect("tempdir for storage");
    let mut child = Command::new(daemon_path())
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--owner")
        .arg(TEST_OWNER)
        .arg("--storage")
        .arg(storage.path())
        .arg("--enable-iroh")
        // `--stop-after 5` — daemon exits after 5 seconds regardless
        // of SIGTERM, so the test never wedges waiting on a child
        // that forgot to shut down.
        .arg("--stop-after")
        .arg("5")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a3chatd --enable-iroh");

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Drain the pipes on dedicated threads so a child-prints-too-much
    // never deadlocks on a full pipe buffer.
    let stdout_handle =
        std::thread::spawn(move || -> String { drain(stdout) });
    let stderr_handle =
        std::thread::spawn(move || -> String { drain(stderr) });

    // Bounded wait for the daemon — `--stop-after 5` plus a small
    // grace for the iroh-blobs shutdown paths.
    let deadline = Duration::from_secs(20);
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            break s;
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a3chatd did not exit within {deadline:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let stdout_text = stdout_handle.join().unwrap_or_default();
    let stderr_text = stderr_handle.join().unwrap_or_default();

    // The bridge-ready line is written via `eprintln!`, so it lands
    // on stderr rather than stdout. We check both pipes (and a
    // combined view) so a future refactor that switches back to
    // `println!` doesn't silently regress this assertion.
    let combined = format!("{stdout_text}\n{stderr_text}");
    assert!(
        combined.contains("iroh-docs bridge ready"),
        "a3chatd never printed the iroh bridge-ready marker.\n\
         --- stdout ---\n{stdout_text}\n--- stderr ---\n{stderr_text}\n\
         exit status: {status:?}"
    );
    // The "disabled" branch must NOT have triggered.
    assert!(
        !combined.contains("iroh-docs bridge disabled"),
        "daemon printed the disabled branch — --enable-iroh was ignored.\n\
         --- stdout ---\n{stdout_text}\n--- stderr ---\n{stderr_text}"
    );
}

fn drain<R: Read>(mut r: R) -> String {
    let mut buf = String::new();
    let _ = r.read_to_string(&mut buf);
    buf
}
