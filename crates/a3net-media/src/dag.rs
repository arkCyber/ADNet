//! Media DAG — a content-addressed directed acyclic graph of
//! every artifact produced by the ingest pipeline.
//!
//! ```text
//!   root
//!     ├── variant "240p"  → segments[0..N]
//!     ├── variant "480p"  → segments[0..N]
//!     ├── variant "720p"  → segments[0..N]
//!     └── audio track     → segments[0..N]
//! ```
//!
//! The DAG is itself a binary record (postcard-compatible) and
//! its top-level hash is the `MediaManifest.root`.

use crate::error::{MediaError, MediaResult};
use crate::integrity::{MediaDigest, SegmentDigest};
use crate::manifest::{AudioManifest, MediaManifest, SegmentRef, VariantManifest};
use crate::transcode::TranscodeOutput;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSegmentNode {
    pub digest: SegmentDigest,
    pub byte_size: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSegmentNode {
    pub digest: SegmentDigest,
    pub byte_size: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantNode {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub codec: u8, // serialised enum tag
    pub fps: u32,
    pub segments: Vec<VideoSegmentNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDag {
    pub root: MediaDigest,
    pub manifest_version: u32,
    pub created_unix_ms: i64,
    pub declared_duration_ms: u64,
    pub declared_byte_size: u64,
    pub variants: Vec<VariantNode>,
    pub audio: Option<AudioTrackNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrackNode {
    pub codec: u8,
    pub sample_rate: u32,
    pub channels: u8,
    pub sample_format: u8,
    pub avg_rms_q16: u32,
    pub silence_ratio_q16: u32,
    pub segments: Vec<AudioSegmentNode>,
}

#[derive(Debug, Clone)]
pub struct MediaDagBuilder;

impl MediaDagBuilder {
    pub fn build(
        manifest: &MediaManifest,
        outputs: &[TranscodeOutput],
    ) -> MediaResult<MediaDag> {
        if manifest.variants.len() != outputs.len() {
            return Err(MediaError::InvalidConfig(format!(
                "manifest has {} variants but {} transcoder outputs",
                manifest.variants.len(),
                outputs.len()
            )));
        }
        let mut v_nodes = Vec::new();
        for (vm, out) in manifest.variants.iter().zip(outputs.iter()) {
            let v = VariantNode {
                label: vm.label.clone(),
                width: vm.width,
                height: vm.height,
                bitrate_kbps: vm.bitrate_kbps,
                codec: vm.codec as u8,
                fps: vm.fps,
            segments: out
                .video_segments
                .iter()
                .enumerate()
                .map(|(_i, s)| {
                    let duration_ms = if out.video_segments.is_empty() {
                        0
                    } else {
                        out.duration_ms / out.video_segments.len() as u64
                    };
                    VideoSegmentNode {
                        digest: SegmentDigest::compute(crate::integrity::LP_VIDEO, s),
                        byte_size: s.len() as u64,
                        duration_ms,
                    }
                })
                .collect(),
            };
            v_nodes.push(v);
        }
        let audio = AudioTrackNode {
            codec: manifest.audio.codec as u8,
            sample_rate: manifest.audio.sample_rate,
            channels: manifest.audio.channels,
            sample_format: manifest.audio.sample_format as u8,
            avg_rms_q16: manifest.audio.avg_rms_q16,
            silence_ratio_q16: manifest.audio.silence_ratio_q16,
            segments: outputs
                .first()
                .map(|o| {
                    let per_seg = if o.audio_segments.is_empty() {
                        0
                    } else {
                        o.duration_ms / o.audio_segments.len() as u64
                    };
                    o.audio_segments
                        .iter()
                        .map(|s| AudioSegmentNode {
                            digest: SegmentDigest::compute(crate::integrity::LP_AUDIO, s),
                            byte_size: s.len() as u64,
                            duration_ms: per_seg,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        Ok(MediaDag {
            root: manifest.root.clone(),
            manifest_version: manifest.manifest_version,
            created_unix_ms: manifest.created_unix_ms,
            declared_duration_ms: manifest.declared_duration_ms,
            declared_byte_size: manifest.declared_byte_size,
            variants: v_nodes,
            audio: Some(audio),
        })
    }
}

impl MediaDag {
    pub fn to_manifest(&self) -> MediaResult<MediaManifest> {
        let variants = self
            .variants
            .iter()
            .map(|v| {
                let codec = match v.codec {
                    0 => crate::codec::VideoCodec::H264,
                    1 => crate::codec::VideoCodec::H265,
                    2 => crate::codec::VideoCodec::Av1,
                    3 => crate::codec::VideoCodec::Vp9,
                    _ => return Err(MediaError::InvalidConfig(format!(
                        "unknown video codec tag {}",
                        v.codec
                    ))),
                };
                let segments = v
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(i, s)| SegmentRef {
                        index: i as u32,
                        duration_ms: s.duration_ms,
                        byte_size: s.byte_size,
                        digest: MediaDigest::from_bytes(s.digest.bytes),
                    })
                    .collect();
                let mut vm = VariantManifest {
                    label: v.label.clone(),
                    width: v.width,
                    height: v.height,
                    bitrate_kbps: v.bitrate_kbps,
                    codec,
                    fps: v.fps,
                    segments,
                    digest: MediaDigest::from_bytes([0u8; 32]),
                };
                vm.compute_digest()?;
                Ok(vm)
            })
            .collect::<MediaResult<Vec<_>>>()?;
        let audio_node = self.audio.as_ref().ok_or_else(|| {
            MediaError::InvalidConfig("media DAG has no audio track".into())
        })?;
        let codec = match audio_node.codec {
            0 => crate::codec::AudioCodec::Aac,
            1 => crate::codec::AudioCodec::Opus,
            2 => crate::codec::AudioCodec::Mp3,
            3 => crate::codec::AudioCodec::Flac,
            _ => {
                return Err(MediaError::InvalidConfig(format!(
                    "unknown audio codec tag {}",
                    audio_node.codec
                )))
            }
        };
        let format = match audio_node.sample_format {
            0 => crate::codec::SampleFormat::S16,
            1 => crate::codec::SampleFormat::S24,
            2 => crate::codec::SampleFormat::F32,
            _ => {
                return Err(MediaError::InvalidConfig(format!(
                    "unknown sample format tag {}",
                    audio_node.sample_format
                )))
            }
        };
        let segments = audio_node
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| SegmentRef {
                index: i as u32,
                duration_ms: s.duration_ms,
                byte_size: s.byte_size,
                digest: MediaDigest::from_bytes(s.digest.bytes),
            })
            .collect();
        let mut audio = AudioManifest {
            codec,
            sample_rate: audio_node.sample_rate,
            channels: audio_node.channels,
            sample_format: format,
            avg_rms_q16: audio_node.avg_rms_q16,
            silence_ratio_q16: audio_node.silence_ratio_q16,
            segments,
            digest: MediaDigest::from_bytes([0u8; 32]),
        };
        audio.compute_digest()?;
        let mut m = MediaManifest {
            manifest_version: self.manifest_version,
            created_unix_ms: self.created_unix_ms,
            declared_duration_ms: self.declared_duration_ms,
            declared_byte_size: self.declared_byte_size,
            variants,
            audio,
            root: self.root.clone(),
        };
        m.compute_root()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
    use crate::config::{MediaConfig, VariantSpec};
    use crate::ingest::MediaIngester;
    use crate::transcode::{Frame, PureTranscoder, TranscodeInput, Transcoder};

    fn make_manifest_with_outputs() -> (MediaManifest, Vec<TranscodeOutput>) {
        let input = TranscodeInput {
            samples: vec![0u8; 48_000 * 2 * 2 * 4_000 / 1_000],
            sample_format: SampleFormat::S16,
            audio_channels: 2,
            audio_codec: AudioCodec::Aac,
            frames: (0..120)
                .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
                .collect(),
            video_codec: VideoCodec::H264,
            fps: 30,
        };
        let target = VariantSpec {
            label: "240p".into(),
            width: 320,
            height: 240,
            bitrate_kbps: 400,
        };
        let out = PureTranscoder.transcode(&input, &target).unwrap();
        let outputs = vec![out];
        let seg = SegmentRef {
            index: 0,
            duration_ms: 2_000,
            byte_size: 1024,
            digest: MediaDigest::from_bytes([7u8; 32]),
        };
        let mut v = VariantManifest {
            label: "240p".into(),
            width: 320,
            height: 240,
            bitrate_kbps: 400,
            codec: VideoCodec::H264,
            fps: 30,
            segments: vec![seg.clone()],
            digest: MediaDigest::from_bytes([0u8; 32]),
        };
        v.compute_digest().unwrap();
        let mut a = AudioManifest {
            codec: AudioCodec::Aac,
            sample_rate: 48_000,
            channels: 2,
            sample_format: SampleFormat::S16,
            avg_rms_q16: 0,
            silence_ratio_q16: 0,
            segments: vec![seg],
            digest: MediaDigest::from_bytes([0u8; 32]),
        };
        a.compute_digest().unwrap();
        let mut m = MediaManifest {
            manifest_version: 1,
            created_unix_ms: 0,
            declared_duration_ms: 4_000,
            declared_byte_size: 4096,
            variants: vec![v],
            audio: a,
            root: MediaDigest::from_bytes([0u8; 32]),
        };
        m.compute_root().unwrap();
        (m, outputs)
    }

    #[test]
    fn dag_builds_from_manifest_and_outputs() {
        let (m, outs) = make_manifest_with_outputs();
        let dag = MediaDagBuilder::build(&m, &outs).unwrap();
        assert_eq!(dag.root, m.root);
        assert_eq!(dag.variants.len(), 1);
        assert!(dag.audio.is_some());
    }

    #[test]
    fn dag_round_trips_through_manifest() {
        // Use a real ingest so the variant / audio digests in the
        // DAG match the digests in the original manifest.
        let ing = MediaIngester::default();
        let samples = vec![0u8; 48_000 * 2 * 2 * 4_000 / 1_000];
        let frames: Vec<Frame> = (0..120)
            .map(|i| Frame::solid(426, 240, (i & 0xFF) as u8, 0, 0))
            .collect();
        let report = ing
            .ingest(samples, SampleFormat::S16, 2, AudioCodec::Aac, frames, VideoCodec::H264, 30)
            .unwrap();
        let dag = MediaDagBuilder::build(&report.manifest, &report.transcoder_outputs).unwrap();
        let m2 = dag.to_manifest().unwrap();
        // The DAG reconstructs the manifest. The root is preserved
        // because the DAG carries the original root and the
        // variant / audio digests are recomputed deterministically.
        assert_eq!(m2.root, dag.root);
        assert_eq!(m2.declared_duration_ms, report.manifest.declared_duration_ms);
        assert_eq!(m2.declared_byte_size, report.manifest.declared_byte_size);
        // The rebuilt manifest must self-verify.
        m2.verify().unwrap();
    }

    #[test]
    fn dag_rejects_variant_count_mismatch() {
        let (m, _) = make_manifest_with_outputs();
        let err = MediaDagBuilder::build(&m, &[]).unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn dag_rejects_invalid_codec_tag() {
        let (m, outs) = make_manifest_with_outputs();
        let mut dag = MediaDagBuilder::build(&m, &outs).unwrap();
        dag.variants[0].codec = 99;
        let err = dag.to_manifest().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn dag_rejects_invalid_audio_codec_tag() {
        let (m, outs) = make_manifest_with_outputs();
        let mut dag = MediaDagBuilder::build(&m, &outs).unwrap();
        dag.audio.as_mut().unwrap().codec = 99;
        let err = dag.to_manifest().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn dag_rejects_invalid_sample_format_tag() {
        let (m, outs) = make_manifest_with_outputs();
        let mut dag = MediaDagBuilder::build(&m, &outs).unwrap();
        dag.audio.as_mut().unwrap().sample_format = 99;
        let err = dag.to_manifest().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn dag_rejects_missing_audio() {
        let (m, outs) = make_manifest_with_outputs();
        let mut dag = MediaDagBuilder::build(&m, &outs).unwrap();
        dag.audio = None;
        let err = dag.to_manifest().unwrap_err();
        assert!(matches!(err, MediaError::InvalidConfig(_)));
    }

    #[test]
    fn dag_serializes_and_restores() {
        let (m, outs) = make_manifest_with_outputs();
        let dag = MediaDagBuilder::build(&m, &outs).unwrap();
        let bytes = bincode::serialize(&dag).unwrap();
        let back: MediaDag = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, dag);
    }

    #[test]
    fn media_config_default_validates() {
        let _ = MediaConfig::default_short_video();
    }
}
