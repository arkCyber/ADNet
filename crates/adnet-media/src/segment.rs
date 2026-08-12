//! Segmenter — bounded-duration goblin-free windowing.
//!
//! The segmenter does NOT decode video. It is a deterministic
//! slicer that takes a `TranscodeOutput` and produces
//! `Segment` records each carrying:
//!   - a BLAKE3 digest
//!   - an explicit byte-size
//!   - a duration_ms
//!   - a sequence index (DAG position)
//!
//! Determinism guarantee: given the same `TranscodeOutput`,
//! `segment()` always emits the same per-segment digests.

use crate::error::{MediaError, MediaResult};
use crate::integrity::{LP_AUDIO, LP_VIDEO, SegmentDigest};
use crate::transcode::TranscodeOutput;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SegmentKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub kind: SegmentKind,
    pub index: u32,
    pub duration_ms: u64,
    pub byte_size: u64,
    pub digest: SegmentDigest,
    /// The raw segment payload, kept around so the DAG can
    /// serialize to a blobstore later.
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segmenter;

impl Segmenter {
    pub fn slice(&self, output: &TranscodeOutput) -> MediaResult<Vec<Segment>> {
        let mut out = Vec::new();
        let n = output.video_segments.len().max(output.audio_segments.len());
        for i in 0..n {
            if let Some(v) = output.video_segments.get(i) {
                let digest = SegmentDigest::compute(LP_VIDEO, v);
                out.push(Segment {
                    kind: SegmentKind::Video,
                    index: i as u32,
                    duration_ms: output.duration_ms / output.video_segments.len() as u64,
                    byte_size: v.len() as u64,
                    digest,
                    payload: v.clone(),
                });
            }
            if let Some(a) = output.audio_segments.get(i) {
                let digest = SegmentDigest::compute(LP_AUDIO, a);
                out.push(Segment {
                    kind: SegmentKind::Audio,
                    index: i as u32,
                    duration_ms: output.duration_ms / output.audio_segments.len() as u64,
                    byte_size: a.len() as u64,
                    digest,
                    payload: a.clone(),
                });
            }
        }
        if out.is_empty() {
            return Err(MediaError::InvalidConfig(
                "transcode output produced no segments".into(),
            ));
        }
        // Sort by (kind, index) for deterministic DAG construction.
        out.sort_by(|a, b| (a.kind as u8).cmp(&(b.kind as u8)).then(a.index.cmp(&b.index)));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
    use crate::config::VariantSpec;
    use crate::transcode::{Frame, PureTranscoder, TranscodeInput, Transcoder};

    fn make_output() -> TranscodeOutput {
        let input = TranscodeInput {
            samples: vec![0u8; 48_000 * 2 * 4_000 / 1_000],
            sample_format: SampleFormat::S16,
            audio_channels: 2,
            audio_codec: AudioCodec::Aac,
            frames: (0..120).map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0)).collect(),
            video_codec: VideoCodec::H264,
            fps: 30,
        };
        let target = VariantSpec { label: "240p".into(), width: 320, height: 240, bitrate_kbps: 400 };
        PureTranscoder.transcode(&input, &target).unwrap()
    }

    #[test]
    fn segmenter_produces_segments() {
        let out = make_output();
        let segs = Segmenter.slice(&out).unwrap();
        assert!(!segs.is_empty());
        // For every video segment there should be a matching audio segment.
        let v = segs.iter().filter(|s| s.kind == SegmentKind::Video).count();
        let a = segs.iter().filter(|s| s.kind == SegmentKind::Audio).count();
        assert_eq!(v, a);
    }

    #[test]
    fn segmenter_is_deterministic() {
        let out = make_output();
        let a = Segmenter.slice(&out).unwrap();
        let b = Segmenter.slice(&out).unwrap();
        // Compare digests — the payloads are also deterministic.
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.digest, y.digest);
        }
    }

    #[test]
    fn segmenter_indices_are_monotonic() {
        let out = make_output();
        let segs = Segmenter.slice(&out).unwrap();
        let mut last_v: Option<u32> = None;
        let mut last_a: Option<u32> = None;
        for s in &segs {
            match s.kind {
                SegmentKind::Video => {
                    if let Some(prev) = last_v {
                        assert_eq!(s.index, prev + 1);
                    }
                    last_v = Some(s.index);
                }
                SegmentKind::Audio => {
                    if let Some(prev) = last_a {
                        assert_eq!(s.index, prev + 1);
                    }
                    last_a = Some(s.index);
                }
            }
        }
    }

    #[test]
    fn segmenter_rejects_empty() {
        let empty = TranscodeOutput {
            variant: VariantSpec { label: "x".into(), width: 1, height: 1, bitrate_kbps: 1 },
            video_segments: vec![],
            audio_segments: vec![],
            duration_ms: 0,
        };
        let err = Segmenter.slice(&empty).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }
}
