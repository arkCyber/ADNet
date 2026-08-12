//! Media integrity — content hashing with length prefixes.
//!
//! SR-1: every segment is BLAKE3-addressed.
//! SR-2: every manifest is BLAKE3-rooted:
//!   `root_hash = blake3( len_prefix(0x01)
//!                       || variant_manifest_cid
//!                       || audio_manifest_cid
//!                       || declared_duration_ms
//!                       || declared_byte_size )`
//! SR-6: every decoder-bound payload is length-prefixed so a
//! truncated stream is detected, not over-read.

use crate::error::{MediaError, MediaResult};
use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// Length-prefix magic byte for video segment frames.
pub const LP_VIDEO: u8 = 0x01;
/// Length-prefix magic byte for audio segment frames.
pub const LP_AUDIO: u8 = 0x02;
/// Length-prefix magic byte for manifest records.
pub const LP_MANIFEST: u8 = 0x03;
/// Length-prefix magic byte for variant records.
pub const LP_VARIANT: u8 = 0x04;

/// BLAKE3-256 digest for a media segment payload.
pub fn segment_hash(payload: &[u8]) -> [u8; 32] {
    *blake3::hash(payload).as_bytes()
}

/// BLAKE3-256 digest for a manifest record — domain-separated
/// via the magic byte so segment hashes cannot collide with
/// manifest hashes.
pub fn manifest_hash(payload: &[u8]) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(&[LP_MANIFEST]);
    h.update(payload);
    *h.finalize().as_bytes()
}

/// Root content hash that binds variants + audio + duration.
/// Deterministically constructed so two ingesters with the same
/// input produce the same root.
pub fn media_root_hash(
    variant_manifest_cid: &[u8; 32],
    audio_manifest_cid: &[u8; 32],
    declared_duration_ms: u64,
    declared_byte_size: u64,
) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(&[LP_MANIFEST]);
    h.update(variant_manifest_cid);
    h.update(audio_manifest_cid);
    h.update(&declared_duration_ms.to_le_bytes());
    h.update(&declared_byte_size.to_le_bytes());
    *h.finalize().as_bytes()
}

/// Decode a length-prefixed payload.
///
/// On-wire format:
/// ```text
/// [u8 kind] [u32 LE length] [length bytes of payload]
/// ```
pub fn decode_lp(buf: &[u8]) -> MediaResult<(u8, &[u8])> {
    if buf.len() < 5 {
        return Err(MediaError::TruncatedFrame { expected: 5, actual: buf.len() });
    }
    let kind = buf[0];
    let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let total = 5 + len;
    if buf.len() < total {
        return Err(MediaError::TruncatedFrame { expected: total, actual: buf.len() });
    }
    Ok((kind, &buf[5..total]))
}

/// Encode a length-prefixed payload (advertised length only).
///
/// Returns the u8 kind + u32 LE length prefix. Callers append
/// the payload directly to amortize the allocation.
pub fn encode_lp_prefix(kind: u8, len: u32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0] = kind;
    out[1..5].copy_from_slice(&len.to_le_bytes());
    out
}

/// Verify frame length does not exceed the captioned payload.
pub fn check_length_prefix(kind: u8, advertised: u64, payload: u64) -> MediaResult<()> {
    if advertised > payload {
        return Err(MediaError::LengthPrefixOverflow { actual: advertised, payload });
    }
    // Reject empty variants for the chosen kind — protects the
    // decoder from a zero-byte frame that would be silently
    // interpreted as "no data".
    if advertised == 0 {
        return Err(MediaError::InvalidLp(kind));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDigest {
    pub bytes: [u8; 32],
}

impl MediaDigest {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }
    pub fn as_hex(&self) -> String {
        hex::encode(self.bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentDigest {
    pub kind: u8,
    pub bytes: [u8; 32],
    pub byte_size: u64,
}

impl SegmentDigest {
    pub fn compute(kind: u8, payload: &[u8]) -> Self {
        let mut h = Hasher::new();
        h.update(&[kind]);
        h.update(payload);
        Self {
            kind,
            bytes: *h.finalize().as_bytes(),
            byte_size: payload.len() as u64,
        }
    }
    /// Lowercase hex of the digest bytes. Matches the canonical
    /// encoding used by [`crate::persist::MediaStore`] for index
    /// sidecar keys.
    pub fn as_hex(&self) -> String {
        hex::encode(self.bytes)
    }
}

// Variant-aware codec tag enum (closed set).
#[allow(dead_code)]
pub(crate) const _CODEC_TAG_LOCK: u8 = LP_VIDEO; // keeps the export alive

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_hash_is_deterministic() {
        let a = segment_hash(b"hello");
        let b = segment_hash(b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn segment_hash_is_collision_free_for_known_inputs() {
        // Different inputs produce different hashes.
        assert_ne!(segment_hash(b"hello"), segment_hash(b"hellp"));
    }

    #[test]
    fn manifest_hash_is_domain_separated() {
        let payload = b"same";
        assert_ne!(segment_hash(payload), manifest_hash(payload));
    }

    #[test]
    fn root_hash_is_deterministic() {
        let a = media_root_hash(&[1u8; 32], &[2u8; 32], 1000, 4096);
        let b = media_root_hash(&[1u8; 32], &[2u8; 32], 1000, 4096);
        assert_eq!(a, b);
    }

    #[test]
    fn root_hash_changes_when_inputs_change() {
        let a = media_root_hash(&[1u8; 32], &[2u8; 32], 1000, 4096);
        let b = media_root_hash(&[1u8; 32], &[2u8; 32], 1001, 4096);
        assert_ne!(a, b);
    }

    #[test]
    fn lp_decode_full_round_trip() {
        let kind = LP_VIDEO;
        let payload = b"frame bytes";
        let mut buf = Vec::new();
        buf.push(kind);
        buf.extend_from_slice(&encode_lp_prefix(kind, payload.len() as u32)[1..5]);
        buf.extend_from_slice(payload);
        let (k, p) = decode_lp(&buf).unwrap();
        assert_eq!(k, kind);
        assert_eq!(p, payload);
    }

    #[test]
    fn lp_decode_truncated_errors() {
        let buf = [LP_VIDEO, 0, 0, 0, 10, 1, 2, 3];
        assert!(matches!(
            decode_lp(&buf),
            Err(MediaError::TruncatedFrame { .. })
        ));
    }

    #[test]
    fn lp_decode_header_only_errors() {
        let buf = [LP_VIDEO, 0, 0, 0];
        assert!(matches!(
            decode_lp(&buf),
            Err(MediaError::TruncatedFrame { .. })
        ));
    }

    #[test]
    fn check_length_prefix_zero_rejected() {
        assert!(matches!(
            check_length_prefix(LP_VIDEO, 0, 100),
            Err(MediaError::InvalidLp(_))
        ));
    }

    #[test]
    fn check_length_prefix_overflow_rejected() {
        assert!(matches!(
            check_length_prefix(LP_VIDEO, 200, 100),
            Err(MediaError::LengthPrefixOverflow { .. })
        ));
    }

    #[test]
    fn check_length_prefix_ok() {
        check_length_prefix(LP_VIDEO, 100, 200).unwrap();
        check_length_prefix(LP_VIDEO, 100, 100).unwrap();
    }
}
