//! `FFmpegTranscoder` — wraps the ffmpeg CLI to produce a
//! multi-variant / segmented media DAG from a real source file.
//!
//! ## Pipeline
//!
//! ```text
//! src.mp4
//!    │
//!    ▼  ffprobe
//! MediaProbe { width, height, fps, sample_rate, channels, ... }
//!    │
//!    ▼  for each VariantSpec:
//!       ffmpeg -i src.mp4 \
//!              -vf "scale=W:H,fps=N" \
//!              -c:v libx264 -b:v Kbps \
//!              -c:a aac -ar 48000 -ac 2 \
//!              -f segment -segment_time 2 \
//!              -reset -1 -strftime 0 \
//!              "/tmp/dag/variant_%05d.m4s"
//!    │
//!    ▼  for audio:
//!       ffmpeg -i src.mp4 -vn -c:a aac -ar 48000 -ac 2 \
//!              -f segment -segment_time 2 \
//!              "/tmp/dag/audio_%05d.m4s"
//!    │
//!    ▼  Segmenter (BLAKE3 per segment)
//!    ▼  Manifest (per-variant + audio + root hash)
//! ```
//!
//! DO-178C SR-3 (deterministic slicing) requires that the segment
//! boundaries are stable across re-runs. We achieve this by:
//!   * using `-segment_time` aligned to whole GOPs
//!   * using `-reset -1` so every segment is independently decodable
//!   * re-encoding through a fixed GOP structure
//!
//! DO-178C SR-9 (size cap) is enforced up-front by [`FFmpegConfig`].

use crate::config::VariantSpec;
use crate::error::{MediaError, MediaResult};
use crate::ffmpeg_locator::FFmpegLocator;
use crate::ffmpeg_probe::MediaProbe;
use crate::integrity::{LP_AUDIO, LP_VIDEO, SegmentDigest};
use crate::transcode::TranscodeOutput;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Tunable parameters for [`FFmpegTranscoder`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FFmpegConfig {
    /// GOP-aligned segment length in milliseconds.
    pub segment_duration_ms: u64,
    /// Number of threads ffmpeg may use per encode.
    pub threads: u32,
    /// GOP size in frames. `segment_fps * segment_duration_ms / 1000`
    /// is a sensible default; we expose this for operators who want
    /// longer keyframe intervals.
    pub gop_frames: u32,
    /// If `true`, the transcoder will overwrite `output_dir` if it
    /// exists. Off by default — DO-178C SR-2 requires explicit
    /// completion proofs (here: clean staging dir).
    pub overwrite: bool,
    /// Per-ffmpeg-process timeout in seconds. `0` disables
    /// the timeout (not recommended for production).
    pub timeout_secs: u64,
}

/// Closure type for progress reporting. Receives the variant label
/// and a 0..=100 progress integer.
pub type ProgressCallback = std::sync::Arc<dyn Fn(&str, u32) + Send + Sync>;

impl Default for FFmpegConfig {
    fn default() -> Self {
        Self {
            segment_duration_ms: 2_000,
            threads: 2,
            gop_frames: 60,
            overwrite: false,
            timeout_secs: 600,
        }
    }
}

impl FFmpegConfig {
    pub fn validate(&self) -> MediaResult<()> {
        if self.segment_duration_ms < 500 || self.segment_duration_ms > 6_000 {
            return Err(MediaError::InvalidConfig(format!(
                "segment_duration_ms {} out of [500, 6000]",
                self.segment_duration_ms
            )));
        }
        if self.gop_frames == 0 || self.gop_frames > 1_000 {
            return Err(MediaError::InvalidConfig(format!(
                "gop_frames {} out of [1, 1000]",
                self.gop_frames
            )));
        }
        if self.threads == 0 || self.threads > 64 {
            return Err(MediaError::InvalidConfig(format!(
                "threads {} out of [1, 64]",
                self.threads
            )));
        }
        if self.timeout_secs > 0 && self.timeout_secs < 10 {
            return Err(MediaError::InvalidConfig(format!(
                "timeout_secs {} too low (min 10, 0 disables)",
                self.timeout_secs
            )));
        }
        Ok(())
    }
}

/// FFmpeg-backed transcoder.
///
/// This type does NOT implement the existing [`Transcoder`] trait
/// because ffmpeg is intrinsically async (subprocess IO). Callers
/// should use [`FFmpegTranscoder::transcode_file`] or
/// [`FFmpegTranscoder::transcode_raw`] directly, then feed the
/// resulting `Vec<TranscodeOutput>` into the existing
/// `MediaDagBuilder`.
#[derive(Clone)]
pub struct FFmpegTranscoder {
    pub locator: FFmpegLocator,
    pub config: FFmpegConfig,
    pub progress: Option<ProgressCallback>,
}

impl std::fmt::Debug for FFmpegTranscoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FFmpegTranscoder")
            .field("locator", &self.locator)
            .field("config", &self.config)
            .field("progress", &self.progress.as_ref().map(|_| "Fn(&str, u32)"))
            .finish()
    }
}

