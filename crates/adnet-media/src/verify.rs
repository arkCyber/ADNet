//! Cross-manifest verification — SR-1, SR-2, SR-7.

use crate::config::{AV_DRIFT_TOLERANCE_MS, CLOCK_SKEW_TOLERANCE_MS};
use crate::error::MediaError;
use crate::manifest::MediaManifest;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifyStatus {
    Ok,
    ManifestCorrupt,
    ManifestClockSkew,
    AvDrift,
    DurationMismatch,
    FrameCountMismatch,
    BitrateMismatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub status: VerifyStatus,
    pub manifest_hash: String,
    pub variants: usize,
    pub segments: usize,
    pub details: Vec<String>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.status == VerifyStatus::Ok
    }
}

/// Verify a manifest in isolation.
pub fn verify_manifest(manifest: &MediaManifest) -> VerifyReport {
    let mut details = Vec::new();
    let verify_attempt = manifest.verify();
    let status = match verify_attempt {
        Ok(()) => VerifyStatus::Ok,
        Err(MediaError::ManifestHashMismatch { .. }) => VerifyStatus::ManifestCorrupt,
        Err(_) => VerifyStatus::ManifestCorrupt,
    };
    if status != VerifyStatus::Ok {
        details.push(format!("manifest hash mismatch: {:?}", verify_attempt.err()));
    }
    let seg_count: usize = manifest
        .variants
        .iter()
        .map(|v| v.segments.len())
        .sum::<usize>()
        + manifest.audio.segments.len();
    VerifyReport {
        status,
        manifest_hash: manifest.root.as_hex(),
        variants: manifest.variants.len(),
        segments: seg_count,
        details,
    }
}

/// Verify a full DAG: manifest + AV drift + clock skew + duration.
pub fn verify_dag(
    manifest: &MediaManifest,
    computed_duration_ms: u64,
    computed_av_drift_ms: i64,
    av_tolerance_ms: i64,
) -> VerifyReport {
    let mut details = Vec::new();
    let mut status = VerifyStatus::Ok;

    // 1. Manifest integrity.
    if let Err(e) = manifest.verify() {
        details.push(format!("manifest integrity: {}", e));
        return VerifyReport {
            status: VerifyStatus::ManifestCorrupt,
            manifest_hash: manifest.root.as_hex(),
            variants: manifest.variants.len(),
            segments: 0,
            details,
        };
    }

    // 2. Clock skew.
    let now = Utc::now().timestamp_millis();
    let drift = (now - manifest.created_unix_ms).abs();
    if drift > CLOCK_SKEW_TOLERANCE_MS {
        details.push(format!(
            "clock skew {} ms exceeds tolerance {} ms",
            drift, CLOCK_SKEW_TOLERANCE_MS
        ));
        status = VerifyStatus::ManifestClockSkew;
    }

    // 3. AV drift.
    if computed_av_drift_ms.abs() > av_tolerance_ms.max(AV_DRIFT_TOLERANCE_MS) {
        details.push(format!(
            "AV drift {} ms exceeds tolerance {} ms",
            computed_av_drift_ms,
            av_tolerance_ms.max(AV_DRIFT_TOLERANCE_MS)
        ));
        status = VerifyStatus::AvDrift;
    }

    // 4. Duration cross-check.
    if computed_duration_ms != manifest.declared_duration_ms {
        details.push(format!(
            "duration mismatch: declared {} ms, computed {} ms",
            manifest.declared_duration_ms, computed_duration_ms
        ));
        status = VerifyStatus::DurationMismatch;
    }

    let seg_count: usize = manifest
        .variants
        .iter()
        .map(|v| v.segments.len())
        .sum::<usize>()
        + manifest.audio.segments.len();

    VerifyReport {
        status,
        manifest_hash: manifest.root.as_hex(),
        variants: manifest.variants.len(),
        segments: seg_count,
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{AudioCodec, SampleFormat, VideoCodec};
    use crate::ingest::MediaIngester;
    use crate::transcode::Frame;

    fn make_manifest() -> MediaManifest {
        let ing = MediaIngester::default();
        let samples = vec![0u8; 48_000 * 2 * 2 * 4_000 / 1_000];
        let frames: Vec<Frame> = (0..120)
            .map(|i| Frame::solid(320, 240, (i & 0xFF) as u8, 0, 0))
            .collect();
        ing.ingest(
            samples,
            SampleFormat::S16,
            2,
            AudioCodec::Aac,
            frames,
            VideoCodec::H264,
            30,
        )
        .unwrap()
        .manifest
    }

    #[test]
    fn verify_manifest_ok() {
        let m = make_manifest();
        let r = verify_manifest(&m);
        assert_eq!(r.status, VerifyStatus::Ok);
        assert!(r.is_ok());
    }

    #[test]
    fn verify_manifest_detects_tampering() {
        let mut m = make_manifest();
        m.declared_duration_ms = 999_999;
        let r = verify_manifest(&m);
        assert_eq!(r.status, VerifyStatus::ManifestCorrupt);
        assert!(!r.is_ok());
    }

    #[test]
    fn verify_dag_flags_clock_skew() {
        let mut m = make_manifest();
        // Set the manifest to claim it was created 100 years ago.
        m.created_unix_ms -= 100i64 * 365 * 24 * 60 * 60 * 1_000;
        m.compute_root().unwrap();
        let r = verify_dag(&m, m.declared_duration_ms, 0, 50);
        assert!(matches!(r.status, VerifyStatus::ManifestClockSkew));
    }

    #[test]
    fn verify_dag_flags_av_drift() {
        let m = make_manifest();
        let r = verify_dag(&m, m.declared_duration_ms, 5_000, 50);
        assert!(matches!(r.status, VerifyStatus::AvDrift));
    }

    #[test]
    fn verify_dag_flags_duration_mismatch() {
        let m = make_manifest();
        let r = verify_dag(&m, m.declared_duration_ms + 1, 0, 50);
        assert!(matches!(r.status, VerifyStatus::DurationMismatch));
    }

    #[test]
    fn verify_dag_ok() {
        let m = make_manifest();
        let r = verify_dag(&m, m.declared_duration_ms, 0, 50);
        assert_eq!(r.status, VerifyStatus::Ok);
    }
}
