//! Streaming CAR I/O for memory-efficient large DAG handling.
//!
//! This module provides `CarWriter` and `CarReader` for streaming
//! CAR file operations without loading all blocks into memory.
//!
//! ## Streaming Benefits
//!
//! - **Low memory footprint**: Process blocks one at a time
//! - **Early termination**: Stop reading once desired blocks are found
//! - **Pipe-friendly**: Works with stdin/stdout for shell integration
//! - **Backpressure support**: Write buffers are bounded
//!
//! ## Example: Streaming Export
//!
//! ```ignore
//! use adnet_blobstore::car::streaming::{CarWriter, CarHeader};
//!
//! let file = std::fs::File::create("export.car")?;
//! let mut writer = CarWriter::new(file);
//! writer.write_header(&CarHeader::new(vec![root_cid]))?;
//!
//! for block in store.iter() {
//!     writer.write_block(&block.cid, &block.data)?;
//! }
//! writer.finish()?;
//! ```

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};

use adnet_types::ContentHash;

use super::{CarBlock, CarError, CarHeader};

/// Read a varint from a reader.
fn read_varint<R: Read>(reader: &mut R) -> Result<u64, CarError> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf).map_err(CarError::Io)?;
        let byte = buf[0];
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift > 63 {
            return Err(CarError::InvalidFormat);
        }
    }
}

/// Write a varint to a writer.
fn write_varint<W: Write>(writer: &mut W, value: u64) -> Result<(), CarError> {
    let mut value = value;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            writer.write_all(&[byte]).map_err(CarError::Io)?;
            return Ok(());
        } else {
            byte |= 0x80;
            writer.write_all(&[byte]).map_err(CarError::Io)?;
        }
    }
}

/// Streaming CAR writer for incremental block writes.
///
/// ## Example
///
/// ```ignore
/// let file = std::fs::File::create("data.car")?;
/// let mut writer = CarWriter::new(file);
/// writer.write_header(&CarHeader::new(vec![root_cid]))?;
/// writer.write_block(&cid, data)?;
/// writer.finish()?;
/// ```
pub struct CarWriter<W: Write> {
    writer: W,
    buffer: Vec<u8>,
    block_count: usize,
}

impl<W: Write> CarWriter<W> {
    /// Create a new CAR writer.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            buffer: Vec::with_capacity(64 * 1024),
            block_count: 0,
        }
    }

    /// Write the CAR header.
    pub fn write_header(&mut self, header: &CarHeader) -> Result<(), CarError> {
        let cbor = header.to_cbor()?;
        write_varint(&mut self.writer, cbor.len() as u64)?;
        self.writer.write_all(&cbor)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Write a single block.
    pub fn write_block(&mut self, cid: &ContentHash, data: &[u8]) -> Result<(), CarError> {
        self.block_count += 1;

        // Convert hex string to bytes
        let cid_hex = cid.as_hex();
        let cid_bytes = hex::decode(cid_hex)
            .map_err(|_| CarError::InvalidHash(cid_hex.to_string()))?;
        write_varint(&mut self.writer, cid_bytes.len() as u64)?;
        self.writer.write_all(&cid_bytes)?;
        write_varint(&mut self.writer, data.len() as u64)?;
        self.writer.write_all(data)?;
        Ok(())
    }

    /// Write a `CarBlock` directly.
    pub fn write_car_block(&mut self, block: &CarBlock) -> Result<(), CarError> {
        self.write_block(&block.cid, &block.data)
    }

    /// Finish writing and flush any remaining data.
    pub fn finish(mut self) -> Result<(), CarError> {
        self.writer.flush()?;
        Ok(())
    }

    /// Get the number of blocks written.
    pub fn block_count(&self) -> usize {
        self.block_count
    }

    /// Consume and finish, returning the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Flush any buffered data to the underlying writer.
    pub fn flush(&mut self) -> Result<(), CarError> {
        self.writer.flush().map_err(CarError::Io)
    }
}

impl<W: Write> From<W> for CarWriter<W> {
    fn from(writer: W) -> Self {
        Self::new(writer)
    }
}

/// Extension trait for writing raw CAR data.
pub trait WriteCarExt {
    fn write_car(&mut self, header: &CarHeader, blocks: &[CarBlock]) -> Result<(), CarError>;
}

impl<W: Write> WriteCarExt for W {
    fn write_car(&mut self, header: &CarHeader, blocks: &[CarBlock]) -> Result<(), CarError> {
        let mut writer = CarWriter::new(self);
        writer.write_header(header)?;
        for block in blocks {
            writer.write_block(&block.cid, &block.data)?;
        }
        writer.finish()
    }
}

