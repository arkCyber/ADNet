//! Byte ranges — iroh-blobs lets a peer fetch a sub-range of a blob
//! (`BlobTicket` with offset + length). ADNet mirrors that with
//! [`RangeSpec`].

use serde::{Deserialize, Serialize};

use crate::error::{AdnetError, Result};

/// Inclusive byte range, half-open `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Construct a new half-open `[start, end)` range. Returns an error
    /// if `start >= end` (the range is empty or inverted) or if the
    /// `end` value would overflow when other operations add to it.
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if start >= end {
            return Err(AdnetError::InvalidTicket(format!(
                "range start {start} must be strictly less than end {end}"
            )));
        }
        Ok(Self { start, end })
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    pub fn whole(size: u64) -> Self {
        Self {
            start: 0,
            end: size,
        }
    }
}

/// Range selector — either a single contiguous range or a list of disjoint
/// ranges (the "ranges" form of HTTP `Range:`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RangeSpec {
    /// Fetch the entire blob.
    #[default]
    All,
    /// Fetch a single contiguous byte range.
    Single(ByteRange),
    /// Fetch several disjoint byte ranges.
    Multi(Vec<ByteRange>),
}

impl RangeSpec {
    /// Convenience: a single `[start, end)` range. Returns `Err` if
    /// `start > end`.
    pub fn single(start: u64, end: u64) -> Result<Self> {
        Ok(Self::Single(ByteRange::new(start, end)?))
    }

    pub fn all() -> Self {
        Self::All
    }

    /// Render as an HTTP `Range:` header value (`bytes=...`) or `None` for
    /// the whole blob.
    pub fn to_http_header(&self) -> Option<String> {
        match self {
            Self::All => None,
            Self::Single(r) => Some(format!("bytes={}-{}", r.start, r.end.saturating_sub(1))),
            Self::Multi(rs) => {
                let parts: Vec<String> = rs
                    .iter()
                    .map(|r| format!("{}-{}", r.start, r.end.saturating_sub(1)))
                    .collect();
                Some(format!("bytes={}", parts.join(",")))
            }
        }
    }

