//! CAR (Content Addressable aRchive) v1 import / export.
//!
//! CAR is the canonical IPFS bundle format: a header that names the
//! root set, followed by an unbounded stream of blocks. This
//! implementation is deliberately minimal — it implements the
//! `IPFS-CAR-v1.1.0` specification tightly enough to round-trip
//! blocks produced by this project, plus enough of the varint /
//! dag-cbor encoding to be useful in practice. It is **not** a
//! full CAR implementation: there is no dag-pb or unixfs
//! traversal, only raw block storage.
//!
//! File layout (per the spec):
//!
//! ```text
//! +---------+---------+---------+---------+---------+---------+
//! | varint  | header  | varint  | block   | varint  | block   | ...
//! +---------+---------+---------+---------+---------+---------+
//! ```
//!
//! The header is a single CBOR map with at least one entry,
//! `"version": 1`. We add `"roots": [cid, ...]` which is what
//! every CAR consumer expects. The CBOR encoder here is a hand-rolled
//! subset that covers only what the header needs (uint + bytes +
//! array + map + text). That avoids pulling a full CBOR dependency.

use std::io::{Read, Write};

use adnet_types::ContentHash;

/// A single CAR block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarBlock {
    /// Content hash (also acts as the CAR CID).
    pub cid: ContentHash,
    /// Raw block bytes.
    pub data: Vec<u8>,
}

impl CarBlock {
    pub fn new(cid: ContentHash, data: Vec<u8>) -> Self {
        Self { cid, data }
    }
}

/// Errors raised during CAR I/O.
#[derive(Debug, thiserror::Error)]
pub enum CarError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CBOR encoding error: {0}")]
    Cbor(String),
    #[error("CAR header is missing required field: {0}")]
    MissingHeaderField(&'static str),
    #[error("CAR version {0} is not supported (only version 1)")]
    UnsupportedVersion(u64),
    #[error("invalid content hash: {0}")]
    InvalidHash(String),
}

/// CAR header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarHeader {
    /// Spec version. Always `1`.
    pub version: u64,
    /// Root content hashes. May be empty.
    pub roots: Vec<ContentHash>,
}

impl CarHeader {
    pub fn new(roots: Vec<ContentHash>) -> Self {
        Self { version: 1, roots }
    }

    /// Serialize the header into a CBOR map.
    pub fn to_cbor(&self) -> Result<Vec<u8>, CarError> {
        // { "version": 1, "roots": [cid, cid, ...] }
        // where each cid is a tagged byte string with tag 42.
        let mut out = Vec::new();
        // indefinite-length map prelude
        out.push(0xbf);
        // "version" key (text string of length 7)
        cbor_text(&mut out, "version")?;
        cbor_uint(&mut out, self.version)?;
        // "roots" key
        cbor_text(&mut out, "roots")?;
        cbor_array_start(&mut out, self.roots.len())?;
        for cid in &self.roots {
            cbor_cid(&mut out, cid)?;
        }
        // break
        out.push(0xff);
        Ok(out)
    }

    pub fn from_cbor(bytes: &[u8]) -> Result<Self, CarError> {
        let mut dec = CborDecoder::new(bytes);
        dec.map()?;
        let mut version = None;
        let mut roots: Vec<ContentHash> = Vec::new();
        loop {
            if dec.peek_break()? {
                dec.consume_break()?;
                break;
            }
            let key = dec.text()?;
            match key.as_str() {
                "version" => version = Some(dec.uint()?),
                "roots" => {
                    let n = dec.array()?;
                    for _ in 0..n {
                        let cid_bytes = dec.cid()?;
                        let hex = hex_lower(&cid_bytes)
                            .ok_or_else(|| CarError::InvalidHash("not lowercase hex".into()))?;
                        let cid = ContentHash::from_hex(&hex)
                            .map_err(|e| CarError::InvalidHash(e.to_string()))?;
                        roots.push(cid);
                    }
                }
                _ => {
                    dec.skip()?;
                }
            }
        }
        let version = version.ok_or(CarError::MissingHeaderField("version"))?;
        if version != 1 {
            return Err(CarError::UnsupportedVersion(version));
        }
        Ok(Self { version, roots })
    }
}