/// Streaming CAR reader for incremental block reads.
///
/// ## Example
///
/// ```ignore
/// let file = std::fs::File::open("data.car")?;
/// let mut reader = CarReader::new(file);
/// let header = reader.header()?;
/// for block_result in reader {
///     let block = block_result?;
///     store.put(&block.cid, &block.data)?;
/// }
/// ```
pub struct CarReader<R: Read> {
    reader: BufReader<R>,
    header: CarHeader,
    finished: bool,
}

impl<R: Read> CarReader<R> {
    /// Create a new CAR reader from a reader.
    pub fn new(reader: R) -> Result<Self, CarError> {
        let mut reader = BufReader::new(reader);
        let header = Self::read_header_inner(&mut reader)?;
        Ok(Self {
            reader,
            header,
            finished: false,
        })
    }

    /// Read the header without consuming it.
    fn read_header_inner(reader: &mut BufReader<R>) -> Result<CarHeader, CarError> {
        let header_len = read_varint(reader)?;
        let mut cbor = vec![0u8; header_len as usize];
        reader.read_exact(&mut cbor)?;
        CarHeader::from_cbor(&cbor)
    }

    /// Get the CAR header.
    pub fn header(&self) -> &CarHeader {
        &self.header
    }

    /// Get the roots from the header.
    pub fn roots(&self) -> &[ContentHash] {
        &self.header.roots
    }
}

impl<R: Read> Iterator for CarReader<R> {
    type Item = Result<CarBlock, CarError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let result = (|| {
            let cid_len = match read_varint(&mut self.reader) {
                Ok(n) => n,
                Err(CarError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    self.finished = true;
                    return Err(CarError::Io(e));
                }
                Err(e) => return Err(e),
            };

            let mut cid_bytes = vec![0u8; cid_len as usize];
            if let Err(e) = self.reader.read_exact(&mut cid_bytes) {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    self.finished = true;
                }
                return Err(CarError::Io(e));
            }

            let data_len = match read_varint(&mut self.reader) {
                Ok(n) => n,
                Err(CarError::Io(e)) if self.reader.buffer().is_empty() => {
                    self.finished = true;
                    return Err(CarError::Io(e));
                }
                Err(e) => return Err(e),
            };

            let mut data = vec![0u8; data_len as usize];
            if let Err(e) = self.reader.read_exact(&mut data) {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    self.finished = true;
                }
                return Err(CarError::Io(e));
            }

            let hex = hex::encode(&cid_bytes);
            let cid = ContentHash::from_hex(&hex)
                .map_err(|e| CarError::InvalidHash(e.to_string()))?;

            Ok(CarBlock::new(cid, data))
        })();

        match &result {
            Err(CarError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.finished = true;
                if result.is_err() && !matches!(result, Ok(_)) {
                    None
                } else {
                    Some(result)
                }
            }
            Ok(_) => Some(result),
            Err(_) => Some(result),
        }
    }
}

/// Async CAR reader for async I/O contexts.
pub struct AsyncCarReader<R> {
    inner: R,
}

impl<R: Read + Unpin> AsyncCarReader<R> {
    /// Create a new async CAR reader.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: Read + Unpin> Read for AsyncCarReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// Line-buffered CAR writer for text-based output.
///
/// Wraps a writer to provide line-buffered output, useful for
/// debugging or text-based CAR inspection.
pub struct LineBufferedCarWriter<W: Write> {
    inner: W,
    line_count: usize,
}

impl<W: Write> LineBufferedCarWriter<W> {
    /// Create a new line-buffered writer.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            line_count: 0,
        }
    }

    /// Write a debug line.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        writeln!(self.inner, "{}", line)?;
        self.line_count += 1;
        Ok(())
    }

    /// Get the number of lines written.
    pub fn line_count(&self) -> usize {
        self.line_count
    }
}

/// Buffered batch writer for efficient small block writes.
///
/// Accumulates blocks in memory and writes them in batches
/// to reduce system call overhead.
pub struct BatchedCarWriter<W: Write> {
    inner: W,
    batch: VecDeque<CarBlock>,
    batch_size: usize,
    max_batch_bytes: usize,
    bytes_buffered: usize,
}

impl<W: Write> BatchedCarWriter<W> {
    /// Create a new batched writer.
    pub fn new(inner: W) -> Self {
        Self::with_batch_size(inner, 100, 64 * 1024)
    }

    /// Create with custom batch parameters.
    pub fn with_batch_size(inner: W, batch_size: usize, max_batch_bytes: usize) -> Self {
        Self {
            inner,
            batch: VecDeque::new(),
            batch_size,
            max_batch_bytes,
            bytes_buffered: 0,
        }
    }