    /// Parse an HTTP `Range:` header value (`bytes=0-99,200-299`) into a
    /// [`RangeSpec`]. `total_size` is needed to resolve "open-ended" ranges
    /// like `bytes=1000-` (from byte 1000 to EOF).
    pub fn from_http_header(header: &str, total_size: u64) -> Result<Self> {
        let body = header
            .strip_prefix("bytes=")
            .ok_or_else(|| AdnetError::InvalidTicket(format!("range header: {header}")))?;
        if body.is_empty() {
            return Err(AdnetError::InvalidTicket(format!("range header: {header}")));
        }
        let mut ranges = Vec::new();
        for part in body.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(AdnetError::InvalidTicket(format!("range header: {header}")));
            }
            let (start_s, end_s) = match part.split_once('-') {
                Some(be) => be,
                None => return Err(AdnetError::InvalidTicket(format!("range header: {header}"))),
            };
            let start: u64;
            let end: u64;
            if start_s.is_empty() {
                // suffix range: bytes=-N → last N bytes
                let suffix: u64 = end_s
                    .parse()
                    .map_err(|_| AdnetError::InvalidTicket(format!("range header: {header}")))?;
                start = total_size.saturating_sub(suffix);
                end = total_size;
            } else {
                start = start_s
                    .parse()
                    .map_err(|_| AdnetError::InvalidTicket(format!("range header: {header}")))?;
                if start >= total_size {
                    return Err(AdnetError::InvalidTicket(format!(
                        "range start {start} >= total_size {total_size}"
                    )));
                }
                let end_incl: u64 = if end_s.is_empty() {
                    total_size.saturating_sub(1)
                } else {
                    end_s
                        .parse::<u64>()
                        .map_err(|_| AdnetError::InvalidTicket(format!("range header: {header}")))?
                };
                // ByteRange uses exclusive end, HTTP uses inclusive end
                end = end_incl.saturating_add(1).min(total_size);
            }
            ranges.push(ByteRange::new(start, end)?);
        }
        Ok(if ranges.len() == 1 {
            RangeSpec::Single(ranges.remove(0))
        } else {
            RangeSpec::Multi(ranges)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_range() {
        let r = ByteRange::whole(1024);
        assert_eq!(r.start, 0);
        assert_eq!(r.end, 1024);
        assert_eq!(r.len(), 1024);
    }

    #[test]
    fn range_invalid() {
        // Inverted range rejected.
        assert!(ByteRange::new(10, 5).is_err());
        // Empty (zero-length) range rejected — semantically a no-op and
        // usually indicates a caller bug. Tests below cover the positive
        // path.
        assert!(ByteRange::new(5, 5).is_err());
        assert!(ByteRange::new(0, 0).is_err());
    }

    #[test]
    fn http_header_roundtrip() {
        let cases: Vec<(RangeSpec, Option<String>)> = vec![
            (RangeSpec::all(), None),
            (
                RangeSpec::single(0, 100).unwrap(),
                Some("bytes=0-99".into()),
            ),
            (
                RangeSpec::Multi(vec![
                    ByteRange::new(0, 50).unwrap(),
                    ByteRange::new(100, 200).unwrap(),
                ]),
                Some("bytes=0-49,100-199".into()),
            ),
        ];
        for (spec, header) in cases {
            assert_eq!(spec.to_http_header(), header);
        }
    }

    #[test]
    fn parse_http_header_single() {
        let r = RangeSpec::from_http_header("bytes=0-99", 1000).unwrap();
        match r {
            RangeSpec::Single(b) => {
                assert_eq!(b.start, 0);
                assert_eq!(b.end, 100); // HTTP is inclusive → +1
            }
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn parse_http_header_multi() {
        let r = RangeSpec::from_http_header("bytes=0-49,200-299", 1000).unwrap();
        match r {
            RangeSpec::Multi(rs) => {
                assert_eq!(rs.len(), 2);
                assert_eq!(rs[0], ByteRange::new(0, 50).unwrap());
                assert_eq!(rs[1], ByteRange::new(200, 300).unwrap());
            }
            _ => panic!("expected multi"),
        }
    }

    #[test]
    fn parse_http_header_open_ended() {
        let r = RangeSpec::from_http_header("bytes=900-", 1000).unwrap();
        match r {
            RangeSpec::Single(b) => {
                assert_eq!(b.start, 900);
                assert_eq!(b.end, 1000);
            }
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn parse_http_header_suffix() {
        let r = RangeSpec::from_http_header("bytes=-100", 1000).unwrap();
        match r {
            RangeSpec::Single(b) => {
                assert_eq!(b.start, 900);
                assert_eq!(b.end, 1000);
            }
            _ => panic!("expected single"),
        }
    }

    /// Ranges that start past the end of the resource must be rejected
    /// by `from_http_header` — they are unsatisfiable per RFC 7233.
    #[test]
    fn parse_http_header_unsatisfiable_rejected() {
        // start >= size
        assert!(RangeSpec::from_http_header("bytes=1000-1099", 1000).is_err());
        // start > end (after clamping)
        assert!(RangeSpec::from_http_header("bytes=200-100", 1000).is_err());
    }

    /// `ByteRange::new` rejects inverted ranges and overflow values.
    #[test]
    fn byte_range_construction_safety() {
        assert!(ByteRange::new(10, 10).is_err(), "empty range must error");
        assert!(ByteRange::new(20, 10).is_err(), "inverted range must error");
        // u64::MAX as start would make end overflow
        assert!(ByteRange::new(u64::MAX, u64::MAX).is_err());
        // Normal cases work
        assert!(ByteRange::new(0, 1).is_ok());
        assert!(ByteRange::new(0, u64::MAX).is_ok());
    }
}