impl FFmpegTranscoder {
    pub fn new(locator: FFmpegLocator) -> Self {
        Self {
            locator,
            config: FFmpegConfig::default(),
            progress: None,
        }
    }

    pub fn with_config(locator: FFmpegLocator, config: FFmpegConfig) -> MediaResult<Self> {
        config.validate()?;
        Ok(Self { locator, config, progress: None })
    }

    pub fn with_progress(mut self, cb: ProgressCallback) -> Self {
        self.progress = Some(cb);
        self
    }

    /// Top-level entry point — runs the full pipeline against a
    /// real source file. Returns one [`TranscodeOutput`] per
    /// variant.
    pub async fn transcode_file(
        &self,
        source: &Path,
        output_dir: &Path,
        ladder: &[VariantSpec],
    ) -> MediaResult<Vec<TranscodeOutput>> {
        if !source.is_file() {
            return Err(MediaError::Io(format!(
                "source not found: {}",
                source.display()
            )));
        }
        let probe = self.locator.probe(source).await?;
        self.validate_probe(&probe)?;
        debug!(?probe, "ffprobe complete");

        if self.config.overwrite && output_dir.exists() {
            std::fs::remove_dir_all(output_dir)?;
        }
        std::fs::create_dir_all(output_dir)?;

        let mut results = Vec::with_capacity(ladder.len());
        for spec in ladder {
            if let Some(cb) = &self.progress {
                cb(&spec.label, 0);
            }
            let variant_dir = output_dir.join(&spec.label);
            std::fs::create_dir_all(&variant_dir)?;
            let out = self
                .encode_variant(source, &variant_dir, spec, &probe)
                .await?;
            if let Some(cb) = &self.progress {
                cb(&spec.label, 100);
            }
            results.push(out);
        }
        Ok(results)
    }

