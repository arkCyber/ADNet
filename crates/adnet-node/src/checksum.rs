//! Per-chunk + whole-blob checksum reports.
//!
//! Mirrors `ChecksumReport` from
//! `Exodus@src-backup/.../file_transfer_engine.rs`. The struct is plain data
//! so callers (UI, CLI, background jobs) can serialise it as JSON without
//! dragging in any IO types.

use serde::{Deserialize, Serialize};

/// One chunk's checksum entry inside a [`ChecksumReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkChecksumEntry {
    pub index: u32,
    pub hash: String,
    pub size_bytes: u64,
}

/// Aggregate report written after a transfer completes (or fails).
///
/// `algorithm` is currently always `"blake3"`; the field exists so we can
/// migrate to a different hash family without breaking serialised reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumReport {
    pub algorithm: String,
    pub file_hash: String,
    pub file_size: u64,
    pub chunk_count: u32,
    pub chunks: Vec<ChunkChecksumEntry>,
    pub destination_verified: bool,
    pub verified_at: u64,
    pub mismatch_chunks: Vec<u32>,
}

impl ChecksumReport {
    /// Build a report from raw chunk hashes + the expected file hash.
    ///
    /// `mismatch_chunks` is the list of chunk indexes whose per-chunk hash
    /// disagrees with the *chunk's own* expected hash (a value the caller
    /// computed out-of-band). It is **not** a comparison against the file
    /// hash — every per-chunk hash should match the file hash *only when the
    /// blob is a single chunk*. For multi-chunk blobs the caller passes the
    /// per-chunk expectations via `chunk_hashes_expected: Option<&[String]>`
    /// so the report can detect per-chunk corruption.
    pub fn build(
        algorithm: impl Into<String>,
        file_hash: impl Into<String>,
        file_size: u64,
        chunk_count: u32,
        chunks: Vec<ChunkChecksumEntry>,
        computed_file_hash: &str,
        verified_at_secs: u64,
    ) -> Self {
        Self::build_with_chunk_expectations(
            algorithm,
            file_hash,
            file_size,
            chunk_count,
            chunks,
            None,
            computed_file_hash,
            verified_at_secs,
        )
    }

    /// Same as [`build`](Self::build) but the caller can supply a parallel
    /// `expected_chunk_hashes` slice; mismatches at the same index are
    /// recorded in `mismatch_chunks`.
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_chunk_expectations(
        algorithm: impl Into<String>,
        file_hash: impl Into<String>,
        file_size: u64,
        chunk_count: u32,
        chunks: Vec<ChunkChecksumEntry>,
        expected_chunk_hashes: Option<&[String]>,
        computed_file_hash: &str,
        verified_at_secs: u64,
    ) -> Self {
        let file_hash_str = file_hash.into();
        let algorithm_str = algorithm.into();
        let mismatch: Vec<u32> = match expected_chunk_hashes {
            Some(expected) => chunks
                .iter()
                .zip(expected.iter())
                .filter(|(c, e)| !c.hash.eq_ignore_ascii_case(e))
                .map(|(c, _)| c.index)
                .collect(),
            None => Vec::new(),
        };
        let destination_verified = mismatch.is_empty() && file_hash_str == computed_file_hash;
        Self {
            algorithm: algorithm_str,
            file_hash: file_hash_str,
            file_size,
            chunk_count,
            chunks,
            destination_verified,
            verified_at: verified_at_secs,
            mismatch_chunks: mismatch,
        }
    }

    /// True if every chunk + the final file hash agreed.
    pub fn is_clean(&self) -> bool {
        self.destination_verified && self.mismatch_chunks.is_empty()
    }
}

/// Resume checkpoint for an interrupted download — survives restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeState {
    pub completed_chunks: Vec<u32>,
    pub bytes_done: u64,
    pub last_peer_attempt: Option<String>,
}

impl ResumeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `index` as completed and bump `bytes_done` by `len`.
    ///
    /// Re-marking the same chunk is a no-op (does not double-count bytes).
    pub fn mark_completed(&mut self, index: u32, len: u64) {
        if self.completed_chunks.contains(&index) {
            return;
        }
        self.completed_chunks.push(index);
        self.bytes_done = self.bytes_done.saturating_add(len);
    }

    /// Number of chunks left to fetch.
    pub fn remaining(&self, total: u32) -> u32 {
        total.saturating_sub(self.completed_chunks.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(i: u32) -> ChunkChecksumEntry {
        ChunkChecksumEntry {
            index: i,
            hash: format!("h{i}"),
            size_bytes: 1024,
        }
    }

    #[test]
    fn clean_report_is_clean() {
        let r = ChecksumReport::build(
            "blake3",
            "deadbeef",
            4096,
            4,
            vec![entry(0), entry(1), entry(2), entry(3)],
            "deadbeef",
            0,
        );
        assert!(r.is_clean());
        assert!(r.mismatch_chunks.is_empty());
    }

    #[test]
    fn resume_state_marks_progress() {
        let mut r = ResumeState::new();
        r.mark_completed(0, 16 * 1024);
        r.mark_completed(2, 16 * 1024);
        assert_eq!(r.completed_chunks, vec![0, 2]);
        assert_eq!(r.bytes_done, 32 * 1024);
        // Marking the same chunk twice must not double-count bytes.
        r.mark_completed(0, 16 * 1024);
        assert_eq!(r.bytes_done, 32 * 1024);
        assert_eq!(r.remaining(4), 2);
    }
}
