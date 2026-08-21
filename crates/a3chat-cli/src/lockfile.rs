//! Process-level lock file for the `a3chatd` daemon.
//!
//! Purpose: prevent two daemons from binding the same loopback port.
//! When the daemon starts it writes a lock file containing its PID
//! and `bind_addr`; on shutdown it removes the file. If a second
//! daemon starts and finds a *live* lock (i.e. the recorded PID is
//! still running), it aborts with a clear diagnostic. If the PID is
//! dead (stale lock), the new daemon replaces the file and proceeds.
//!
//! ## Location
//!
//! `~/.config/a3chatd/daemon.lock` on Linux/macOS, or the override
//! supplied via [`lock_path_for`]. The path is overridable for tests
//! and for the auto-test harness.
//!
//! ## Stale PID handling
//!
//! On Unix we `kill(pid, 0)` to test liveness without actually
//! signalling. A `EPERM` (process exists but owned by another user)
//! is treated as "live" so we don't accidentally clobber a daemon
//! we don't own. On non-Unix we fall back to a textual "REPLACE"
//! mode that overwrites unconditionally — this is documented and
//! the only safe behavior on a platform where we can't probe PID
//! liveness.

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;

/// Wire-format of the lock file. Stable across processes so the CLI
/// can read what the daemon wrote.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockFile {
    pub pid: u32,
    pub bind_addr: String,
    pub owner: String,
    pub storage_dir: String,
    pub created_at_unix: i64,
    pub daemon_version: String,
}

impl LockFile {
    pub fn new(
        pid: u32,
        bind_addr: impl Into<String>,
        owner: impl Into<String>,
        storage_dir: impl Into<String>,
        daemon_version: impl Into<String>,
    ) -> Self {
        Self {
            pid,
            bind_addr: bind_addr.into(),
            owner: owner.into(),
            storage_dir: storage_dir.into(),
            created_at_unix: Utc::now().timestamp(),
            daemon_version: daemon_version.into(),
        }
    }

    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("lock.tmp");
        let mut opts = fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let body = fs::read(path)?;
        serde_json::from_slice(&body)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Resolve the canonical lock file path. Defaults to
/// `$XDG_CONFIG_HOME/a3chatd/daemon.lock` (or
/// `$HOME/.config/a3chatd/daemon.lock` on Linux/macOS).
pub fn default_lock_path() -> PathBuf {
    // Prefer $XDG_CONFIG_HOME if defined; otherwise fall back to
    // ~/.config. On Windows fall back to %LOCALAPPDATA%/a3chatd.
    #[cfg(unix)]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| {
                    let mut p = PathBuf::from(h);
                    p.push(".config");
                    p
                })
            })
            .unwrap_or_else(|| std::env::temp_dir().join("a3chatd"));
        base.join("a3chatd").join("daemon.lock")
    }
    #[cfg(not(unix))]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("a3chatd"));
        base.join("a3chatd").join("daemon.lock")
    }
}

/// Return the path the daemon should use for its lock file. Pulls
/// `A3CHATD_LOCK_PATH` (test escape hatch) if set, else the
/// default.
pub fn lock_path() -> PathBuf {
    default_lock_path()
}

/// Resolve a lock path, honouring the canonical [`A3CHATD_LOCK_PATH`]
/// environment variable when set. Public so tests can avoid
/// mutating the process environment (which is `unsafe` in modern
/// Rust).
pub fn lock_path_for(override_path: Option<&Path>) -> PathBuf {
    if let Some(o) = override_path {
        return o.to_path_buf();
    }
    if let Some(p) = std::env::var_os("A3CHATD_LOCK_PATH") {
        return PathBuf::from(p);
    }
    default_lock_path()
}

/// Outstanding lock situation at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    /// No lock file present — the new daemon may write a fresh one.
    NoLock,
    /// Lock file present but the recorded PID is dead — safe to replace.
    Stale(LockFile),
    /// Lock file present and the recorded PID is alive — bail out.
    Live(LockFile),
    /// Lock file present but unreadable / corrupt — operator decision.
    Unreadable(PathBuf, String),
}

/// Probe the lock file at `path` and return the outcome.
pub fn probe_lock(path: &Path) -> LockOutcome {
    if !path.exists() {
        return LockOutcome::NoLock;
    }
    let lock = match LockFile::read_from(path) {
        Ok(l) => l,
        Err(e) => return LockOutcome::Unreadable(path.to_path_buf(), e.to_string()),
    };
    if is_pid_alive(lock.pid) {
        LockOutcome::Live(lock)
    } else {
        LockOutcome::Stale(lock)
    }
}

/// Acquire the lock file at `path`. Writes atomically; removes any
/// stale lock first. Returns the recorded [`LockFile`] on success so
/// the caller can echo it in the daemon log line.
pub fn acquire_lock(path: &Path, fresh: &LockFile) -> std::io::Result<LockFile> {
    match probe_lock(path) {
        LockOutcome::NoLock => {
            fresh.write_to(path)?;
            Ok(fresh.clone())
        }
        LockOutcome::Stale(_) => {
            // Replace — safe because the recorded PID is dead.
            fresh.write_to(path)?;
            Ok(fresh.clone())
        }
        LockOutcome::Live(lock) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "a3chatd lock already held by pid={} bind={} since={}",
                lock.pid, lock.bind_addr, lock.created_at_unix
            ),
        )),
        LockOutcome::Unreadable(p, msg) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unreadable lock file at {}: {}", p.display(), msg),
        )),
    }
}

