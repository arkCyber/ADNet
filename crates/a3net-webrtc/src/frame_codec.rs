//! Frame codec helpers for the WebRTC DataChannel.
//!
//! For the simple case (frames ≤ 16 KiB), the A3Net [`Frame`](a3net_types)
//! codec passes through unchanged. This module exists so that future
//! chunked-stream support has a clear home, and so that the integration
//! with [`crate::dc_session`] can have a stable interface.

use bytes::Bytes;

use crate::error::{WebRtcError, WebRtcResult};

/// The wire format of a single A3Net frame carried over the WebRTC
/// DataChannel.
///
/// Each frame is:
/// - 4-byte big-endian length prefix
/// - `length` bytes of payload
///
/// Identical to the wire format used by the QUIC transport — see
/// `a3net-transport::frame::FrameCodec`. We re-state it here so the
/// contract is documented in this crate too.
pub fn encode_frame(payload: &[u8]) -> WebRtcResult<Bytes> {
    if payload.len() > u32::MAX as usize {
        return Err(WebRtcError::Frame(format!(
            "payload too large: {}",
            payload.len()
        )));
    }
    let len = (payload.len() as u32).to_be_bytes();
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(payload);
    Ok(Bytes::from(out))
}

/// Try to decode a single frame from `buf`. Returns `Ok(None)` if `buf`
/// is too short.
pub fn try_decode(buf: &[u8]) -> WebRtcResult<Option<(Bytes, usize)>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return Ok(None);
    }
    Ok(Some((Bytes::copy_from_slice(&buf[4..4 + len]), 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let payload = b"hello a3net";
        let enc = encode_frame(payload).unwrap();
        let (decoded, consumed) = try_decode(&enc).unwrap().unwrap();
        assert_eq!(decoded.as_ref(), payload);
        assert_eq!(consumed, enc.len());
    }

    #[test]
    fn short_buffer_is_none() {
        let buf = [0u8, 0];
        let r = try_decode(&buf).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn truncated_body_is_none() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&10u32.to_be_bytes());
        buf.extend_from_slice(b"hello");
        let r = try_decode(&buf).unwrap();
        assert!(r.is_none());
    }
}
