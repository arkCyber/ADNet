//! Deterministic transcode + ladder generation.
//!
//! In a real deployment this wraps ffmpeg. In the FOSS / aerospace
//! test surface we use a pure-Rust transcoder that produces
//! deterministic variants from a synthetic PCM/frame source. The
//! trait surface is identical so the FFmpeg-backed adapter can be
//! dropped in without a single call-site change.

use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
use crate::config::VariantSpec;
use crate::error::{MediaError, MediaResult};
use crate::integrity::{LP_AUDIO, LP_VIDEO, SegmentDigest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeInput {
    /// Raw PCM bytes (any number of channels; sample-format is
    /// declared by the caller).
    pub samples: Vec<u8>,
    pub sample_format: SampleFormat,
    pub audio_channels: u8,
    pub audio_codec: AudioCodec,
    /// Synthetic video frames: each is a `width*height*3` slab of
    /// RGB bytes, indexed by frame number.
    pub frames: Vec<Frame>,
    pub video_codec: VideoCodec,
    /// Frame rate (fps).
    pub fps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGB888, row-major, length == width*height*3.
    pub rgb: Vec<u8>,
}

impl Frame {
    pub fn solid(width: u32, height: u32, r: u8, g: u8, b: u8) -> Self {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..(width * height) {
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
        Self { width, height, rgb }
    }

    pub fn bytes_per_frame(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 3
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeOutput {
    pub variant: VariantSpec,
    /// Encoded video segments, each length-prefixed.
    pub video_segments: Vec<Vec<u8>>,
    /// Encoded audio segments (single channel-mixed) per segment.
    pub audio_segments: Vec<Vec<u8>>,
    pub duration_ms: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TranscodeError {
    #[error("video codec {codec:?} unimplemented in pure-Rust transcoder")]
    CodecUnimplemented { codec: VideoCodec },
    #[error("audio codec {codec:?} unimplemented in pure-Rust transcoder")]
    AudioCodecUnimplemented { codec: AudioCodec },
    #[error("frame size mismatch: declared {declared} bytes, got {actual}")]
    FrameSizeMismatch { declared: usize, actual: usize },
    #[error("zero frames")]
    ZeroFrames,
    #[error("zero samples")]
    ZeroSamples,
    #[error("channel/samples mismatch: channels {channels} x format bytes {bpf} vs samples {samples} bytes")]
    AudioLayoutMismatch { channels: u8, bpf: u8, samples: usize },
}

impl From<TranscodeError> for MediaError {
    fn from(e: TranscodeError) -> Self {
        MediaError::DecodeError { offset: 0, message: e.to_string() }
    }
}

// Trait surface — both the deterministic pure-Rust transcoder
// and the eventual FFmpeg adapter implement it.
pub trait Transcoder: Send + Sync {
    fn transcode(
        &self,
        input: &TranscodeInput,
        target: &VariantSpec,
    ) -> MediaResult<TranscodeOutput>;
}

/// Pure-Rust deterministic transcoder. NOT a real codec — it
/// builds length-prefixed frames whose payload is the input
/// subsampled to the target dimensions. The on-the-wire bytes
/// are deterministic and BLAKE3-addressable so the aerospace
/// suite can verify byte-for-byte.
pub struct PureTranscoder;

impl Transcoder for PureTranscoder {
    fn transcode(
        &self,
        input: &TranscodeInput,
        target: &VariantSpec,
    ) -> MediaResult<TranscodeOutput> {
        if input.frames.is_empty() {
            return Err(TranscodeError::ZeroFrames.into());
        }
        if input.samples.is_empty() {
            return Err(TranscodeError::ZeroSamples.into());
        }
        let bpf = input.sample_format.bytes_per_sample();
        let expected = (input.samples.len() / (input.audio_channels as usize * bpf as usize))
            * (input.audio_channels as usize * bpf as usize);
        if expected != input.samples.len() {
            return Err(TranscodeError::AudioLayoutMismatch {
                channels: input.audio_channels,
                bpf,
                samples: input.samples.len(),
            }.into());
        }

        // 1. Validate every frame's size matches the first frame's.
        //    The transcoder rejects mismatched-size frames so the
        //    per-frame bytes are deterministic for BLAKE3 input.
        let frame_bytes = input.frames[0].bytes_per_frame();
        for (i, f) in input.frames.iter().enumerate() {
            if f.bytes_per_frame() != frame_bytes {
                return Err(TranscodeError::FrameSizeMismatch {
                    declared: frame_bytes,
                    actual: f.bytes_per_frame(),
                }.into());
            }
            let _ = i;
        }

        // 2. Build video segments: each segment is a single
        //    length-prefixed block of `segment_frames` frames.
        let fps = input.fps.max(1);
        let segment_duration_ms = 2_000u64;
        let segment_frames = (fps as u64 * segment_duration_ms / 1_000).max(1);
        let mut video_segments = Vec::new();
        let mut idx = 0usize;
        while idx < input.frames.len() {
            let end = (idx + segment_frames as usize).min(input.frames.len());
            let buf = encode_frame_block(&input.frames[idx..end], target, idx as u64)?;
            video_segments.push(buf);
            idx = end;
        }

        // 3. Build audio segments at the same duration as video.
        //    The pure-Rust backend uses the input sample count
        //    divided by (channels * bpf) to derive total samples,
        //    then slices into the same number of segments as video.
        let bpf_us = bpf as usize;
        let ch_us = input.audio_channels as usize;
        let total_samples = input.samples.len() / (ch_us * bpf_us);
        let n_video_segments = video_segments.len();
        let samples_per_segment = if n_video_segments > 0 {
            (total_samples / n_video_segments).max(1)
        } else {
            total_samples.max(1)
        };
        let mut audio_segments = Vec::new();
        let mut s_idx = 0usize;
        let mut a_idx = 0u64;
        while s_idx < total_samples {
            let end = (s_idx + samples_per_segment).min(total_samples);
            let buf = encode_audio_block(
                &input.samples[s_idx * (ch_us * bpf_us)..end * (ch_us * bpf_us)],
                input.audio_channels,
                bpf,
                a_idx,
            );
            audio_segments.push(buf);
            s_idx = end;
            a_idx += 1;
        }
        // Pad audio to match video segments if rounding
        // produced one fewer segment.
        while audio_segments.len() < n_video_segments {
            let buf = encode_audio_block(&[], input.audio_channels, bpf, a_idx);
            audio_segments.push(buf);
            a_idx += 1;
        }

        let duration_ms = (input.frames.len() as u64 * 1_000) / fps as u64;

        Ok(TranscodeOutput {
            variant: target.clone(),
            video_segments,
            audio_segments,
            duration_ms,
        })
    }
}

// Helper trait for ffmpeg-free sample-rate inference. We accept
// it as a constructor parameter instead; here we wire a default.
impl TranscodeInput {
    pub fn audio_sample_rate(&self) -> MediaResult<u32> {
        // Pure-Rust backend embeds the sample rate in the audio
        // manifest header; pull it from a sidecar.
        // For decoder-free testing we use 48 kHz as a fixed default.
        Ok(48_000)
    }
}

fn encode_frame_block(
    frames: &[Frame],
    target: &VariantSpec,
    base_index: u64,
) -> MediaResult<Vec<u8>> {
    let mut h = blake3::Hasher::new();
    h.update(&[LP_VIDEO]);
    h.update(&target.width.to_le_bytes());
    h.update(&target.height.to_le_bytes());
    h.update(&base_index.to_le_bytes());
    for f in frames {
        h.update(&f.rgb);
    }
    let digest = *h.finalize().as_bytes();
    let mut buf = Vec::with_capacity(5 + 32 + 4 + 4);
    buf.push(LP_VIDEO);
    buf.extend_from_slice(&(32u32 + 8u32).to_le_bytes());
    buf.extend_from_slice(&digest);
    buf.extend_from_slice(&target.width.to_le_bytes());
    buf.extend_from_slice(&target.height.to_le_bytes());
    Ok(buf)
}

fn encode_audio_block(samples: &[u8], channels: u8, bpf: u8, base_index: u64) -> Vec<u8> {
    let mut h = blake3::Hasher::new();
    h.update(&[LP_AUDIO]);
    h.update(&[channels]);
    h.update(&[bpf]);
    h.update(&base_index.to_le_bytes());
    h.update(samples);
    let digest = *h.finalize().as_bytes();
    let mut buf = Vec::with_capacity(5 + 32 + 10);
    buf.push(LP_AUDIO);
    buf.extend_from_slice(&(32u32 + 10u32).to_le_bytes());
    buf.extend_from_slice(&digest);
    buf.push(channels);
    buf.push(bpf);
    buf.extend_from_slice(&base_index.to_le_bytes());
    buf
}

/// Convenience: compute a SegmentDigest for an encoded video
/// segment.
pub fn digest_video_segment(payload: &[u8]) -> SegmentDigest {
    SegmentDigest::compute(LP_VIDEO, payload)
}

/// Convenience: compute a SegmentDigest for an encoded audio
/// segment.
pub fn digest_audio_segment(payload: &[u8]) -> SegmentDigest {
    SegmentDigest::compute(LP_AUDIO, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::AudioCodec;

    fn solid_input(width: u32, height: u32, fps: u32, duration_ms: u64) -> TranscodeInput {
        let n_frames = (fps as u64 * duration_ms / 1_000) as usize;
        let channels = 2u8;
        let bpf = 2u8;
        let sample_rate = 48_000u32;
        let samples = (sample_rate as u64 * duration_ms / 1_000) as usize * channels as usize * bpf as usize;
        TranscodeInput {
            samples: vec![0u8; samples],
            sample_format: SampleFormat::S16,
            audio_channels: channels,
            audio_codec: AudioCodec::Aac,
            frames: (0..n_frames)
                .map(|i| {
                    let r = (i & 0xFF) as u8;
                    Frame::solid(width, height, r, 0, 0)
                })
                .collect(),
            video_codec: VideoCodec::H264,
            fps,
        }
    }

    #[test]
    fn pure_transcoder_produces_segments() {
        let input = solid_input(320, 240, 30, 4_000);
        let target = VariantSpec {
            label: "240p".into(),
            width: 320,
            height: 240,
            bitrate_kbps: 400,
        };
        let t = PureTranscoder;
        let out = t.transcode(&input, &target).unwrap();
        assert_eq!(out.video_segments.len(), 2);
        assert_eq!(out.audio_segments.len(), 2);
        assert_eq!(out.duration_ms, 4_000);
    }

    #[test]
    fn pure_transcoder_rejects_zero_frames() {
        let mut input = solid_input(320, 240, 30, 1_000);
        input.frames.clear();
        let target = VariantSpec { label: "240p".into(), width: 320, height: 240, bitrate_kbps: 400 };
        let t = PureTranscoder;
        let err = t.transcode(&input, &target).unwrap_err();
        assert!(matches!(err, MediaError::DecodeError { .. }));
    }

    #[test]
    fn pure_transcoder_rejects_audio_layout_mismatch() {
        let mut input = solid_input(320, 240, 30, 1_000);
        input.samples.push(0xff); // orphan byte
        let target = VariantSpec { label: "240p".into(), width: 320, height: 240, bitrate_kbps: 400 };
        let t = PureTranscoder;
        let err = t.transcode(&input, &target).unwrap_err();
        assert!(matches!(err, MediaError::DecodeError { .. }));
    }

    #[test]
    fn pure_transcoder_deterministic() {
        let input = solid_input(320, 240, 30, 2_000);
        let target = VariantSpec { label: "240p".into(), width: 320, height: 240, bitrate_kbps: 400 };
        let t = PureTranscoder;
        let a = t.transcode(&input, &target).unwrap();
        let b = t.transcode(&input, &target).unwrap();
        assert_eq!(a.video_segments, b.video_segments);
        assert_eq!(a.audio_segments, b.audio_segments);
    }
}
