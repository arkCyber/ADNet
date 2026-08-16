//! Media manifest — HLS-style description object that addresses
//! every variant segment and audio segment by content hash.
//!
//! The manifest is itself BLAKE3-addressed and is the single
//! source of truth at playback time. Verifying the manifest
//! digest is sufficient to validate every segment reference.

use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
use crate::error::{MediaError, MediaResult};
use crate::integrity::{manifest_hash, MediaDigest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRef {
    pub index: u32,
    pub duration_ms: u64,
    pub byte_size: u64,
    pub digest: MediaDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantManifest {
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: u32,
    pub codec: VideoCodec,
    pub fps: u32,
    pub segments: Vec<SegmentRef>,
    pub digest: MediaDigest,
}

impl VariantManifest {
    pub fn compute_digest(&mut self) -> MediaResult<()> {
        let mut buf = Vec::new();
        buf.extend_from_slice(self.label.as_bytes());
        buf.extend_from_slice(&self.width.to_le_bytes());
        buf.extend_from_slice(&self.height.to_le_bytes());
        buf.extend_from_slice(&self.bitrate_kbps.to_le_bytes());
        buf.extend_from_slice(&[self.codec as u8]);
        buf.extend_from_slice(&self.fps.to_le_bytes());
        for s in &self.segments {
            buf.extend_from_slice(&s.index.to_le_bytes());
            buf.extend_from_slice(&s.duration_ms.to_le_bytes());
            buf.extend_from_slice(&s.byte_size.to_le_bytes());
            buf.extend_from_slice(&s.digest.bytes);
        }
        self.digest = MediaDigest::from_bytes(manifest_hash(&buf));
        Ok(())
    }

    pub fn verify(&self) -> MediaResult<()> {
        let mut copy = self.clone();
        copy.compute_digest()?;
        if copy.digest != self.digest {
            return Err(MediaError::ManifestHashMismatch {
                expected: self.digest.as_hex(),
                actual: copy.digest.as_hex(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioManifest {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub sample_format: SampleFormat,
    pub avg_rms_q16: u32, // avg_rms * 65536, for integer-only hashing
    pub silence_ratio_q16: u32,
    pub segments: Vec<SegmentRef>,
    pub digest: MediaDigest,
}

impl AudioManifest {
    pub fn compute_digest(&mut self) -> MediaResult<()> {
        let mut buf = Vec::new();
        buf.push(self.codec as u8);
        buf.extend_from_slice(&self.sample_rate.to_le_bytes());
        buf.push(self.channels);
        buf.push(self.sample_format as u8);
        buf.extend_from_slice(&self.avg_rms_q16.to_le_bytes());
        buf.extend_from_slice(&self.silence_ratio_q16.to_le_bytes());
        for s in &self.segments {
            buf.extend_from_slice(&s.index.to_le_bytes());
            buf.extend_from_slice(&s.duration_ms.to_le_bytes());
            buf.extend_from_slice(&s.byte_size.to_le_bytes());
            buf.extend_from_slice(&s.digest.bytes);
        }
        self.digest = MediaDigest::from_bytes(manifest_hash(&buf));
        Ok(())
    }

    pub fn verify(&self) -> MediaResult<()> {
        let mut copy = self.clone();
        copy.compute_digest()?;
        if copy.digest != self.digest {
            return Err(MediaError::ManifestHashMismatch {
                expected: self.digest.as_hex(),
                actual: copy.digest.as_hex(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaManifest {
    pub manifest_version: u32,
    pub created_unix_ms: i64,
    pub declared_duration_ms: u64,
    pub declared_byte_size: u64,
    pub variants: Vec<VariantManifest>,
    pub audio: AudioManifest,
    pub root: MediaDigest,
}

impl MediaManifest {
    pub fn manifest_version() -> u32 {
        1
    }

    pub fn compute_root(&mut self) -> MediaResult<()> {
        // Verify each variant manifest first.
        for v in &self.variants {
            v.verify()?;
        }
        self.audio.verify()?;
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.manifest_version.to_le_bytes());
        buf.extend_from_slice(&self.created_unix_ms.to_le_bytes());
        buf.extend_from_slice(&self.declared_duration_ms.to_le_bytes());
        buf.extend_from_slice(&self.declared_byte_size.to_le_bytes());
        for v in &self.variants {
            buf.extend_from_slice(&v.digest.bytes);
        }
        buf.extend_from_slice(&self.audio.digest.bytes);
        let root_bytes = manifest_hash(&buf);
        self.root = MediaDigest::from_bytes(root_bytes);
        Ok(())
    }

    pub fn verify(&self) -> MediaResult<()> {
        let mut copy = self.clone();
        copy.compute_root()?;
        if copy.root != self.root {
            return Err(MediaError::ManifestHashMismatch {
                expected: self.root.as_hex(),
                actual: copy.root.as_hex(),
            });
        }
        Ok(())
    }
}

pub type MediaManifestV1 = MediaManifest;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest() -> MediaManifest {
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
        m
    }

    #[test]
    fn manifest_round_trip() {
        let m = make_manifest();
        let bytes = bincode::serialize(&m).unwrap();
        let decoded: MediaManifest = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn manifest_verify_ok() {
        let m = make_manifest();
        m.verify().unwrap();
    }

    #[test]
    fn manifest_tampering_detected() {
        let mut m = make_manifest();
        m.declared_duration_ms = 9_999;
        assert!(matches!(m.verify(), Err(MediaError::ManifestHashMismatch { .. })));
    }

    #[test]
    fn variant_tampering_detected() {
        let mut m = make_manifest();
        m.variants[0].width = 640;
        assert!(matches!(m.verify(), Err(MediaError::ManifestHashMismatch { .. })));
    }

    #[test]
    fn audio_tampering_detected() {
        let mut m = make_manifest();
        m.audio.silence_ratio_q16 = 65_535;
        assert!(matches!(m.verify(), Err(MediaError::ManifestHashMismatch { .. })));
    }
}
