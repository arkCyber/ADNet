//! Ingest pipeline — bytes → MediaDag.
//!
//! Coordinates: transcode → segment → audio analysis → manifest → DAG.
//! Returns an `IngestReport` containing every intermediate artifact
//! so the caller can persist them to the blobstore later.

use crate::audio::{analyze, AudioConfig, AudioEnergy};
use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
use crate::config::{MediaConfig, MAX_MEDIA_BYTES};
use crate::dag::{MediaDag, MediaDagBuilder};
use crate::error::{MediaError, MediaResult};
use crate::integrity::{MediaDigest, SegmentDigest};
use crate::manifest::{AudioManifest, MediaManifest, SegmentRef, VariantManifest};
use crate::segment::{Segment, Segmenter};
use crate::transcode::{Frame, PureTranscoder, TranscodeInput, TranscodeOutput, Transcoder};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub manifest: MediaManifest,
    pub dag: MediaDag,
    pub audio_energy: AudioEnergy,
    pub segments: Vec<Segment>,
    pub transcoder_outputs: Vec<TranscodeOutput>,
}

#[derive(Debug, Clone)]
pub struct MediaIngester {
    pub config: MediaConfig,
    pub audio_config: AudioConfig,
}

// In real deployment this accepts &dyn Transcoder. For the
// aerospace reference we hard-code PureTranscoder.
impl Default for MediaIngester {
    fn default() -> Self {
        Self {
            config: MediaConfig::default_short_video(),
            audio_config: AudioConfig::default(),
        }
    }
}

