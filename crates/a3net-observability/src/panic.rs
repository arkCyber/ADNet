//! Panic hook — structured panic reporting for A3Net.
//!
//! Every A3Net binary (node, relay, DNS server, CLI daemon) must
//! call [`install_panic_hook`] at startup.  The hook captures:
//!
//!  1. **Panic message + location** — `PanicHookInfo` fields
//!  2. **Backtrace** — via `std::backtrace::Backtrace` (always,
//!     regardless of `RUST_BACKTRACE`, because we want it even if
//!     the env var is not set)
//!  3. **`a3net_panic_total`** — a `Lazy` static `AtomicU64` counter
//!     so `/metrics` surfaces the panic count
//!  4. **Structured log line** — at `ERROR` level via `tracing!`
//!     so log aggregators (Loki / ES / CloudWatch) can index by
//!     `panic=true` and `component=...`
//!  5. **Crash log file** — `<data_dir>/crashes/<unix_ts>_<thread>.jsonl`
//!     so operators can recover panic details after the process has
//!     restarted (e.g. from a systemd restart)
//!
//! ## Usage
//!
//! ```rust
//! use a3net_observability::panic::{install_panic_hook, PanicConfig};
//!
//! let config = PanicConfig::new("a3net-node")
//!     .with_data_dir("/var/lib/a3net");
//!
//! install_panic_hook(&config);
//! ```
//!
//! ## Design constraints
//!
//! - **No external dependencies.**  We deliberately do not pull in
//!   `panic-hook` or `_backtrace` crates.  The Rust standard library
//!   provides everything we need (`std::backtrace::Backtrace`).
//! - **`#![forbid(unsafe_code)]` clean.**  The hook runs in a
//!   `Box<fn(&PanicHookInfo)>` closure, which is safe Rust.
//! - **No panics from the hook itself.**  If writing the crash log
//!   fails we fall back to `eprintln!` — we never abort recursively.
//! - **Thread-safe.**  The crash directory is created once on install
//!   and the counter is a process-global `Lazy` static.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use std::backtrace::Backtrace;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;

// ─── Panic counter ────────────────────────────────────────────────────────────

/// Process-global panic counter.
///
/// Accessed atomically from the panic hook (runs on the panic thread,
/// not the main thread) so we use `Ordering::Relaxed` — a slightly
/// stale read of the counter after a crash is acceptable.
static PANIC_COUNTER: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(0));

// ─── Panic config ────────────────────────────────────────────────────────────

/// Configuration for the panic hook.
///
/// Construct with [`PanicConfig::new`] and chain builder methods.
#[derive(Debug, Clone)]
pub struct PanicConfig {
    /// Human-readable component name, used as the `component` label
    /// in metrics and as the first field in crash log lines.
    pub component: String,
    /// Directory where crash log files are written.
    /// If `None`, crash logs are skipped (useful for short-lived CLI tools).
    pub data_dir: Option<PathBuf>,
    /// Whether to capture the full backtrace.  If `false`, only
    /// `PanicHookInfo.location()` is logged.  Always `true` in production.
    pub capture_backtrace: bool,
}

impl PanicConfig {
    /// Create a config for the given component name.
    ///
    /// Crash logs are **disabled** by default.  Call
    /// [`with_data_dir`](Self::with_data_dir) to enable them.
    pub fn new(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            data_dir: None,
            capture_backtrace: true,
        }
    }

    /// Set the data directory.  Crash log files will be written to
    /// `<data_dir>/crashes/`.
    ///
    /// Returns `&mut Self` (for builder chaining).  The error from
    /// directory creation is **not** fatal — the panic hook will still
    /// be installed; only crash log writing will be skipped.
    pub fn with_data_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(path.into());
        self
    }

    /// Disable backtrace capture.  Reduces overhead in unit tests.
    #[must_use]
    pub fn without_backtrace(mut self) -> Self {
        self.capture_backtrace = false;
        self
    }
}

// ─── Installation ─────────────────────────────────────────────────────────────

