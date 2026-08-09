//! Length-prefixed frame codec used by every transport backend.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
/// Maximum frame body size (4 MiB). Prevents memory abuse from a hostile peer.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;

/// A single framed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame(pub Vec<u8>);

impl Frame {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn text(s: impl Into<String>) -> Self {
        Self(s.into().into_bytes())
    }

    pub fn from_json<T: serde::Serialize>(value: &T) -> serde_json::Result<Self> {
        Ok(Self(serde_json::to_vec(value)?))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Helpers to read/write frames over any `AsyncRead + AsyncWrite` pair.
pub struct FrameCodec;

impl FrameCodec {
    /// Synchronous encode (used for non-async framing like in-memory tests).
    pub fn encode(frame: &Frame) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + frame.len());
        let len = (frame.len() as u32).to_be_bytes();
        out.extend_from_slice(&len);
        out.extend_from_slice(&frame.0);
        out
    }

    /// Sync decode over an in-memory buffer.
    pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, super::TransportError> {
        if buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > MAX_FRAME_SIZE {
            return Err(super::TransportError::FrameTooLarge(len, MAX_FRAME_SIZE));
        }
        if buf.len() < 4 + len {
            return Ok(None);
        }
        Ok(Some((Frame(buf[4..4 + len].to_vec()), 4 + len)))
    }

    pub async fn write<W: AsyncWriteExt + Unpin>(
        w: &mut W,
        frame: &Frame,
    ) -> Result<(), super::TransportError> {
        if frame.len() > MAX_FRAME_SIZE {
            return Err(super::TransportError::FrameTooLarge(
                frame.len(),
                MAX_FRAME_SIZE,
            ));
        }
        let len = (frame.len() as u32).to_be_bytes();
        w.write_all(&len).await.map_err(super::TransportError::Io)?;
        w.write_all(&frame.0)
            .await
            .map_err(super::TransportError::Io)?;
        Ok(())
    }

    /// Read exactly one frame from the stream. Returns `Ok(None)` on clean EOF.
    pub async fn read<R: AsyncReadExt + Unpin>(
        r: &mut R,
    ) -> Result<Option<Frame>, super::TransportError> {
        let mut len_buf = [0u8; 4];
        match r.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(super::TransportError::Io(e)),
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_SIZE {
            return Err(super::TransportError::FrameTooLarge(len, MAX_FRAME_SIZE));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)
            .await
            .map_err(super::TransportError::Io)?;
        Ok(Some(Frame(body)))
    }

    /// Decode one frame from a `quinn::RecvStream` (or any `AsyncRead`).
    /// Returns `Ok(None)` on clean EOF or peer-initiated close (including
    /// "connection lost" variants that `quinn::RecvStream` may produce).
    pub async fn decode_stream<R: AsyncReadExt + Unpin>(
        r: &mut R,
    ) -> Result<Option<Frame>, String> {
        let mut len_buf = [0u8; 4];
        match r.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return Ok(None);
            }
            Err(e) if is_peer_close(&e) => return Ok(None),
            Err(e) => return Err(format!("read len: {e}")),
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_SIZE {
            return Err(format!("frame too large: {len} > {MAX_FRAME_SIZE}"));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)
            .await
            .map_err(|e| format!("read body: {e}"))?;
        Ok(Some(Frame(body)))
    }
}

fn is_peer_close(e: &io::Error) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("connection lost")
        || msg.contains("closed")
        || msg.contains("reset")
        || msg.contains("aborted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn frame_roundtrip() {
        let payload = Frame::new(b"hello adnet".to_vec());
        let mut buf = Vec::new();
        FrameCodec::write(&mut buf, &payload).await.unwrap();
        assert_eq!(buf.len(), 4 + payload.len());

        let mut cur = BufReader::new(buf.as_slice());
        let read = FrameCodec::read(&mut cur).await.unwrap().unwrap();
        assert_eq!(read, payload);
    }

    #[tokio::test]
    async fn frame_eof_yields_none() {
        let buf: &[u8] = &[];
        let mut cur = BufReader::new(buf);
        assert!(FrameCodec::read(&mut cur).await.unwrap().is_none());
    }

    #[test]
    fn sync_encode_decode_roundtrip() {
        let frame = Frame::text("hi");
        let bytes = FrameCodec::encode(&frame);
        let (decoded, consumed) = FrameCodec::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, bytes.len());
    }

    /// Defensive: a frame body larger than the wire cap is rejected by
    /// `decode_stream` (and by `read`) without reading the full body,
    /// so a hostile peer cannot OOM the process.
    #[tokio::test]
    async fn decode_rejects_oversized_frame() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        let claimed = (MAX_FRAME_SIZE + 1) as u32;
        buf.extend_from_slice(&claimed.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]); // truncated body
        let mut cur = Cursor::new(buf);
        let err = FrameCodec::decode_stream(&mut cur).await.unwrap_err();
        assert!(err.contains("frame too large"), "got {err}");
    }
}