impl MediaIngester {
    pub fn new(config: MediaConfig) -> MediaResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            audio_config: AudioConfig::default(),
        })
    }

    /// Ingest raw PCM audio + RGB-888 frames. Pure-Rust backend
    /// only — the FFmpeg adapter would be a separate constructor.
    pub fn ingest(
        &self,
        samples: Vec<u8>,
        sample_format: SampleFormat,
        audio_channels: u8,
        audio_codec: AudioCodec,
        frames: Vec<Frame>,
        video_codec: VideoCodec,
        fps: u32,
    ) -> MediaResult<IngestReport> {
        if frames.is_empty() {
            return Err(MediaError::InputTooSmall { actual: 0, min: 1 });
        }
        if frames.len() as u64 > u32::MAX as u64 {
            return Err(MediaError::InputTooLarge {
                actual: frames.len() as u64,
                limit: u32::MAX as u64,
            });
        }
        if fps < 1 || fps > 120 {
            return Err(MediaError::InvalidConfig(format!(
                "fps {} out of [1, 120]",
                fps
            )));
        }
        let declared_byte_size = samples.len() as u64
            + frames.iter().map(|f| f.bytes_per_frame() as u64).sum::<u64>();
        if declared_byte_size > MAX_MEDIA_BYTES {
            return Err(MediaError::InputTooLarge {
                actual: declared_byte_size,
                limit: MAX_MEDIA_BYTES,
            });
        }

        let input = TranscodeInput {
            samples,
            sample_format,
            audio_channels,
            audio_codec,
            frames,
            video_codec,
            fps,
        };

        // 1. Transcode each variant.
        let transcoder = PureTranscoder;
        let mut transcoder_outputs = Vec::new();
        for v in self.config.ladder.iter() {
            let out = transcoder.transcode(&input, v)?;
            transcoder_outputs.push(out);
        }

        // 2. Segmenter.
        let mut segments = Vec::new();
        for out in &transcoder_outputs {
            let mut segs = Segmenter.slice(out)?;
            segments.append(&mut segs);
        }

        // 3. Audio analysis.
        let audio_energy = analyze(
            &input.samples,
            sample_format,
            audio_channels,
            self.config.audio_sample_rate,
            self.audio_config,
        )?;
        let avg_rms_q16 = (audio_energy.avg_rms * 65_536.0) as u32;
        let silence_ratio_q16 = (audio_energy.silence_ratio * 65_536.0) as u32;

        // 4. Build manifests.
        let mut variants = Vec::new();
        for out in &transcoder_outputs {
            let segs: Vec<SegmentRef> = out
                .video_segments
                .iter()
                .enumerate()
                .map(|(i, payload)| {
                    let hash = SegmentDigest::compute(crate::integrity::LP_VIDEO, payload);
                    SegmentRef {
                        index: i as u32,
                        duration_ms: out.duration_ms / out.video_segments.len() as u64,
                        byte_size: payload.len() as u64,
                        digest: MediaDigest::from_bytes(hash.bytes),
                    }
                })
                .collect();
            let mut vm = VariantManifest {
                label: out.variant.label.clone(),
                width: out.variant.width,
                height: out.variant.height,
                bitrate_kbps: out.variant.bitrate_kbps,
                codec: video_codec,
                fps,
                segments: segs,
                digest: MediaDigest::from_bytes([0u8; 32]),
            };
            vm.compute_digest()?;
            variants.push(vm);
        }

        let audio_segs: Vec<SegmentRef> = transcoder_outputs
            .first()
            .map(|o| {
                o.audio_segments
                    .iter()
                    .enumerate()
                    .map(|(i, payload)| {
                        let hash = SegmentDigest::compute(crate::integrity::LP_AUDIO, payload);
                        SegmentRef {
                            index: i as u32,
                            duration_ms: o.duration_ms / o.audio_segments.len().max(1) as u64,
                            byte_size: payload.len() as u64,
                            digest: MediaDigest::from_bytes(hash.bytes),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut audio_manifest = AudioManifest {
            codec: audio_codec,
            sample_rate: self.config.audio_sample_rate,
            channels: audio_channels,
            sample_format: sample_format,
            avg_rms_q16,
            silence_ratio_q16,
            segments: audio_segs,
            digest: MediaDigest::from_bytes([0u8; 32]),
        };
        audio_manifest.compute_digest()?;

        let mut manifest = MediaManifest {
            manifest_version: MediaManifest::manifest_version(),
            created_unix_ms: Utc::now().timestamp_millis(),
            declared_duration_ms: transcoder_outputs
                .first()
                .map(|o| o.duration_ms)
                .unwrap_or(0),
            declared_byte_size,
            variants,
            audio: audio_manifest,
            root: MediaDigest::from_bytes([0u8; 32]),
        };
        manifest.compute_root()?;

        // 5. DAG.
        let dag = MediaDagBuilder::build(&manifest, &transcoder_outputs)?;

        Ok(IngestReport {
            manifest,
            dag,
            audio_energy,
            segments,
            transcoder_outputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_runs_end_to_end() {
        let ing = MediaIngester::default();
        let samples = vec![0u8; 48_000 * 2 * 2 * 4_000 / 1_000];
        let frames: Vec<Frame> = (0..120)
            .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
            .collect();
        let report = ing
            .ingest(
                samples,
                SampleFormat::S16,
                2,
                AudioCodec::Aac,
                frames,
                VideoCodec::H264,
                30,
            )
            .unwrap();
        assert!(!report.segments.is_empty());
        assert!(report.manifest.variants.len() >= 4);
        report.manifest.verify().unwrap();
    }

    #[test]
    fn ingest_rejects_zero_frames() {
        let ing = MediaIngester::default();
        let err = ing
            .ingest(
                vec![0u8; 1024],
                SampleFormat::S16,
                2,
                AudioCodec::Aac,
                vec![],
                VideoCodec::H264,
                30,
            )
            .unwrap_err();
        assert!(matches!(err, MediaError::InputTooSmall { .. }));
    }

    #[test]
    fn ingest_rejects_invalid_fps() {
        let ing = MediaIngester::default();
        let frames = vec![Frame::solid(320, 240, 0, 0, 0)];
        let err = ing
            .ingest(
                vec![0u8; 1024],
                SampleFormat::S16,
                2,
                AudioCodec::Aac,
                frames,
                VideoCodec::H264,
                0,
            )
            .unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn ingest_rejects_oversized_payload() {
        let mut ing = MediaIngester::default();
        // Shrink the per-variant cap by overriding the limit via
        // declared_byte_size — we cannot modify MAX_MEDIA_BYTES at
        // runtime, so we exercise the limiter via a too-large
        // declared_byte_size that the LADDER would exceed.
        ing.config.audio_sample_rate = 48_000;
        let huge = 48_000u64 * 2 * 2 * (MAX_MEDIA_BYTES / 96_000 + 4);
        let samples = vec![0u8; huge as usize];
        let frames = vec![Frame::solid(320, 240, 0, 0, 0)];
        let err = ing
            .ingest(
                samples,
                SampleFormat::S16,
                2,
                AudioCodec::Aac,
                frames,
                VideoCodec::H264,
                30,
            )
            .unwrap_err();
        assert!(matches!(err, MediaError::InputTooLarge { .. }));
    }
}