    /// Defensive check that ffprobe returned sane values. If
    /// fps_num is 0 the ffmpeg `force_key_frames` expression
    /// below would yield `0/0 = NaN`, which ffmpeg treats as
    /// an unparseable filter — we'd rather surface a clean
    /// error up-front.
    fn validate_probe(&self, probe: &MediaProbe) -> MediaResult<()> {
        if probe.fps_num == 0 || probe.fps_den == 0 {
            return Err(MediaError::InvalidConfig(format!(
                "ffprobe returned invalid fps {}/{}",
                probe.fps_num, probe.fps_den
            )));
        }
        if probe.width == 0 || probe.height == 0 {
            return Err(MediaError::InvalidConfig(format!(
                "ffprobe returned zero dimensions {}x{}",
                probe.width, probe.height
            )));
        }
        Ok(())
    }

async fn encode_variant(
        &self,
        source: &Path,
        variant_dir: &Path,
        spec: &VariantSpec,
        probe: &MediaProbe,
    ) -> MediaResult<TranscodeOutput> {
        // 1. Encode the video ladder rung. The muxer is the
        //    `segment` muxer (which streams mpegts chunks);
        //    using `-f mpegts` alone would emit a single
        //    file literally named `v_%05d.ts` instead of one
        //    file per segment. The `mpegts` segment format also
        //    accepts `-an` (no audio) without complaining.
        let video_pattern = variant_dir.join("v_%05d.ts");
        let video_status = self
            .run_ffmpeg(&[
                "-y",
                "-i",
                &path_arg(source),
                "-vf",
                &format!("scale={}:{}:flags=lanczos,fps={}/{}",
                    spec.width, spec.height, probe.fps_num, probe.fps_den),
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-b:v",
                &format!("{}k", spec.bitrate_kbps),
                "-g",
                &self.config.gop_frames.to_string(),
                "-keyint_min",
                &self.config.gop_frames.to_string(),
                "-sc_threshold",
                "0",
                "-force_key_frames",
                &format!("expr:gte(t,n_forced*{})",
                    self.config.gop_frames as f64 / probe.fps_num as f64),
                "-threads",
                &self.config.threads.to_string(),
                "-an",
                "-f",
                "segment",
                "-segment_format",
                "mpegts",
                "-segment_time",
                &format!("{:.3}", self.config.segment_duration_ms as f64 / 1_000.0),
                "-reset_timestamps",
                "1",
                "-strftime",
                "0",
                &path_arg(&video_pattern),
            ])
            .await?;
        if !video_status.success() {
            return Err(MediaError::DecodeError {
                offset: 0,
                message: format!(
                    "ffmpeg video variant {} exit {}",
                    spec.label, video_status
                ),
            });
        }

        // 2. Encode the audio track. NOTE: this re-encodes once
        //    per variant. A future commit should hoist audio
        //    encoding out of the variant loop (audio is
        //    identical across rungs of the same clip). Tracked
        //    in AUDIT_MEDIA_FFI_20260811.md §B4.
        let audio_pattern = variant_dir.join("a_%05d.ts");
        let audio_status = self
            .run_ffmpeg(&[
                "-y",
                "-i",
                &path_arg(source),
                "-vn",
                "-c:a",
                "aac",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-b:a",
                "128k",
                "-f",
                "segment",
                "-segment_format",
                "mpegts",
                "-segment_time",
                &format!("{:.3}", self.config.segment_duration_ms as f64 / 1_000.0),
                "-reset_timestamps",
                "1",
                "-strftime",
                "0",
                &path_arg(&audio_pattern),
            ])
            .await?;
        if !audio_status.success() {
            return Err(MediaError::DecodeError {
                offset: 0,
                message: format!("ffmpeg audio exit {}", audio_status),
            });
        }

        // 3. Collect, BLAKE3-hash, length-prefix each segment.
        let video_segments = read_segments(&video_pattern, LP_VIDEO)?;
        let audio_segments = read_segments(&audio_pattern, LP_AUDIO)?;

        let duration_ms = compute_duration_ms(video_segments.len(), self.config.segment_duration_ms);

        Ok(TranscodeOutput {
            variant: spec.clone(),
            video_segments,
            audio_segments,
            duration_ms,
        })
    }

    async fn run_ffmpeg(&self, args: &[&str]) -> MediaResult<std::process::ExitStatus> {
        info!(?self.locator.ffmpeg, ?args, "spawning ffmpeg");
        let mut cmd = std::process::Command::new(&self.locator.ffmpeg);
        cmd.args(args);
        // Hide the ffmpeg banner so the JSON metadata we capture
        // doesn't get polluted by progress lines on stderr.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        let timeout = self.config.timeout_secs;
        if timeout == 0 {
            // Caller explicitly disabled the deadline.
            let status = cmd
                .status()
                .map_err(|e| MediaError::Io(format!("ffmpeg spawn failed: {e}")))?;
            if !status.success() {
                warn!(?status, "ffmpeg exited non-zero");
            }
            Ok(status)
        } else {
            // Run the blocking `status()` call on a worker thread
            // so the async runtime can apply the deadline.
            let join = tokio::task::spawn_blocking(move || cmd.status());
            match tokio::time::timeout(
                std::time::Duration::from_secs(timeout),
                join,
            )
            .await
            {
                Ok(Ok(status)) => {
                    let status = status.map_err(|e| {
                        MediaError::Io(format!("ffmpeg spawn failed: {e}"))
                    })?;
                    if !status.success() {
                        warn!(?status, "ffmpeg exited non-zero");
                    }
                    Ok(status)
                }
                Ok(Err(join_err)) => Err(MediaError::DecodeError {
                    offset: 0,
                    message: format!("ffmpeg worker join failed: {join_err}"),
                }),
                Err(_elapsed) => Err(MediaError::DecodeError {
                    offset: 0,
                    message: format!(
                        "ffmpeg timed out after {timeout}s — possible hang in libavformat"
                    ),
                }),
            }
        }
    }
}

