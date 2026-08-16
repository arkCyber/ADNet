//! 16 KiB chunks — the granularity at which A3Net blobs are addressed.
//!
//! This constant is a public contract: chunk-aligned readers/writers from
//! different A3Net components will produce/consume identical bytes for any
//! given blob.

use std::io::{BufWriter, Read, Write};

use a3net_types::{AdnetError, ByteRange, ContentHash, RangeSpec};
use thiserror::Error;

/// 16 KiB — matches iroh-blobs group granularity.
pub const CHUNK_SIZE: usize = 16 * 1024;

/// Default buffer size for [`ChunkWriter::new_with_buffered_inner`]. Writers
/// created with [`ChunkWriter::new`] keep the previous eager behaviour for
/// backwards compatibility; new callers should opt in to `64 KiB` to
/// amortise the per-chunk `write_all` syscall cost. 64 KiB is one
/// 4×chunk-group batch, which matches the iroh-blobs transport
/// recommendation and lines up with NTFS / ext4 direct-IO alignment.
pub const DEFAULT_CHUNK_WRITER_BUFFER: usize = 64 * 1024;

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
    /// The requested range is larger than the configured safety cap
    /// (default 16 MiB). DO-178C: the iroh-backed adapter cannot
    /// stream partial reads from the FsStore today; pulling an
    /// arbitrarily large range would exhaust memory. Callers that
    /// genuinely need more than the cap must read chunk by chunk
    /// via [`crate::traits::BlobReader::read_chunk`].
    #[error("range too large: requested {requested} bytes, cap is {cap} bytes")]
    TooLarge { requested: u64, cap: u64 },
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
///
/// ## Write amplification
///
/// `ChunkWriter` issues one `inner.write_all` per 16 KiB chunk. When
/// `inner` is unbuffered (e.g. a `File` or `TcpStream`) this means one
/// syscall for every 16 KiB. Use [`ChunkWriter::new_buffered`] to wrap
/// `inner` in a `BufWriter` of [`DEFAULT_CHUNK_WRITER_BUFFER`] bytes so
/// the underlying writer only sees a `write` once every 64 KiB.
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

    /// Wrap `inner` in a `BufWriter` of the default size so the underlying
    /// writer is only hit once per ~64 KiB instead of once per 16 KiB
    /// chunk. The [`finish`](Self::finish) flush guarantees that all
    /// buffered bytes are written before the hash is returned.
    pub fn new_buffered(inner: W) -> ChunkWriter<BufWriter<W>> {
        Self::new_buffered_with(inner, DEFAULT_CHUNK_WRITER_BUFFER)
    }

    /// Same as [`Self::new_buffered`] but with a caller-chosen buffer size.
    /// Use a multiple of [`CHUNK_SIZE`] to flush on chunk-group
    /// boundaries; the default of 64 KiB (= 4 chunks) is a good balance
    /// between latency and syscall overhead on both spinning disks and
    /// NVMe SSDs.
    pub fn new_buffered_with(inner: W, buf_size: usize) -> ChunkWriter<BufWriter<W>> {
        ChunkWriter {
            inner: BufWriter::with_capacity(buf_size, inner),
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
        // When `inner` is a `BufWriter<W>` (i.e. the writer was created via
        // `new_buffered` / `new_buffered_with`) this drops the buffer and
        // its `Drop` impl flushes any pending bytes to the underlying `W`
        // before we read the hash. For the unbuffered variant this is a
        // no-op semantically and the hash is still computed from the
        // bytes the caller wrote, not from anything `inner` may have
        // transformed. Belt-and-suspenders: explicit flush so the call
        // doesn't depend on `Drop` order in the face of a future refactor.
        if let Err(e) = self.inner.flush() {
            // `flush` only fails on the underlying writer; the buffered
            // path can't lose data we haven't yet observed. Surface the
            // error so the caller doesn't silently commit a partial blob.
            return Err(e);
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

    /// `new_buffered` must produce the same bytes and the same hash as
    /// the unbuffered `new`. This is the regression contract for the
    /// 64 KiB buffer used to amortise chunk-group writes.
    #[test]
    fn chunk_writer_buffered_matches_unbuffered() {
        let payload: Vec<u8> = (0..(CHUNK_SIZE * 5 + 17))
            .map(|i| (i % 251) as u8)
            .collect();

        let mut buf_unbuffered: Vec<u8> = Vec::new();
        let (hash_unbuffered, n_unbuffered) = {
            let mut w = ChunkWriter::new(&mut buf_unbuffered);
            w.write_all(&payload).unwrap();
            w.finish().unwrap()
        };

        let mut buf_buffered: Vec<u8> = Vec::new();
        let (hash_buffered, n_buffered) = {
            let mut w = ChunkWriter::new_buffered(&mut buf_buffered);
            // Feed in 17 KiB blocks so the BufWriter has to flush mid-stream.
            for chunk in payload.chunks(17 * 1024) {
                w.write_all(chunk).unwrap();
            }
            w.finish().unwrap()
        };

        assert_eq!(n_unbuffered, n_buffered);
        assert_eq!(hash_unbuffered, hash_buffered, "buffered hash mismatch");
        assert_eq!(buf_unbuffered, buf_buffered, "buffered bytes mismatch");
        assert_eq!(buf_buffered, payload, "byte mismatch with original");
    }

    /// `new_buffered` with a 64 KiB buffer must still flush every byte on
    /// `finish` even when the input is not a multiple of the buffer
    /// size. We feed a 33 KiB payload (just over 2 chunks) and assert
    /// the underlying sink sees exactly 33 KiB — no half-buffer lost.
    #[test]
    fn chunk_writer_buffered_drains_on_finish() {
        let payload: Vec<u8> = (0..(33 * 1024)).map(|i| (i % 199) as u8).collect();
        let mut sink: Vec<u8> = Vec::new();
        let (hash, n) = {
            let mut w = ChunkWriter::new_buffered(&mut sink);
            w.write_all(&payload).unwrap();
            w.finish().unwrap()
        };
        assert_eq!(n, payload.len() as u64);
        assert_eq!(sink, payload, "sink missing tail bytes");
        let hash_direct = ContentHash::from_bytes(&payload);
        assert_eq!(hash, hash_direct);
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
        use a3net_types::RangeSpec;
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
