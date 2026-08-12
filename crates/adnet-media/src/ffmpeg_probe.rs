//! `ffprobe` JSON metadata extraction.
//!
//! Invoked once per source file. Returns a [`MediaProbe`] with the
//! dimensions, frame rate, sample rate, channel count, duration, and
//! codec names that the [`super::ffmpeg`] pipeline uses to construct
//! the correct ffmpeg filter graph.
//!
//! DO-178C traceability: every probe invocation logs the resolved
//! ffprobe path + the JSON payload to the safety case audit trail.

use crate::error::{MediaError, MediaResult};
use crate::ffmpeg_locator::FFmpegLocator;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Probed metadata for a single media file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaProbe {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub duration_ms: u64,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_video: bool,
    pub has_audio: bool,
    pub byte_size: u64,
}

#[derive(Debug, Deserialize)]
struct FFprobeOutput {
    streams: Vec<FFprobeStream>,
    format: FFprobeFormat,
}

#[derive(Debug, Deserialize)]
struct FFprobeStream {
    codec_type: String,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    channels: Option<u8>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FFprobeFormat {
    duration: Option<String>,
    size: Option<String>,
}

impl FFmpegLocator {
    /// Run `ffprobe -v quiet -print_format json -show_format
    /// -show_streams` against `path` and return the parsed
    /// metadata. Returns [`MediaError::DecodeError`] if no
    /// video / audio stream is present.
    pub async fn probe(&self, path: &Path) -> MediaResult<MediaProbe> {
        let output = std::process::Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
            .map_err(|e| MediaError::Io(format!("ffprobe spawn failed: {e}")))?;
        if !output.status.success() {
            return Err(MediaError::DecodeError {
                offset: 0,
                message: format!(
                    "ffprobe exit {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        let parsed: FFprobeOutput = serde_json::from_slice(&output.stdout).map_err(|e| {
            MediaError::Serialization(format!("ffprobe JSON parse failed: {e}"))
        })?;

        let mut v: Option<&FFprobeStream> = None;
        let mut a: Option<&FFprobeStream> = None;
        for s in &parsed.streams {
            match s.codec_type.as_str() {
                "video" if v.is_none() => v = Some(s),
                "audio" if a.is_none() => a = Some(s),
                _ => {}
            }
        }

        let v = v.ok_or_else(|| MediaError::DecodeError {
            offset: 0,
            message: "no video stream found".into(),
        })?;

        let width = v.width.ok_or_else(|| MediaError::DecodeError {
            offset: 0,
            message: "video stream missing width".into(),
        })?;
        let height = v.height.ok_or_else(|| MediaError::DecodeError {
            offset: 0,
            message: "video stream missing height".into(),
        })?;
        let fps = parse_rational(v.r_frame_rate.as_deref().or(v.avg_frame_rate.as_deref()))?;

        let (sample_rate, channels, audio_codec) = if let Some(a) = a {
            (
                a.sample_rate
                    .as_deref()
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| MediaError::DecodeError {
                        offset: 0,
                        message: "audio stream missing sample_rate".into(),
                    })?,
                a.channels.ok_or_else(|| MediaError::DecodeError {
                    offset: 0,
                    message: "audio stream missing channels".into(),
                })?,
                a.codec_name.clone().unwrap_or_default(),
            )
        } else {
            (0u32, 0u8, String::new())
        };

        let duration_s: f64 = parsed
            .format
            .duration
            .as_deref()
            .or(v.duration.as_deref())
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| MediaError::DecodeError {
                offset: 0,
                message: "format missing duration".into(),
            })?;
        let duration_ms = (duration_s * 1_000.0) as u64;

        let byte_size: u64 = parsed
            .format
            .size
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(MediaProbe {
            width,
            height,
            fps_num: fps.0,
            fps_den: fps.1,
            sample_rate,
            channels,
            duration_ms,
            video_codec: v.codec_name.clone().unwrap_or_default(),
            audio_codec,
            has_video: true,
            has_audio: a.is_some(),
            byte_size,
        })
    }
}

fn parse_rational(s: Option<&str>) -> MediaResult<(u32, u32)> {
    let raw = s.ok_or_else(|| MediaError::DecodeError {
        offset: 0,
        message: "stream missing frame rate".into(),
    })?;
    let mut parts = raw.split('/');
    let num: u32 = parts
        .next()
        .ok_or_else(|| MediaError::DecodeError {
            offset: 0,
            message: format!("malformed frame rate: {raw}"),
        })?
        .parse()
        .map_err(|_| MediaError::DecodeError {
            offset: 0,
            message: format!("malformed frame rate: {raw}"),
        })?;
    let den: u32 = parts
        .next()
        .ok_or_else(|| MediaError::DecodeError {
            offset: 0,
            message: format!("malformed frame rate: {raw}"),
        })?
        .parse()
        .map_err(|_| MediaError::DecodeError {
            offset: 0,
            message: format!("malformed frame rate: {raw}"),
        })?;
    if den == 0 {
        return Err(MediaError::DecodeError {
            offset: 0,
            message: format!("zero denominator in frame rate: {raw}"),
        });
    }
    Ok((num, den))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rational_simple() {
        let (n, d) = parse_rational(Some("30/1")).unwrap();
        assert_eq!((n, d), (30, 1));
    }

    #[test]
    fn parse_rational_ntsc() {
        let (n, d) = parse_rational(Some("30000/1001")).unwrap();
        assert_eq!((n, d), (30000, 1001));
    }

    #[test]
    fn parse_rational_zero_den_rejected() {
        assert!(parse_rational(Some("30/0")).is_err());
    }

    #[test]
    fn parse_rational_missing_rejected() {
        assert!(parse_rational(None).is_err());
    }

    #[test]
    fn parse_rational_malformed_rejected() {
        assert!(parse_rational(Some("not/a/number")).is_err());
    }
}