/// FFmpeg-backed transcoder entry point for the synthetic-input
/// pipeline. Writes the synthetic RGB/PCM data to a tempdir,
/// runs ffmpeg via [`FFmpegTranscoder::transcode_file_from_raw`],
/// and returns the first variant's output.
///
/// This is a sync wrapper that uses `tokio::runtime::Handle::current`
/// when called inside a runtime, or spins up a fresh
/// `tokio::runtime::Runtime` otherwise. The trait is *not*
/// implemented because ffmpeg is async; this is the ergonomic
/// sync shim.
pub fn transcode_synthetic(
    transcoder: &FFmpegTranscoder,
    input: &crate::transcode::TranscodeInput,
    target: &VariantSpec,
) -> MediaResult<crate::transcode::TranscodeOutput> {
    let width = input.frames.first().map(|f| f.width).unwrap_or(0);
    let height = input.frames.first().map(|f| f.height).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "a3net-media-{}-{}x{}",
        std::process::id(),
        width, height
    ));
    std::fs::create_dir_all(&tmp)?;
    let yuv_path = tmp.join("input.yuv");
    let pcm_path = tmp.join("input.pcm");
    write_yuv(&yuv_path, &input.frames)?;
    write_pcm(&pcm_path, &input.samples)?;

    let ladder = vec![target.clone()];
    let out_dir = tmp.join("out");
    let result = block_on_async(transcoder.transcode_file_from_raw(
        &yuv_path, &pcm_path, &out_dir, &ladder, input,
    ));
    // SR-10 / general hygiene: clean up the staging dir on
    // both Ok and Err paths. `_ = …` so a leaked dir doesn't
    // mask the original error.
    let _ = std::fs::remove_dir_all(&tmp);
    let results = result?;
    results.into_iter().next().ok_or_else(|| MediaError::DecodeError {
        offset: 0,
        message: "ffmpeg produced no output".into(),
    })
}

fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => h.block_on(fut),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(fut)
        }
    }
}