    /// Add a block to the batch, flushing if needed.
    pub fn write_block(&mut self, block: &CarBlock) -> Result<(), CarError> {
        let block_bytes = block.data.len() + block.cid.as_hex().len() / 2;
        self.batch.push_back(block.clone());
        self.bytes_buffered += block_bytes;

        if self.batch.len() >= self.batch_size || self.bytes_buffered >= self.max_batch_bytes {
            self.flush_batch()?;
        }
        Ok(())
    }

    /// Flush the current batch.
    pub fn flush_batch(&mut self) -> Result<(), CarError> {
        let mut writer = CarWriter::new(&mut self.inner);
        while let Some(block) = self.batch.pop_front() {
            writer.write_car_block(&block)?;
        }
        writer.finish()?;
        self.bytes_buffered = 0;
        Ok(())
    }

    /// Finish writing all batches.
    pub fn finish(mut self) -> Result<(), CarError> {
        self.flush_batch()
    }

    /// Get the number of batches written.
    pub fn batch_count(&self) -> usize {
        self.batch.len()
    }
}

/// Read a CAR file from a reader.
pub fn read_car<R: Read>(reader: R) -> Result<(CarHeader, Vec<CarBlock>), CarError> {
    let mut car_reader = CarReader::new(reader)?;
    let header = car_reader.header().clone();
    let mut blocks = Vec::new();
    for block_result in car_reader {
        blocks.push(block_result?);
    }
    Ok((header, blocks))
}

/// Write a CAR file to a writer.
pub fn write_car<W: Write>(writer: W, header: &CarHeader, blocks: &[CarBlock]) -> Result<(), CarError> {
    let mut car_writer = CarWriter::new(writer);
    car_writer.write_header(header)?;
    for block in blocks {
        car_writer.write_block(&block.cid, &block.data)?;
    }
    car_writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes(&[byte])
    }

    #[test]
    fn test_car_writer_basic() {
        let mut buf = Vec::new();
        let mut writer = CarWriter::new(&mut buf);

        let header = CarHeader::new(vec![sample_hash(0x42)]);
        writer.write_header(&header).unwrap();
        writer
            .write_block(&sample_hash(0x42), b"hello world")
            .unwrap();
        writer.finish().unwrap();

        // Verify the output
        let mut cursor = Cursor::new(&buf);
        let (header, blocks) = read_car(&mut cursor).unwrap();
        assert_eq!(header.roots.len(), 1);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data, b"hello world");
    }

    #[test]
    fn test_car_reader_basic() {
        // Create a CAR file manually
        let header = CarHeader::new(vec![sample_hash(0x42)]);
        let blocks = vec![CarBlock::new(sample_hash(0x42), b"test data".to_vec())];

        let mut buf = Vec::new();
        {
            let mut cursor = Cursor::new(&mut buf);
            let mut writer = CarWriter::new(&mut cursor);
            writer.write_header(&header).unwrap();
            for block in &blocks {
                writer.write_block(&block.cid, &block.data).unwrap();
            }
            writer.finish().unwrap();
        }

        // Read it back
        let reader = CarReader::new(Cursor::new(&buf)).unwrap();
        assert_eq!(reader.header().version, 1);
        assert_eq!(reader.roots().len(), 1);

        let read_blocks: Vec<_> = reader.collect();
        assert_eq!(read_blocks.len(), 1);
        assert_eq!(read_blocks[0].as_ref().unwrap().data, b"test data");
    }

    #[test]
    fn test_batched_writer() {
        let mut buf = Vec::new();
        let mut writer = BatchedCarWriter::with_batch_size(&mut buf, 3, 1024);

        let header = CarHeader::new(vec![]);
        // Note: header should be written separately via CarWriter
        let mut car_writer = CarWriter::new(&mut buf);
        car_writer.write_header(&header).unwrap();

        for i in 0..5 {
            let block = CarBlock::new(sample_hash(i), format!("block {}", i).into_bytes());
            writer.write_block(&block).unwrap();
        }
        writer.finish().unwrap();

        // Verify we can read it back
        let mut cursor = Cursor::new(&buf);
        let (_, blocks) = read_car(&mut cursor).unwrap();
        assert_eq!(blocks.len(), 5);
    }

    #[test]
    fn test_streaming_large_file() {
        // Test that streaming works correctly for large files
        let mut buf = Vec::new();
        let mut writer = CarWriter::new(&mut buf);

        let header = CarHeader::new(vec![sample_hash(0x01)]);
        writer.write_header(&header).unwrap();

        // Write many small blocks
        for i in 0..100 {
            let data = format!("block number {}", i);
            writer.write_block(&sample_hash(i as u8), data.as_bytes()).unwrap();
        }
        writer.finish().unwrap();

        // Read back with streaming
        let reader = CarReader::new(Cursor::new(&buf)).unwrap();
        let count = reader.count();
        assert_eq!(count, 100);
    }
}