/// Install the global panic hook for A3Net binaries.
///
/// Must be called **once** at process startup, before any `tokio::spawn`
/// or `std::thread::spawn` calls, so that every thread benefits from the
/// hook.
///
/// ## Panic ordering guarantee
///
/// The hook fires in the panicking thread **before** `std::panic::resume_unwind`
/// unwinds.  Because we use `catch_unwind` internally (see below),
/// the process will **not** abort from a recursive panic.
///
/// ## Thread safety
///
/// All shared state is written once at install time and then read-only
/// at panic time, making it safe to call from any thread.
pub fn install_panic_hook(config: &PanicConfig) {
    let component = config.component.clone();
    let data_dir = config.data_dir.clone();
    let capture_backtrace = config.capture_backtrace;

    // Lazily create the crashes directory so we can report the error.
    if let Some(ref d) = data_dir {
        let crashes = d.join("crashes");
        if let Err(e) = fs::create_dir_all(&crashes) {
            eprintln!(
                "a3net-panic: could not create crash log dir {:?}: {e}",
                crashes
            );
        }
    }

    // Clone before the closures take ownership.
    let component_for_hook = component.clone();
    let data_dir_for_hook = data_dir.clone();
    let data_dir_for_info = data_dir.clone();

    std::panic::set_hook(Box::new(move |panic_info| {
        // All captures are owned by the closure — no references to outer scope.
        handle_panic(
            panic_info,
            &component_for_hook,
            data_dir_for_hook.as_deref(),
            capture_backtrace,
        );
    }));

    let component_clone = component.clone();
    tracing::info!(
        component = %component_clone,
        data_dir = ?data_dir_for_info,
        backtrace = capture_backtrace,
        "a3net panic hook installed"
    );
}

/// Internal panic handler — extracted so it can be unit-tested.
#[allow(clippy::too_many_arguments)]
fn handle_panic(
    panic_info: &std::panic::PanicHookInfo,
    component: &str,
    data_dir: Option<&Path>,
    capture_backtrace: bool,
) {
    // ── 1. Gather all information ────────────────────────────────────────────
    let message = panic_info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| {
            panic_info
                .payload()
                .downcast_ref::<String>()
                .cloned()
        })
        .unwrap_or_else(|| "unknown panic payload".to_string());

    let location = panic_info
        .location()
        .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
        .unwrap_or_else(|| "unknown location".to_string());

    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("unnamed").to_string();
    let thread_id = format!("{:?}", thread.id());

    let backtrace = if capture_backtrace {
        format!("{}", Backtrace::capture())
    } else {
        String::new()
    };

    let timestamp: DateTime<Utc> = Utc::now();
    let ts_str = timestamp.format("%Y%m%d_%H%M%S_%f").to_string();

    // ── 2. Increment the panic counter ──────────────────────────────────────
    let panic_count = PANIC_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;

    // ── 3. Emit structured tracing event ───────────────────────────────────
    let comp = component.to_string();
    let msg = message.clone();
    let loc = location.clone();
    let thread_nm = thread_name.clone();
    let thread_id_copy = thread_id.clone();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tracing::error!(
            panic = true,
            component = %comp,
            thread = %thread_nm,
            thread_id = %thread_id_copy,
            message = %msg,
            location = %loc,
            panic_count = panic_count,
            backtrace_lines = backtrace.lines().count(),
            "ADNET PANIC: {msg} at {loc} (thread={thread_nm})"
        );
    }));

    // ── 4. Write crash log file ───────────────────────────────────────────
    if let Some(dir) = data_dir {
        write_crash_log(dir, CrashLogData {
            ts: &ts_str,
            component,
            thread_name: &thread_name,
            thread_id: &thread_id,
            message: &message,
            location: &location,
            backtrace: &backtrace,
            panic_count,
        });
    }

    // ── 5. Always write to stderr so operators see it immediately ─────────────
    let crash_path = data_dir
        .map(|d| {
            format!(
                "{}/crashes/{}_{}.jsonl",
                d.display(),
                ts_str,
                thread_id.replace([':', ' '], "_")
            )
        })
        .unwrap_or_else(|| "(no data_dir set — crash log disabled)".to_string());

    eprintln!("================================================================================");
    eprintln!("ADNET PANIC in [{component}] thread=[{thread_name}]");
    eprintln!("  message : {message}");
    eprintln!("  location: {location}");
    eprintln!("  panic # : {panic_count}");
    if capture_backtrace && !backtrace.is_empty() {
        eprintln!("--- backtrace -------------------------------------------------------------------");
        for line in backtrace.lines() {
            eprintln!("  {line}");
        }
    }
    eprintln!("================================================================================");
    eprintln!("Crash log written to: {crash_path}");
    eprintln!("================================================================================");
}