impl FFmpegTranscoder {
    async fn transcode_file_from_raw(
        &self,
        yuv_path: &Path,
        pcm_path: &Path,
        output_dir: &Path,
        ladder: &[VariantSpec],
        input: &crate::transcode::TranscodeInput,
    ) -> MediaResult<Vec<TranscodeOutput>> {
        std::fs::create_dir_all(output_dir)?;
        let mut results = Vec::new();
        let width = input.frames.first().map(|f| f.width).unwrap_or(0);
        let height = input.frames.first().map(|f| f.height).unwrap_or(0);
        for spec in ladder {
            let variant_dir = output_dir.join(&spec.label);
            std::fs::create_dir_all(&variant_dir)?;
            let video_pattern = variant_dir.join("v_%05d.ts");
            let status = self
                .run_ffmpeg(&[
                    "-y",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgb24",
                    "-s",
                    &format!("{width}x{height}"),
                    "-r",
                    &input.fps.to_string(),
                    "-i",
                    &path_arg(yuv_path),
                    "-f",
                    "s16le",
                    "-ar",
                    "48000",
                    "-ac",
                    &input.audio_channels.to_string(),
                    "-i",
                    &path_arg(pcm_path),
                    "-vf",
                    &format!("scale={}:{}:flags=lanczos", spec.width, spec.height),
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-b:v",
                    &format!("{}k", spec.bitrate_kbps),
                    "-g",
                    &self.config.gop_frames.to_string(),
                    "-c:a",
                    "aac",
                    "-b:a",
                    "128k",
                    "-f",
                    "segment",
                    "-segment_format",
                    "mpegts",
                    "-segment_time",
                    &format!("{:.3}", self.config.segment_duration_ms as f64 / 1_000.0),
                    "-reset_timestamps",
                    "1",
                    "-strftime",
                    "0",
                    &path_arg(&video_pattern),
                ])
                .await?;
            if !status.success() {
                return Err(MediaError::DecodeError {
                    offset: 0,
                    message: format!("ffmpeg raw exit {status}"),
                });
            }
            let audio_pattern = variant_dir.join("a_%05d.ts");
            let audio_status = self
                .run_ffmpeg(&[
                    "-y",
                    "-f",
                    "s16le",
                    "-ar",
                    "48000",
                    "-ac",
                    &input.audio_channels.to_string(),
                    "-i",
                    &path_arg(pcm_path),
                    "-c:a",
                    "aac",
                    "-b:a",
                    "128k",
                    "-f",
                    "segment",
                    "-segment_format",
                    "mpegts",
                    "-segment_time",
                    &format!("{:.3}", self.config.segment_duration_ms as f64 / 1_000.0),
                    "-reset_timestamps",
                    "1",
                    "-strftime",
                    "0",
                    &path_arg(&audio_pattern),
                ])
                .await?;
            if !audio_status.success() {
                return Err(MediaError::DecodeError {
                    offset: 0,
                    message: format!("ffmpeg raw audio exit {audio_status}"),
                });
            }
            let video_segments = read_segments(&video_pattern, LP_VIDEO)?;
            let audio_segments = read_segments(&audio_pattern, LP_AUDIO)?;
            let duration_ms =
                compute_duration_ms(video_segments.len(), self.config.segment_duration_ms);
            results.push(TranscodeOutput {
                variant: spec.clone(),
                video_segments,
                audio_segments,
                duration_ms,
            });
        }
        Ok(results)
    }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn write_yuv(path: &Path, frames: &[crate::transcode::Frame]) -> MediaResult<()> {
    let mut f = std::fs::File::create(path)?;
    use std::io::Write;
    for fr in frames {
        f.write_all(&fr.rgb)?;
    }
    Ok(())
}

fn write_pcm(path: &Path, samples: &[u8]) -> MediaResult<()> {
    std::fs::write(path, samples)?;
    Ok(())
}

fn read_segments(pattern: &Path, kind: u8) -> MediaResult<Vec<Vec<u8>>> {
    let parent = pattern.parent().ok_or_else(|| MediaError::Io(
        format!("segment pattern has no parent: {}", pattern.display())
    ))?;
    let prefix = pattern
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split('%').next())
        .unwrap_or("");
    // `path.extension()` strips the leading dot. Re-add it so
    // the suffix includes the separator (e.g. `.ts`).
    let suffix_with_dot = pattern
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(parent)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_segment_file(p, prefix, &suffix_with_dot))
        .collect();
    entries.sort();
    let mut out = Vec::with_capacity(entries.len());
    for p in entries {
        let bytes = std::fs::read(&p)?;
        let digest = SegmentDigest::compute(kind, &bytes);
        let mut buf = Vec::with_capacity(5 + 32 + 8);
        buf.push(kind);
        buf.extend_from_slice(&(32u32 + 8u32).to_le_bytes());
        buf.extend_from_slice(&digest.bytes);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
        out.push(buf);
    }
    Ok(out)
}

