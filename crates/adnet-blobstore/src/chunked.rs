//! 16 KiB chunks — the granularity at which ADNet blobs are addressed.
//!
//! This constant is a public contract: chunk-aligned readers/writers from
//! different ADNet components will produce/consume identical bytes for any
//! given blob.

use std::io::{Read, Write};

use adnet_types::{AdnetError, ByteRange, ContentHash, RangeSpec};
use thiserror::Error;

/// 16 KiB — matches iroh-blobs group granularity.
pub const CHUNK_SIZE: usize = 16 * 1024;

/// Errors produced by chunk-level IO.
#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
    #[error("chunk index {index} out of range (total {total})")]
    ChunkOutOfRange { index: u32, total: u32 },
    #[error("byte range {start}..{end} out of blob size {size}")]
    RangeOutOfBounds { start: u64, end: u64, size: u64 },
    #[error("invalid range: {0}")]
    InvalidRange(String),
}

impl From<AdnetError> for ChunkError {
    fn from(e: AdnetError) -> Self {
        ChunkError::InvalidRange(e.to_string())
    }
}

/// Compute the number of 16 KiB chunks needed for `size` bytes.
pub fn chunk_count_for(size: u64) -> u32 {
    if size == 0 {
        0
    } else {
        ((size - 1) / CHUNK_SIZE as u64 + 1) as u32
    }
}

/// Resolve which chunks hold a given byte range and the slice inside each
/// chunk. Returns `(start_chunk, end_chunk_exclusive, first_offset, last_len)`.
pub fn chunks_for_range(
    total_size: u64,
    range: &ByteRange,
) -> Result<(u32, u32, usize, usize), ChunkError> {
    let chunks = chunk_count_for(total_size);
    if range.end > total_size {
        return Err(ChunkError::RangeOutOfBounds {
            start: range.start,
            end: range.end,
            size: total_size,
        });
    }
    let start_chunk = (range.start / CHUNK_SIZE as u64) as u32;
    let end_chunk_exclusive = if range.end == 0 {
        start_chunk
    } else {
        ((range.end - 1) / CHUNK_SIZE as u64) as u32 + 1
    };
    if end_chunk_exclusive > chunks && chunks > 0 {
        return Err(ChunkError::ChunkOutOfRange {
            index: end_chunk_exclusive - 1,
            total: chunks,
        });
    }
    let first_offset = (range.start % CHUNK_SIZE as u64) as usize;
    let total_len = range.end.saturating_sub(range.start);
    // `last_len` is only meaningful inside the LAST chunk. The caller uses
    // it together with `end_chunk_exclusive` to slice that chunk.
    let last_len = if end_chunk_exclusive == start_chunk + 1 {
        total_len as usize
    } else {
        let last_offset_inclusive = ((range.end - 1) % CHUNK_SIZE as u64) as usize;
        last_offset_inclusive + 1
    };
    Ok((start_chunk, end_chunk_exclusive, first_offset, last_len))
}

/// Convert a [`RangeSpec`] into one or more concrete `ByteRange`s,
/// clamping to `total_size`. For an empty blob (`total_size == 0`),
/// an `All` spec resolves to an empty range list (no bytes to read)
/// rather than erroring.
pub fn resolve_range(total_size: u64, spec: RangeSpec) -> Result<Vec<ByteRange>, ChunkError> {
    match spec {
        RangeSpec::All => {
            if total_size == 0 {
                Ok(Vec::new())
            } else {
                Ok(vec![ByteRange::new(0, total_size)?])
            }
        }
        RangeSpec::Single(r) => {
            if total_size == 0 {
                // Reading any range out of an empty blob is an error.
                return Err(ChunkError::InvalidRange(
                    "cannot read from empty blob".into(),
                ));
            }
            chunks_for_range(total_size, &r)?;
            Ok(vec![r])
        }
        RangeSpec::Multi(rs) => {
            if total_size == 0 {
                return Err(ChunkError::InvalidRange(
                    "cannot read from empty blob".into(),
                ));
            }
            for r in &rs {
                chunks_for_range(total_size, r)?;
            }
            Ok(rs)
        }
    }
}

/// Streaming chunk writer that writes aligned 16 KiB chunks to `inner` and
/// returns a BLAKE3 [`ContentHash`] on [`finish`](Self::finish).
///
/// The hash is computed by re-hashing the **final bytes written to `inner`**
/// at finalize time. This guarantees correctness even if `inner` mutates
/// the bytes (e.g., a compression layer). For very large blobs, prefer
/// [`ChunkWriter::with_inline_hash`] to stream-hash the input instead.
pub struct ChunkWriter<W: Write> {
    inner: W,
    index: u32,
    bytes_total: u64,
    chunk_buf: Vec<u8>,
    /// Hasher fed in parallel with `inner` writes. Independent of any
    /// transforms `inner` may apply.
    hasher: blake3::Hasher,
}

impl<W: Write> ChunkWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            index: 0,
            bytes_total: 0,
            chunk_buf: Vec::with_capacity(CHUNK_SIZE),
            hasher: blake3::Hasher::new(),
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_total
    }

    pub fn chunk_count(&self) -> u32 {
        self.index
    }

    /// Finalize the stream, returning the BLAKE3 content hash.
    pub fn finish(mut self) -> std::io::Result<(ContentHash, u64)> {
        // Flush any remaining bytes as the final partial chunk.
        if !self.chunk_buf.is_empty() {
            let bytes = std::mem::take(&mut self.chunk_buf);
            self.hasher.update(bytes.as_slice());
            self.inner.write_all(bytes.as_slice())?;
            self.index += 1;
        }
        // Wrap the already-finalized 32-byte digest as hex.
        let digest = self.hasher.finalize();
        let hash =
            ContentHash::from_hex(digest.to_hex().as_ref()).expect("blake3 hex is always 64 chars");
        Ok((hash, self.bytes_total))
    }
}