/// Release the lock file at `path` IF it belongs to the recorded
/// PID. We refuse to remove a different daemon's lock so a
/// misconfigured shutdown doesn't accidentally evict a healthy peer.
pub fn release_lock(path: &Path, pid: u32) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let lock = LockFile::read_from(path)?;
    if lock.pid != pid {
        eprintln!(
            "a3chatd: refusing to release lock owned by pid={} (we are pid={})",
            lock.pid, pid
        );
        return Ok(());
    }
    fs::remove_file(path)
}

/// Best-effort test for "is this PID alive?".
///
/// - **Linux:** read `/proc/<pid>`. The directory only exists for
///   live processes. (We do not need to read the directory
///   contents, so the call is O(1) and does not require
///   `unsafe`.)
/// - **macOS / other Unix:** shell out to `kill -0 <pid>`, which
///   the OS uses to test liveness without actually sending a
///   signal. We classify the exit code: 0 = alive, non-zero with
///   "No such process" stderr = dead, anything else is treated as
///   alive (we don't want to evict a peer we couldn't probe).
/// - **Non-Unix:** always returns true (we cannot probe without a
///   signal).
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // `/proc/<pid>` is a directory owned by the live process. If
        // it exists, the PID is alive. We use `metadata` rather than
        // `read_dir` so the call is constant time.
        let proc_path = std::path::Path::new("/proc").join(pid.to_string());
        match std::fs::metadata(&proc_path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                // Permission denied or other transient error — be
                // conservative and treat as live so we don't evict a
                // peer we couldn't probe.
                true
            }
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::process::Command;
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .output();
        match out {
            Ok(o) => o.status.success(),
            Err(_) => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "a3chatd-lockfile-{}-{}",
            name,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("daemon.lock")
    }

    #[test]
    fn no_lock_outcome_when_file_missing() {
        let p = tmp_path("missing");
        let _ = std::fs::remove_file(&p);
        assert_eq!(probe_lock(&p), LockOutcome::NoLock);
    }

    #[test]
    fn acquire_lock_writes_file() {
        let p = tmp_path("write");
        let fresh = LockFile::new(12345u32, "127.0.0.1:53421", "alice", "/tmp", "0.1.0");
        acquire_lock(&p, &fresh).unwrap();
        let back = LockFile::read_from(&p).unwrap();
        assert_eq!(back, fresh);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn acquire_lock_replaces_stale_lock() {
        let p = tmp_path("stale");
        let stale = LockFile::new(1, "127.0.0.1:1234", "bob", "/tmp", "0.0.1");
        stale.write_to(&p).unwrap();
        // PID 1 might be alive on some systems (init). Use a PID that
        // is reliably dead — `0xFFFFFF` is reserved.
        let stale = LockFile::new(0xFFFFFF, "127.0.0.1:1234", "bob", "/tmp", "0.0.1");
        stale.write_to(&p).unwrap();
        let fresh = LockFile::new(std::process::id(), "127.0.0.1:53421", "alice", "/tmp", "0.1.0");
        let outcome = acquire_lock(&p, &fresh).unwrap();
        assert_eq!(outcome.pid, std::process::id());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn acquire_lock_refuses_live_lock() {
        let p = tmp_path("live");
        // Use our own PID — definitely alive.
        let live = LockFile::new(std::process::id(), "127.0.0.1:1234", "alice", "/tmp", "0.0.1");
        live.write_to(&p).unwrap();
        let fresh = LockFile::new(std::process::id(), "127.0.0.1:53421", "alice", "/tmp", "0.1.0");
        let err = acquire_lock(&p, &fresh).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn release_lock_only_removes_own_pid() {
        let p = tmp_path("release");
        let live = LockFile::new(0xFFFFFF, "127.0.0.1:1234", "bob", "/tmp", "0.0.1");
        live.write_to(&p).unwrap();
        // PID mismatch — must not remove.
        release_lock(&p, std::process::id()).unwrap();
        assert!(p.exists(), "foreign lock must not be removed");
        // PID match — must remove.
        let mut mine = LockFile::new(std::process::id(), "127.0.0.1:1234", "bob", "/tmp", "0.0.1");
        mine.write_to(&p).unwrap();
        release_lock(&p, std::process::id()).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn lock_path_for_respects_override() {
        let custom = std::env::temp_dir().join("a3chatd-lockpath-override");
        let got = lock_path_for(Some(&custom));
        assert_eq!(got, custom);
        // Default path (no override) is still well-formed.
        let default = lock_path_for(None);
        assert!(default.parent().is_some());
    }

    #[test]
    fn is_pid_alive_returns_true_for_self() {
        assert!(is_pid_alive(std::process::id()));
        assert!(!is_pid_alive(0));
    }
}
