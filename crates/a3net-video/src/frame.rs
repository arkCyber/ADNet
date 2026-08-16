//! Video frame types with strict integrity guarantees.
//!
//! All frames carry explicit length prefixes and are validated against
//! DO-178C DAL-B safety requirements (SR-6: truncation rejected).

use serde::{Deserialize, Serialize};
use std::fmt;

pub use crate::codec::FrameType;
use crate::codec::{PixelFormat, VideoCodec};
use crate::config::MAX_FRAME_BYTES;
use crate::error::{VideoError, VideoResult};

/// Length prefix byte for video frames.
pub const LP_VIDEO_FRAME: u8 = 0x56; // 'V'

/// Frame identifier (globally unique per stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameId {
    /// High 32 bits of timestamp (nanoseconds since epoch).
    pub ts_hi: u32,
    /// Low 32 bits of timestamp.
    pub ts_lo: u32,
    /// Sequence number (monotonically increasing per stream).
    pub seq: u32,
}

impl FrameId {
    /// Creates a new frame ID from a timestamp and sequence.
    pub fn new(timestamp_ns: u64, seq: u32) -> Self {
        Self {
            ts_hi: (timestamp_ns >> 32) as u32,
            ts_lo: timestamp_ns as u32,
            seq,
        }
    }

    /// Returns the timestamp in nanoseconds.
    pub fn timestamp_ns(&self) -> u64 {
        ((self.ts_hi as u64) << 32) | (self.ts_lo as u64)
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame@{:.6}s:{}", self.timestamp_ns() as f64 / 1e9, self.seq)
    }
}

/// Raw (uncompressed) video frame with pixel data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawFrame {
    /// Frame identifier.
    pub id: FrameId,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format.
    pub format: PixelFormat,
    /// Pixel data (owned buffer).
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    /// Presentation timestamp (nanoseconds since epoch).
    pub pts_ns: u64,
    /// Decode timestamp (nanoseconds since epoch).
    pub dts_ns: u64,
}

impl RawFrame {
    /// Creates a new raw frame with validation.
    pub fn new(
        id: FrameId,
        width: u32,
        height: u32,
        format: PixelFormat,
        data: Vec<u8>,
        pts_ns: u64,
        dts_ns: u64,
    ) -> VideoResult<Self> {
        // Validate dimensions are even for YUV420
        if format == PixelFormat::Yuv420 && (width % 2 != 0 || height % 2 != 0) {
            return Err(VideoError::InvalidConfig {
                param: "frame_dimensions",
                value: format!("{}x{}", width, height),
                reason: "YUV420 requires even dimensions",
            });
        }

        // Validate data size matches expected for format
        let expected_size = Self::expected_data_size(width, height, format)?;
        if data.len() != expected_size {
            return Err(VideoError::TruncatedFrame {
                expected: expected_size,
                actual: data.len(),
            });
        }

        // Validate timestamp monotonicity
        if dts_ns > pts_ns {
            return Err(VideoError::InvalidConfig {
                param: "timestamp",
                value: format!("dts={} > pts={}", dts_ns, pts_ns),
                reason: "DTS must not exceed PTS",
            });
        }

        Ok(Self {
            id,
            width,
            height,
            format,
            data,
            pts_ns,
            dts_ns,
        })
    }

    /// Creates a solid-color test frame.
    pub fn solid(width: u32, height: u32, r: u8, g: u8, b: u8) -> VideoResult<Self> {
        let format = PixelFormat::Rgba;
        let data_size = Self::expected_data_size(width, height, format)?;
        let mut data = vec![0u8; data_size];
        for i in (0..data.len()).step_by(4) {
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
            data[i + 3] = 255; // Alpha
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        Self::new(
            FrameId::new(ts, 0),
            width,
            height,
            format,
            data,
            ts,
            ts,
        )
    }

    /// Returns the expected data size for given dimensions and format.
    pub fn expected_data_size(width: u32, height: u32, format: PixelFormat) -> VideoResult<usize> {
        let rgb_size = (width as usize) * (height as usize) * 4; // RGBA
        let yuv_size = match format {
            PixelFormat::Yuv420 => {
                let chroma = (width as usize / 2) * (height as usize / 2);
                (width as usize * height as usize) + 2 * chroma
            }
            PixelFormat::Yuv422 => {
                let chroma = (width as usize / 2) * (height as usize) * 2;
                (width as usize * height as usize) + chroma
            }
            PixelFormat::Yuv444 => {
                (width as usize * height as usize) * 3
            }
            PixelFormat::Rgba | PixelFormat::Bgra => rgb_size,
        };
        Ok(yuv_size)
    }

    /// Validates frame size against maximum limit.
    pub fn validate_size(data_len: usize) -> VideoResult<()> {
        if data_len > MAX_FRAME_BYTES {
            return Err(VideoError::FrameTooLarge {
                size: data_len,
                limit: MAX_FRAME_BYTES,
            });
        }
        Ok(())
    }

    /// Returns the frame size in bytes.
    pub fn byte_size(&self) -> usize {
        4 + 4 + 4 + 4 + 1 + 4 + self.data.len() // header + data
    }

    /// Returns the presentation timestamp in seconds.
    pub fn pts_seconds(&self) -> f64 {
        self.pts_ns as f64 / 1e9
    }
}

/// Encoded (compressed) video frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodedFrame {
    /// Frame identifier.
    pub id: FrameId,
    /// Codec used for encoding.
    pub codec: VideoCodec,
    /// Frame type (keyframe or delta).
    pub frame_type: FrameType,
    /// Encoded bitstream data.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    /// Presentation timestamp (nanoseconds since epoch).
    pub pts_ns: u64,
    /// Decode timestamp (nanoseconds since epoch).
    pub dts_ns: u64,
    /// Is this a keyframe?
    pub is_keyframe: bool,
}