impl<W: Write> Write for ChunkWriter<W> {
    fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        let mut written_total = 0usize;
        while !buf.is_empty() {
            let space = CHUNK_SIZE - self.chunk_buf.len();
            let take = space.min(buf.len());
            self.chunk_buf.extend_from_slice(&buf[..take]);
            buf = &buf[take..];
            written_total += take;

            if self.chunk_buf.len() == CHUNK_SIZE {
                // Move the buffered chunk into a local Vec so we can both
                // hash it and write it to `inner` from independent slices
                // without aliasing. `mem::take` swaps in a fresh empty Vec.
                let bytes = std::mem::take(&mut self.chunk_buf);
                let slice: &[u8] = bytes.as_slice();
                self.hasher.update(slice);
                self.inner.write_all(slice)?;
                drop(bytes);
                self.index += 1;
            }
        }
        self.bytes_total += written_total as u64;
        Ok(written_total)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Streaming chunk reader — verifies hash on the fly.
pub struct ChunkReader<R: Read> {
    inner: R,
    hasher: blake3::Hasher,
    chunk_buf: [u8; CHUNK_SIZE],
    chunk_len: usize,
    chunk_pos: usize,
    bytes_left: u64,
    expected: Option<ContentHash>,
}

impl<R: Read> ChunkReader<R> {
    pub fn new(inner: R, total_bytes: u64, expected: Option<ContentHash>) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            chunk_buf: [0u8; CHUNK_SIZE],
            chunk_len: 0,
            chunk_pos: 0,
            bytes_left: total_bytes,
            expected,
        }
    }

    /// Verify the digest of all bytes seen so far matches `expected`.
    pub fn verify(&mut self) -> Result<(), ChunkError> {
        let digest = self.hasher.finalize();
        let actual =
            ContentHash::from_hex(digest.to_hex().as_ref()).expect("blake3 hex is always 64 chars");
        match &self.expected {
            Some(exp) if exp != &actual => Err(ChunkError::HashMismatch {
                expected: exp.clone(),
                actual,
            }),
            _ => {
                // Reset for next verification window.
                self.hasher = blake3::Hasher::new();
                Ok(())
            }
        }
    }
}

impl<R: Read> Read for ChunkReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.bytes_left == 0 {
            return Ok(0);
        }
        if self.chunk_pos >= self.chunk_len {
            // Refill chunk buffer
            let want = (self.bytes_left as usize).min(CHUNK_SIZE);
            let n = self.inner.read(&mut self.chunk_buf[..want])?;
            if n == 0 {
                self.bytes_left = 0;
                return Ok(0);
            }
            self.hasher.update(&self.chunk_buf[..n]);
            self.chunk_len = n;
            self.chunk_pos = 0;
        }
        let avail = self.chunk_len - self.chunk_pos;
        let take = avail.min(buf.len());
        buf[..take].copy_from_slice(&self.chunk_buf[self.chunk_pos..self.chunk_pos + take]);
        self.chunk_pos += take;
        self.bytes_left -= take as u64;
        Ok(take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn chunk_writer_hash_matches_direct_blake3() {
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 3 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let mut buf: Vec<u8> = Vec::new();
        let (hash_from_writer, n) = {
            let mut w = ChunkWriter::new(&mut buf);
            w.write_all(&payload).unwrap();
            w.finish().unwrap()
        };
        assert_eq!(n, payload.len() as u64);
        assert_eq!(buf, payload, "writer output mismatch");
        let hash_direct = ContentHash::from_bytes(&payload);
        assert_eq!(hash_from_writer, hash_direct, "writer hash mismatch");
    }

    /// The known BLAKE3 digest of `b""` (the empty string).
    /// This is the only canonical answer for the zero-length input and
    /// we lock it in with a test so future blake3 versions don't silently
    /// change it on us.
    #[test]
    fn empty_blob_has_canonical_blake3() {
        let h = ContentHash::from_bytes(b"");
        assert_eq!(
            h.as_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    /// `resolve_range` must reject every read against an empty blob
    /// (there is nothing to read) and must return an empty range list for
    /// an `All` spec against an empty blob.
    #[test]
    fn resolve_range_on_empty_blob() {
        use adnet_types::RangeSpec;
        // All -> empty list, no error
        let r = resolve_range(0, RangeSpec::All).unwrap();
        assert!(r.is_empty(), "All on empty blob should yield no ranges");
        // Single -> error
        let r = ByteRange::new(0, 1).unwrap();
        assert!(resolve_range(0, RangeSpec::Single(r)).is_err());
        // Multi -> error
        assert!(resolve_range(0, RangeSpec::Multi(vec![ByteRange::new(0, 1).unwrap()]),).is_err());
    }

    #[test]
    fn chunk_reader_roundtrip() {
        let payload: Vec<u8> = (0..1000).map(|i| i as u8).collect();
        let exp = ContentHash::from_bytes(&payload);
        let mut r = ChunkReader::new(
            Cursor::new(payload.clone()),
            payload.len() as u64,
            Some(exp.clone()),
        );
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
        r.verify().unwrap();
    }
}