/// Encode a CAR v1 file to `writer`. The header is emitted first,
/// followed by each block as a `varint(cid_len) + cid_bytes + varint(data_len) + data_bytes`.
pub fn write_car<W: Write>(
    writer: &mut W,
    header: &CarHeader,
    blocks: &[CarBlock],
) -> Result<(), CarError> {
    let cbor = header.to_cbor()?;
    write_varint(writer, cbor.len() as u64)?;
    writer.write_all(&cbor)?;
    for block in blocks {
        let cid_bytes = hex_to_lower_bytes(block.cid.as_hex())
            .ok_or_else(|| CarError::InvalidHash(block.cid.as_hex().to_string()))?;
        write_varint(writer, cid_bytes.len() as u64)?;
        writer.write_all(&cid_bytes)?;
        write_varint(writer, block.data.len() as u64)?;
        writer.write_all(&block.data)?;
    }
    Ok(())
}

/// Decode a CAR v1 file from `reader`. The header is parsed, then
/// blocks are read until EOF.
pub fn read_car<R: Read>(reader: &mut R) -> Result<(CarHeader, Vec<CarBlock>), CarError> {
    let header_len = read_varint(reader)?;
    let mut cbor = vec![0u8; header_len as usize];
    reader.read_exact(&mut cbor)?;
    let header = CarHeader::from_cbor(&cbor)?;
    let mut blocks = Vec::new();
    loop {
        let cid_len = match read_varint(reader) {
            Ok(n) => n,
            Err(CarError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        let mut cid_bytes = vec![0u8; cid_len as usize];
        reader.read_exact(&mut cid_bytes)?;
        let data_len = read_varint(reader)?;
        let mut data = vec![0u8; data_len as usize];
        reader.read_exact(&mut data)?;
        let hex = hex_lower(&cid_bytes)
            .ok_or_else(|| CarError::InvalidHash(format!("{:02x?}", cid_bytes)))?;
        let cid = ContentHash::from_hex(&hex).map_err(|e| CarError::InvalidHash(e.to_string()))?;
        blocks.push(CarBlock::new(cid, data));
    }
    Ok((header, blocks))
}

fn hex_to_lower_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = HEX_LOWER[bytes[i] as usize]?;
        let lo = HEX_LOWER[bytes[i + 1] as usize]?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_lower(bytes: &[u8]) -> Option<String> {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let hi = (b >> 4) as usize;
        let lo = (b & 0xf) as usize;
        s.push(HEX_CHARS[hi] as char);
        s.push(HEX_CHARS[lo] as char);
    }
    Some(s)
}

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
const HEX_LOWER: [Option<u8>; 256] = {
    let mut t: [Option<u8>; 256] = [None; 256];
    let chars = b"0123456789abcdef";
    let mut i = 0;
    while i < chars.len() {
        t[chars[i] as usize] = Some(i as u8);
        i += 1;
    }
    // Also accept uppercase for the `hex_lower` path used when
    // round-tripping our own header (always lowercase) but be lenient.
    let upper = b"0123456789ABCDEF";
    let mut i = 0;
    while i < upper.len() {
        if t[upper[i] as usize].is_none() {
            t[upper[i] as usize] = Some(i as u8);
        }
        i += 1;
    }
    t
};

/// Encode an unsigned integer as an LEB128 varint.
fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> std::io::Result<()> {
    while value >= 0x80 {
        writer.write_all(&[(value as u8) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])
}

fn read_varint<R: Read>(reader: &mut R) -> Result<u64, CarError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        let n = reader.read(&mut byte)?;
        if n == 0 {
            return Err(CarError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "varint truncated",
            )));
        }
        let b = byte[0];
        if shift >= 64 {
            return Err(CarError::Cbor("varint overflow".into()));
        }
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

// ---------------------------------------------------------------------------
// Tiny CBOR encoder/decoder. Implements only the shapes used by CAR headers.
// ---------------------------------------------------------------------------

fn cbor_text<W: Write>(w: &mut W, s: &str) -> std::io::Result<()> {
    let bytes = s.as_bytes();
    // CBOR text string: major type 3. Length ≤ 23 fits in the same byte.
    if bytes.len() <= 23 {
        w.write_all(&[0x60 | bytes.len() as u8])?;
    } else {
        // For longer strings emit a two-byte length encoding.
        w.write_all(&[0x78])?;
        write_varint(w, bytes.len() as u64)?;
    }
    w.write_all(bytes)
}

fn cbor_uint<W: Write>(w: &mut W, value: u64) -> std::io::Result<()> {
    w.write_all(&[0x1b])?; // uint major type 0, length 8
    w.write_all(&value.to_be_bytes())
}