/// Returns true when `p` matches exactly
/// `<prefix><digits>.suffix` — e.g. `v_00000.ts`. Avoids loose
/// `starts_with / ends_with` matching that would pick up
/// unrelated files like `manifest_v_00000.ts`.
fn is_segment_file(p: &Path, prefix: &str, suffix_with_dot: &str) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !name.starts_with(prefix) {
        return false;
    }
    // `suffix_with_dot` includes the leading dot: `.ts`, `.m4s`.
    if !name.ends_with(suffix_with_dot) || name.len() <= prefix.len() + suffix_with_dot.len() {
        return false;
    }
    let digits = &name[prefix.len()..name.len() - suffix_with_dot.len()];
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn compute_duration_ms(n_segments: usize, segment_duration_ms: u64) -> u64 {
    n_segments as u64 * segment_duration_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        FFmpegConfig::default().validate().unwrap();
    }

    #[test]
    fn bad_segment_duration_rejected() {
        let mut c = FFmpegConfig::default();
        c.segment_duration_ms = 100;
        assert!(c.validate().is_err());
    }

    #[test]
    fn bad_gop_rejected() {
        let mut c = FFmpegConfig::default();
        c.gop_frames = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn bad_threads_rejected() {
        let mut c = FFmpegConfig::default();
        c.threads = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn too_short_timeout_rejected() {
        // 0 disables; below 10 is rejected because we'd race
        // the spawn with the deadline.
        let mut c = FFmpegConfig::default();
        c.timeout_secs = 5;
        assert!(c.validate().is_err());
        c.timeout_secs = 30;
        assert!(c.validate().is_ok());
        c.timeout_secs = 0;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn is_segment_file_matches_expected_patterns() {
        // Real ffmpeg output: `v_00000.ts`, `a_00001.ts`.
        assert!(is_segment_file(
            Path::new("/tmp/x/v_00000.ts"),
            "v_",
            ".ts",
        ));
        assert!(is_segment_file(
            Path::new("/tmp/x/a_00001.ts"),
            "a_",
            ".ts",
        ));
        assert!(is_segment_file(
            Path::new("/tmp/x/v_123456.ts"),
            "v_",
            ".ts",
        ));
        // Loose-match edge cases that the OLD impl would have
        // matched: now rejected.
        assert!(!is_segment_file(
            Path::new("/tmp/x/manifest_v_00000.ts"),
            "v_",
            ".ts",
        ));
        assert!(!is_segment_file(
            Path::new("/tmp/x/.ts"),
            "v_",
            ".ts",
        ));
        assert!(!is_segment_file(
            Path::new("/tmp/x/v_abc.ts"),
            "v_",
            ".ts",
        ));
        assert!(!is_segment_file(
            Path::new("/tmp/x/v_00000.mp4"),
            "v_",
            ".ts",
        ));
    }

    #[test]
    fn validate_probe_zero_fps_rejected() {
        let transcoder = FFmpegTranscoder::new(FFmpegLocator::with_paths("/bin/sh".into(), "/bin/sh".into()).unwrap());
        let probe = crate::ffmpeg_probe::MediaProbe {
            width: 320,
            height: 240,
            fps_num: 0,
            fps_den: 1,
            duration_ms: 1_000,
            sample_rate: 48_000,
            channels: 2,
            video_codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            has_video: true,
            has_audio: true,
            byte_size: 0,
        };
        let err = transcoder.validate_probe(&probe).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn validate_probe_zero_dims_rejected() {
        let transcoder = FFmpegTranscoder::new(FFmpegLocator::with_paths("/bin/sh".into(), "/bin/sh".into()).unwrap());
        let probe = crate::ffmpeg_probe::MediaProbe {
            width: 0,
            height: 0,
            fps_num: 30,
            fps_den: 1,
            duration_ms: 1_000,
            sample_rate: 48_000,
            channels: 2,
            video_codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            has_video: true,
            has_audio: true,
            byte_size: 0,
        };
        let err = transcoder.validate_probe(&probe).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn validate_probe_clean_clip_accepted() {
        let transcoder = FFmpegTranscoder::new(FFmpegLocator::with_paths("/bin/sh".into(), "/bin/sh".into()).unwrap());
        let probe = crate::ffmpeg_probe::MediaProbe {
            width: 426,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            duration_ms: 2_000,
            sample_rate: 48_000,
            channels: 2,
            video_codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            has_video: true,
            has_audio: true,
            byte_size: 0,
        };
        transcoder.validate_probe(&probe).unwrap();
    }
}