impl EncodedFrame {
    /// Creates a new encoded frame with validation.
    pub fn new(
        id: FrameId,
        codec: VideoCodec,
        frame_type: FrameType,
        data: Vec<u8>,
        pts_ns: u64,
        dts_ns: u64,
    ) -> VideoResult<Self> {
        // Validate size
        Self::validate_size(data.len())?;

        let is_keyframe = frame_type == FrameType::Keyframe;

        Ok(Self {
            id,
            codec,
            frame_type,
            data,
            pts_ns,
            dts_ns,
            is_keyframe,
        })
    }

    /// Returns the presentation timestamp in nanoseconds.
    pub fn pts_ns(&self) -> u64 {
        self.pts_ns
    }

    /// Returns the decode timestamp in nanoseconds.
    pub fn dts_ns(&self) -> u64 {
        self.dts_ns
    }

    /// Validates frame size against maximum limit.
    pub fn validate_size(data_len: usize) -> VideoResult<()> {
        if data_len > MAX_FRAME_BYTES {
            return Err(VideoError::FrameTooLarge {
                size: data_len,
                limit: MAX_FRAME_BYTES,
            });
        }
        Ok(())
    }

    /// Returns the frame size in bytes.
    pub fn byte_size(&self) -> usize {
        4 + 4 + 4 + 4 + 1 + self.data.len() // header + data
    }

    /// Returns a length-prefixed wire format for transport.
    pub fn to_wire_format(&self) -> Vec<u8> {
        let mut wire = Vec::with_capacity(5 + self.data.len());
        wire.push(LP_VIDEO_FRAME);
        wire.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        wire.extend_from_slice(&self.data);
        wire
    }

    /// Decodes a length-prefixed frame from wire format.
    pub fn from_wire_format(
        codec: VideoCodec,
        wire: &[u8],
        ts_ns: u64,
        seq: u32,
        frame_type: FrameType,
    ) -> VideoResult<Self> {
        if wire.len() < 5 {
            return Err(VideoError::TruncatedFrame {
                expected: 5,
                actual: wire.len(),
            });
        }

        let prefix = wire[0];
        if prefix != LP_VIDEO_FRAME {
            return Err(VideoError::TruncatedFrame {
                expected: LP_VIDEO_FRAME as usize,
                actual: prefix as usize,
            });
        }

        let declared_len = u32::from_le_bytes([wire[1], wire[2], wire[3], wire[4]]) as usize;
        if wire.len() < 5 + declared_len {
            return Err(VideoError::TruncatedFrame {
                expected: 5 + declared_len,
                actual: wire.len(),
            });
        }

        let data = wire[5..5 + declared_len].to_vec();
        Self::new(
            FrameId::new(ts_ns, seq),
            codec,
            frame_type,
            data,
            ts_ns,
            ts_ns,
        )
    }
}

/// Generic video frame wrapper (raw or encoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum VideoFrame {
    /// Raw (uncompressed) frame.
    Raw(RawFrame),
    /// Encoded (compressed) frame.
    Encoded(EncodedFrame),
}

impl VideoFrame {
    /// Returns the frame identifier.
    pub fn id(&self) -> FrameId {
        match self {
            VideoFrame::Raw(f) => f.id,
            VideoFrame::Encoded(f) => f.id,
        }
    }

    /// Returns the presentation timestamp.
    pub fn pts_ns(&self) -> u64 {
        match self {
            VideoFrame::Raw(f) => f.pts_ns,
            VideoFrame::Encoded(f) => f.pts_ns,
        }
    }

    /// Returns the decode timestamp.
    pub fn dts_ns(&self) -> u64 {
        match self {
            VideoFrame::Raw(f) => f.dts_ns,
            VideoFrame::Encoded(f) => f.dts_ns,
        }
    }

    /// Returns true if this is a keyframe.
    pub fn is_keyframe(&self) -> bool {
        match self {
            VideoFrame::Raw(_) => true, // All raw frames are self-contained
            VideoFrame::Encoded(f) => f.is_keyframe,
        }
    }

    /// Returns the frame size in bytes.
    pub fn byte_size(&self) -> usize {
        match self {
            VideoFrame::Raw(f) => f.byte_size(),
            VideoFrame::Encoded(f) => f.byte_size(),
        }
    }
}

/// Frame type enumeration for external use.
pub type Frame = VideoFrame;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_frame_validation() {
        // Valid RGBA frame
        let frame = RawFrame::solid(320, 240, 255, 0, 0).unwrap();
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 240);

        // Invalid: odd dimensions with YUV420
        assert!(RawFrame::new(
            FrameId::new(0, 0),
            321,
            240,
            PixelFormat::Yuv420,
            vec![0; 320 * 240 * 3 / 2],
            0,
            0,
        )
        .is_err());
    }

    #[test]
    fn encoded_frame_wire_format() {
        let frame = EncodedFrame::new(
            FrameId::new(1000, 5),
            VideoCodec::H264,
            FrameType::Keyframe,
            b"test data".to_vec(),
            1000,
            1000,
        )
        .unwrap();

        let wire = frame.to_wire_format();
        let decoded = EncodedFrame::from_wire_format(
            VideoCodec::H264,
            &wire,
            1000,
            5,
            FrameType::Keyframe,
        )
        .unwrap();

        assert_eq!(decoded.data, b"test data");
    }

    #[test]
    fn video_frame_wrapper() {
        let raw = RawFrame::solid(640, 480, 0, 255, 0).unwrap();
        let frame = VideoFrame::Raw(raw);
        assert!(frame.is_keyframe());
        // Just check it's non-zero and reasonable
        assert!(frame.byte_size() > 100);
    }
}