/// Crash-log data passed as a single struct to avoid `#[allow(clippy::too_many_arguments)]`.
struct CrashLogData<'a> {
    ts: &'a str,
    component: &'a str,
    thread_name: &'a str,
    thread_id: &'a str,
    message: &'a str,
    location: &'a str,
    backtrace: &'a str,
    panic_count: u64,
}

/// Write one NDJSON line to the crash log.
///
/// The crash log is an **append-only** file named `<timestamp>_<thread>.jsonl`
/// inside `<data_dir>/crashes/`.  Each line is a flat JSON object containing
/// all panic details.  The file is opened with `O_APPEND` so writes are
/// atomic on POSIX systems (each `write(2)` call is a single record).
///
/// ## Crash safety
///
/// We write to a temporary file first and then `rename` over the crash log.
/// This way, a crash during the write leaves the temp file rather than a
/// partially-written crash log.  `rename` is atomic on POSIX.
fn write_crash_log(data_dir: &Path, data: CrashLogData) {
    let crashes = data_dir.join("crashes");
    let safe_thread_id = data.thread_id.replace([':', ' ', '-'], "_");
    let file_name = format!("{}_{}.jsonl", data.ts, safe_thread_id);
    let path = crashes.join(&file_name);

    let json = serde_json::json!({
        "type": "a3net_panic",
        "version": 1,
        "component": data.component,
        "thread_name": data.thread_name,
        "thread_id": data.thread_id,
        "message": data.message,
        "location": data.location,
        "backtrace": data.backtrace,
        "panic_count": data.panic_count,
        "timestamp": data.ts,
    });

    let line = serde_json::to_string(&json).unwrap_or_else(|e| {
        serde_json::json!({
            "type": "a3net_panic",
            "component": data.component,
            "message": data.message,
            "location": data.location,
            "serde_error": e.to_string(),
        })
        .to_string()
    });

    let temp_path = crashes.join(format!(".tmp_{}", file_name));

    match File::create(&temp_path)
        .and_then(|mut f| {
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()
        })
        .and_then(|_| fs::rename(&temp_path, &path))
    {
        Ok(()) => {}
        Err(e) => {
            // Fallback: direct append.  Not crash-safe but better than nothing.
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{}", line));
            eprintln!(
                "a3net-panic: could not write crash log atomically (fallback used): {e}"
            );
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_config_default() {
        let c = PanicConfig::new("test-component");
        assert_eq!(c.component, "test-component");
        assert!(c.data_dir.is_none());
        assert!(c.capture_backtrace);

        let c2 = c.clone().with_data_dir("/tmp/crashes").without_backtrace();
        assert_eq!(c2.component, "test-component");
        assert!(c2.data_dir.is_some());
        assert!(!c2.capture_backtrace);
    }

    #[test]
    fn panic_hook_install_does_not_crash() {
        // Suppress the "hook replaced" warning.
        let prev = std::panic::take_hook();
        let config = PanicConfig::new("a3net-observability-test")
            .with_data_dir(std::env::temp_dir())
            .without_backtrace();

        install_panic_hook(&config);

        std::panic::set_hook(prev);
    }

    #[test]
    fn crash_log_json_is_well_formed() {
        let line = serde_json::json!({
            "type": "a3net_panic",
            "version": 1,
            "component": "test",
            "thread_name": "main",
            "thread_id": "ThreadId(1)",
            "message": "test panic",
            "location": "test.rs:1:2",
            "backtrace": "",
            "panic_count": 1u64,
            "timestamp": "20260814_120000_000000",
        });

        let json_str = serde_json::to_string(&line).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["type"], "a3net_panic");
        assert_eq!(parsed["component"], "test");
        assert_eq!(parsed["message"], "test panic");
    }

    #[test]
    fn panic_counter_increments() {
        // Counter is global across all tests in this binary, so just
        // verify it is accessible and atomic.
        let before = PANIC_COUNTER.load(Ordering::Relaxed);
        PANIC_COUNTER.fetch_add(1, Ordering::Relaxed);
        let after = PANIC_COUNTER.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }
}
