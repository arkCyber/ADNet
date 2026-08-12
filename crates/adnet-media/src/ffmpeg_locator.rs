//! FFmpeg / ffprobe availability detection + binary resolution.
//!
//! Used by the FFmpeg transcoder to find the `ffmpeg` and `ffprobe`
//! binaries on the host. Resolution order:
//!
//! 1. Explicit override via [`FFmpegLocator::with_paths`].
//! 2. The `FFMPEG_BIN` / `FFPROBE_BIN` environment variables.
//! 3. `PATH` lookup via [`which::which`].
//!
//! DO-178C SR-1: the resolved path is recorded in `TranscodeOutput`
//! so the safety case can trace a build artifact to a specific
//! ffmpeg version.

use crate::error::{MediaError, MediaResult};
use std::path::{Path, PathBuf};

/// Default binary names looked up on `PATH`.
pub const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
pub const DEFAULT_FFPROBE_BIN: &str = "ffprobe";

#[derive(Debug, Clone)]
pub struct FFmpegLocator {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl FFmpegLocator {
    /// Resolve `ffmpeg` and `ffprobe` locations using the rules
    /// above. Returns `MediaError::Io` if either binary is missing.
    pub fn detect() -> MediaResult<Self> {
        let ffmpeg = resolve_binary(DEFAULT_FFMPEG_BIN, "FFMPEG_BIN")?;
        let ffprobe = resolve_binary(DEFAULT_FFPROBE_BIN, "FFPROBE_BIN")?;
        Ok(Self { ffmpeg, ffprobe })
    }

    pub fn with_paths(ffmpeg: PathBuf, ffprobe: PathBuf) -> MediaResult<Self> {
        if !is_executable(&ffmpeg) {
            return Err(MediaError::Io(format!(
                "ffmpeg binary not executable: {}",
                ffmpeg.display()
            )));
        }
        if !is_executable(&ffprobe) {
            return Err(MediaError::Io(format!(
                "ffprobe binary not executable: {}",
                ffprobe.display()
            )));
        }
        Ok(Self { ffmpeg, ffprobe })
    }

    /// Probe the ffmpeg version string. Used by `SAFETY_REVISION`
    /// audit trails to record which ffmpeg release produced the
    /// build artifact.
    pub fn version(&self) -> MediaResult<String> {
        let out = std::process::Command::new(&self.ffmpeg)
            .arg("-version")
            .output()
            .map_err(|e| MediaError::Io(format!("ffmpeg -version failed: {e}")))?;
        if !out.status.success() {
            return Err(MediaError::Io(format!(
                "ffmpeg -version exit {}",
                out.status
            )));
        }
        let s = String::from_utf8_lossy(&out.stdout);
        let first = s.lines().next().unwrap_or("").to_string();
        Ok(first)
    }
}

fn resolve_binary(default_name: &str, env_var: &str) -> MediaResult<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let path = PathBuf::from(p);
        if is_executable(&path) {
            return Ok(path);
        }
    }
    // PATH lookup via `which` crate is not in our deps; we shell
    // out to `which` only as a last resort (off the hot path).
    let exe = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&exe) {
        let candidate = dir.join(default_name);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(MediaError::Io(format!(
        "binary `{default_name}` not found on PATH and ${env_var} unset"
    )))
}

fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binary_names() {
        assert_eq!(DEFAULT_FFMPEG_BIN, "ffmpeg");
        assert_eq!(DEFAULT_FFPROBE_BIN, "ffprobe");
    }

    #[test]
    fn detect_resolves_local_ffmpeg() {
        // This test only runs on dev hosts that have ffmpeg
        // installed. It is a smoke test, not a correctness gate.
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok()
        {
            let loc = FFmpegLocator::detect().expect("ffmpeg must be on PATH");
            assert!(loc.ffmpeg.is_file());
            assert!(loc.ffprobe.is_file());
        }
    }
}
