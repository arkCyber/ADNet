//! H.264 codec glue.
//!
//! We use the pure-Rust `h264-reader` crate (always available, see
//! the `h264-reader` workspace dep) for parsing. The optional
//! `h264 = ["dep:h264-reader"]` feature flag exists so callers can
//! gate the module on platforms that have a decoder.

#![allow(dead_code)]

use bytes::Bytes;

/// H.264 NAL unit type. We expose the type so higher layers can
/// distinguish keyframes from delta frames without parsing the bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NalUnitType {
    /// Non-IDR slice.
    Slice = 1,
    /// IDR (key) slice.
    IdrSlice = 5,
    /// SPS — Sequence Parameter Set.
    Sps = 7,
    /// PPS — Picture Parameter Set.
    Pps = 8,
    /// Access Unit Delimiter.
    Aud = 9,
    /// End of sequence.
    EndOfSequence = 10,
    /// End of stream.
    EndOfStream = 11,
    /// Anything else.
    Other(u8),
}

impl NalUnitType {
    pub fn from_byte(b: u8) -> Self {
        match b & 0x1f {
            1 => NalUnitType::Slice,
            5 => NalUnitType::IdrSlice,
            7 => NalUnitType::Sps,
            8 => NalUnitType::Pps,
            9 => NalUnitType::Aud,
            10 => NalUnitType::EndOfSequence,
            11 => NalUnitType::EndOfStream,
            other => NalUnitType::Other(other),
        }
    }

    pub fn is_keyframe(&self) -> bool {
        matches!(self, NalUnitType::IdrSlice | NalUnitType::Sps | NalUnitType::Pps)
    }
}

/// Iterate NAL units in an H.264 bitstream. Each NAL is preceded by
/// a 4-byte start code `00 00 00 01` (we don't currently support the
/// 3-byte variant). Returns `(nal_type, payload)`.
pub fn iter_nalus(stream: &[u8]) -> Vec<(NalUnitType, Bytes)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 < stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 0 && stream[i + 3] == 1 {
            i += 4;
            let start = i;
            // Find next start code
            let mut end = stream.len();
            let mut j = start + 3;
            while j + 3 < stream.len() {
                if stream[j] == 0 && stream[j + 1] == 0 && stream[j + 2] == 0 && stream[j + 3] == 1 {
                    end = j;
                    break;
                }
                j += 1;
            }
            if i < stream.len() {
                let nal_type = NalUnitType::from_byte(stream[i]);
                out.push((nal_type, Bytes::copy_from_slice(&stream[start..end])));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nal_type_roundtrip() {
        assert_eq!(NalUnitType::from_byte(0x65), NalUnitType::IdrSlice);
        assert_eq!(NalUnitType::from_byte(0x67), NalUnitType::Sps);
        assert_eq!(NalUnitType::from_byte(0x68), NalUnitType::Pps);
        assert!(NalUnitType::IdrSlice.is_keyframe());
        assert!(!NalUnitType::Slice.is_keyframe());
    }

    #[test]
    fn iter_nalus_basic() {
        // SPS + IDR, both preceded by a 4-byte start code.
        let stream: Vec<u8> = vec![
            0, 0, 0, 1, 0x67, 0x42, 0, 0,
            0, 0, 0, 1, 0x65, 0x88, 0x84, 0,
        ];
        let nals = iter_nalus(&stream);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].0, NalUnitType::Sps);
        assert_eq!(nals[1].0, NalUnitType::IdrSlice);
    }
}