fn cbor_array_start<W: Write>(w: &mut W, len: usize) -> std::io::Result<()> {
    // Use definite-length array if small enough; otherwise indefinite.
    if len < 24 {
        w.write_all(&[0x80 | len as u8])?;
    } else {
        w.write_all(&[0x9f])?;
    }
    Ok(())
}

/// Encode a `ContentHash` as a CBOR tag 42 byte string — the IPFS
/// "CID" tag. We omit the actual CID framing (multibase + version +
/// codec) because `ContentHash` already IS the multihash used by
/// this project; the receiving side reconstructs it via
/// `ContentHash::from_hex`.
fn cbor_cid<W: Write>(w: &mut W, cid: &ContentHash) -> std::io::Result<()> {
    w.write_all(&[0xc2, 0x58])?; // tag 42 + byte string (1-byte length follows)
    let bytes = hex_to_lower_bytes(cid.as_hex()).expect("ContentHash is valid hex");
    w.write_all(&[bytes.len() as u8])?;
    w.write_all(&bytes)
}

struct CborDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> CborDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn peek(&self) -> Result<u8, CarError> {
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| CarError::Cbor("unexpected end of CBOR".into()))
    }

    fn consume(&mut self) -> Result<u8, CarError> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn peek_break(&self) -> Result<bool, CarError> {
        Ok(self.peek()? == 0xff)
    }

    fn consume_break(&mut self) -> Result<(), CarError> {
        if self.consume()? != 0xff {
            return Err(CarError::Cbor("expected break".into()));
        }
        Ok(())
    }

    fn skip(&mut self) -> Result<(), CarError> {
        let major = self.consume()? >> 5;
        let len = self.read_len()?;
        match major {
            0 | 1 | 7 => {}
            2 | 3 => self.pos += len,
            4 | 5 => {
                for _ in 0..len {
                    self.skip()?;
                }
            }
            6 => {
                let _tag = self.consume()?;
                self.skip()?;
            }
            _ => return Err(CarError::Cbor("unknown major type".into())),
        }
        Ok(())
    }

    fn map(&mut self) -> Result<(), CarError> {
        let b = self.consume()?;
        let major = b >> 5;
        if major != 5 {
            return Err(CarError::Cbor("expected map".into()));
        }
        Ok(())
    }

    fn array(&mut self) -> Result<usize, CarError> {
        let b = self.consume()?;
        let major = b >> 5;
        if major != 4 {
            return Err(CarError::Cbor("expected array".into()));
        }
        let info = b & 0x1f;
        if info == 0x1f {
            // Indefinite-length: walk until break.
            let mut count = 0;
            while !self.peek_break()? {
                self.skip()?;
                count += 1;
            }
            self.consume_break()?;
            Ok(count)
        } else {
            Ok(info as usize)
        }
    }

    fn text(&mut self) -> Result<String, CarError> {
        let b = self.consume()?;
        let major = b >> 5;
        if major != 3 {
            return Err(CarError::Cbor("expected text string".into()));
        }
        let len = self.read_len_after_major(b)?;
        let s = std::str::from_utf8(&self.bytes[self.pos..self.pos + len])
            .map_err(|e| CarError::Cbor(e.to_string()))?;
        self.pos += len;
        Ok(s.to_string())
    }

    fn uint(&mut self) -> Result<u64, CarError> {
        let b = self.consume()?;
        let major = b >> 5;
        if major != 0 {
            return Err(CarError::Cbor("expected uint".into()));
        }
        let info = b & 0x1f;
        match info {
            n @ 0..=23 => Ok(n as u64),
            24 => {
                let v = *self
                    .bytes
                    .get(self.pos)
                    .ok_or_else(|| CarError::Cbor("truncated uint8".into()))?;
                self.pos += 1;
                Ok(v as u64)
            }
            25 => {
                let bytes: [u8; 2] = self.bytes[self.pos..self.pos + 2]
                    .try_into()
                    .map_err(|_| CarError::Cbor("truncated uint16".into()))?;
                self.pos += 2;
                Ok(u16::from_be_bytes(bytes) as u64)
            }
            26 => {
                let bytes: [u8; 4] = self.bytes[self.pos..self.pos + 4]
                    .try_into()
                    .map_err(|_| CarError::Cbor("truncated uint32".into()))?;
                self.pos += 4;
                Ok(u32::from_be_bytes(bytes) as u64)
            }
            27 => {
                let bytes: [u8; 8] = self.bytes[self.pos..self.pos + 8]
                    .try_into()
                    .map_err(|_| CarError::Cbor("truncated uint64".into()))?;
                self.pos += 8;
                Ok(u64::from_be_bytes(bytes))
            }
            _ => Err(CarError::Cbor("unsupported uint length".into())),
        }
    }

    /// Read a CID, which in this project is just a CBOR tag 42 byte
    /// string. Returns the inner bytes.
    fn cid(&mut self) -> Result<Vec<u8>, CarError> {
        let b = self.consume()?;
        if b != 0xc2 {
            return Err(CarError::Cbor("expected tag 42 (CID)".into()));
        }
        let b = self.consume()?;
        let major = b >> 5;
        if major != 2 {
            return Err(CarError::Cbor("expected byte string after tag 42".into()));
        }
        let len = self.read_len_after_major(b)?;
        let out = self.bytes[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(out)
    }

    fn read_len(&mut self) -> Result<usize, CarError> {
        let b = self.peek()?;
        let info = b & 0x1f;
        match info {
            n @ 0..=23 => {
                self.pos += 1;
                Ok(n as usize)
            }
            24 => {
                self.pos += 1;
                let v = *self
                    .bytes
                    .get(self.pos)
                    .ok_or_else(|| CarError::Cbor("truncated len".into()))?;
                self.pos += 1;
                Ok(v as usize)
            }
            _ => Err(CarError::Cbor("unsupported length".into())),
        }
    }

    /// Same as [`read_len`](Self::read_len), but the major byte has
    /// already been consumed; we reuse the bits in `major_byte`.
    fn read_len_after_major(&mut self, major_byte: u8) -> Result<usize, CarError> {
        let info = major_byte & 0x1f;
        match info {
            n @ 0..=23 => Ok(n as usize),
            24 => {
                let v = *self
                    .bytes
                    .get(self.pos)
                    .ok_or_else(|| CarError::Cbor("truncated len".into()))?;
                self.pos += 1;
                Ok(v as usize)
            }
            _ => Err(CarError::Cbor("unsupported length".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hash(byte: u8) -> ContentHash {
        // Build a valid 64-char hex string by hashing a single byte.
        ContentHash::from_bytes(&[byte])
    }

    #[test]
    fn varint_roundtrip() {
        for value in [0u64, 1, 127, 128, 16383, 16384, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).unwrap();
            let mut cursor = std::io::Cursor::new(buf);
            let decoded = read_varint(&mut cursor).unwrap();
            assert_eq!(value, decoded);
        }
    }

    #[test]
    fn header_roundtrip_empty_roots() {
        let h = CarHeader::new(vec![]);
        let cbor = h.to_cbor().unwrap();
        let parsed = CarHeader::from_cbor(&cbor).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn header_roundtrip_with_roots() {
        let h = CarHeader::new(vec![sample_hash(0x01), sample_hash(0x02)]);
        let cbor = h.to_cbor().unwrap();
        let parsed = CarHeader::from_cbor(&cbor).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn car_roundtrip_with_blocks() {
        let header = CarHeader::new(vec![sample_hash(0x42)]);
        let blocks = vec![
            CarBlock::new(sample_hash(0x42), b"hello world".to_vec()),
            CarBlock::new(sample_hash(0x99), vec![1, 2, 3, 4, 5, 6, 7, 8]),
        ];

        let mut buf = Vec::new();
        write_car(&mut buf, &header, &blocks).unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let (parsed_header, parsed_blocks) = read_car(&mut cursor).unwrap();

        assert_eq!(parsed_header, header);
        assert_eq!(parsed_blocks, blocks);
    }

    #[test]
    fn header_rejects_unsupported_version() {
        let mut bytes = vec![0xbf];
        cbor_text(&mut bytes, "version").unwrap();
        cbor_uint(&mut bytes, 2).unwrap();
        cbor_text(&mut bytes, "roots").unwrap();
        bytes.push(0x80);
        bytes.push(0xff);
        let result = CarHeader::from_cbor(&bytes);
        assert!(matches!(result, Err(CarError::UnsupportedVersion(2))));
    }

    #[test]
    fn hex_helpers_roundtrip() {
        let original = vec![0xde, 0xad, 0xbe, 0xef];
        let hex = hex_lower(&original).unwrap();
        assert_eq!(hex, "deadbeef");
        let back = hex_to_lower_bytes(&hex).unwrap();
        assert_eq!(back, original);
    }
}
