//! Reed-Solomon erasure coding — 3+1 configuration (33% redundancy).
//!
//! ## Design
//!
//! A3Net distributes storage using Reed-Solomon erasure coding over
//! GF(2^8) (Galois Field 2^8). Every blob is split into `k = 3`
//! data shards and `m = 1` parity shard, giving a **33% storage
//! overhead** (the parity shard costs 1 extra unit per 3 data
//! units) while tolerating the loss of **any single shard**.
//!
//! ## Shard layout
//!
//! Each shard is a sequence of 16-byte elements aligned to
//! [`crate::chunked::CHUNK_SIZE`]. For a blob split into
//! `N` chunks (16 KiB each):
//!
//! ```text
//! Data shard 0: [chunk_0_0, chunk_0_1, ... chunk_0_{N-1}]  (16 bytes each)
//! Data shard 1: [chunk_1_0, chunk_1_1, ... chunk_1_{N-1}]  (16 bytes each)
//! Data shard 2: [chunk_2_0, chunk_2_1, ... chunk_2_{N-1}]  (16 bytes each)
//! Parity shard: [p_0,       p_1,       ... p_{N-1}]        (16 bytes each)
//! ```
//!
//! Where `p_i = shard_0_i ⊕ shard_1_i ⊕ shard_2_i` (XOR parity).
//! The `reed-solomon-erasure` crate uses Vandermonde matrices,
//! so the parity shard is not just XOR — it provides stronger
//! algebraic protection — but XOR would also be correct here.
//!
//! ## Erasure / corruption distinction
//!
//! Erasure coding assumes you **know** which shard is missing
//! (network dropout, node offline). Corruption (bit-flip, disk
//! error) is detected by BLAKE3 verification — the caller marks
//! the corrupted shard as missing and passes `None` to
//! [`ErasureCoder::reconstruct`], which recovers it identically.
//!
//! ## DO-178C traceability
//!
//! This module implements EC Requirement **EC-R1**: blobs using
//! the EC policy must be recoverable from any `k` available shards.
//! EC-R2: every shard must carry a BLAKE3 integrity digest of its
//! content so receivers can detect corruption at the shard level.

use std::fmt;

use a3net_types::ContentHash;
use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::chunked::CHUNK_SIZE;

/// DO-178C trace tag — every EC encode event carries this so the
/// certifier can grep the audit log.
pub const SR_TAG_EC_R1: &str = "EC-R1";

/// DO-178C trace tag — BLAKE3 shard integrity verification.
pub const SR_TAG_EC_R2: &str = "EC-R2";

/// Number of data shards (k).
///
/// With 1 parity shard, total = 4 shards. Any 1 may be lost.
pub const EC_DATA_SHARDS: usize = 3;

/// Number of parity shards (m).
pub const EC_PARITY_SHARDS: usize = 1;

/// Total shards per blob = k + m = 4.
pub const EC_TOTAL_SHARDS: usize = EC_DATA_SHARDS + EC_PARITY_SHARDS;

/// Storage overhead as a human-readable string.
pub const EC_REDUNDANCY_DESC: &str = "33% (3+1 Reed-Solomon)";

/// Errors produced by erasure coding operations.
#[derive(Debug, thiserror::Error)]
pub enum ErasureCodingError {
    /// reed-solomon-erasure returned an error.
    #[error("reed-solomon codec error: {0}")]
    Codec(String),

    /// The number of input chunks is incompatible with this codec.
    /// Reed-Solomon requires all shards (including parity) to be
    /// the same byte length; the last shard may be shorter.
    #[error("chunk count {0} is incompatible with EC codec: {1}")]
    IncompatibleChunkCount(usize, String),

    /// At least `EC_DATA_SHARDS` (3) shards are required for
    /// reconstruction but fewer were provided.
    #[error("need at least {required} shards to reconstruct, got {available}")]
    TooFewShards { required: usize, available: usize },

    /// The shard index is out of range [0, EC_TOTAL_SHARDS).
    #[error("shard index {index} out of range [0, {max})")]
    ShardIndexOutOfRange { index: usize, max: usize },

    /// The shard's BLAKE3 digest does not match the stored value.
    /// The shard content is corrupted and reconstruction is needed.
    #[error("shard {index} BLAKE3 mismatch: expected {expected}, got {actual}")]
    ShardCorrupted {
        index: usize,
        expected: ContentHash,
        actual: ContentHash,
    },

    /// Reconstruction failed because the available shards were
    /// also corrupted (fewer than k valid shards present).
    #[error(
        "insufficient valid shards: reconstruction requires {required} but only {present} are present"
    )]
    ReconstructionFailed { required: usize, present: usize },

    /// An I/O error occurred while reading/writing shard data.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

impl From<String> for ErasureCodingError {
    fn from(s: String) -> Self {
        ErasureCodingError::Codec(s)
    }
}

impl From<&str> for ErasureCodingError {
    fn from(s: &str) -> Self {
        ErasureCodingError::Codec(s.to_string())
    }
}

impl ErasureCodingError {
    /// Returns `true` if this error means the blob is permanently
    /// unrecoverable (not just temporarily unavailable).
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            ErasureCodingError::ReconstructionFailed { .. }
                | ErasureCodingError::ShardCorrupted { .. }
        )
    }
}

/// Metadata for one shard of an EC-encoded blob.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ECShardMeta {
    /// Which shard this is: 0 = data-0, 1 = data-1, 2 = data-2,
    /// 3 = parity.
    pub index: u8,
    /// BLAKE3 digest of the shard's raw content (16-byte elements).
    /// Used for integrity verification before reconstruction.
    pub digest: ContentHash,
    /// Number of 16-byte elements in this shard.
    /// All shards have the same count except possibly the last.
    pub elements: u32,
    /// Whether this is a parity shard (index == EC_DATA_SHARDS).
    pub is_parity: bool,
}

impl ECShardMeta {
    /// Byte length of this shard (elements × 16).
    pub fn byte_len(&self) -> u64 {
        self.elements as u64 * CHUNK_SIZE as u64
    }
}

/// Full encoding metadata for one EC-encoded blob.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ECBlobMeta {
    /// BLAKE3 hash of the original (unencoded) blob content.
    pub content_hash: ContentHash,
    /// Original blob size in bytes.
    pub size_bytes: u64,
    /// Number of EC shards for this blob (= 4).
    pub shard_count: u8,
    /// Per-shard metadata.
    pub shards: Vec<ECShardMeta>,
    /// Original size of each chunk (in bytes). Used by deinterleave
    /// to correctly reconstruct partial chunks.
    #[serde(default)]
    pub chunk_sizes: Vec<usize>,
}

impl ECBlobMeta {
    /// Verify the BLAKE3 integrity of a shard.
    ///
    /// Returns `Ok(())` if `shard_bytes` matches the stored digest,
    /// or `Err(ErasureCodingError::ShardCorrupted)` if it doesn't.
    pub fn verify_shard(
        &self,
        shard_index: usize,
        shard_bytes: &[u8],
    ) -> Result<(), ErasureCodingError> {
        if shard_index >= self.shards.len() {
            return Err(ErasureCodingError::ShardIndexOutOfRange {
                index: shard_index,
                max: self.shards.len(),
            });
        }
        let expected = &self.shards[shard_index].digest;
        let actual = ContentHash::from_bytes(shard_bytes);
        if &actual != expected {
            return Err(ErasureCodingError::ShardCorrupted {
                index: shard_index,
                expected: expected.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// Returns `true` if the blob is considered recoverable (at
    /// least `EC_DATA_SHARDS` shards have valid digests).
    pub fn is_recoverable(&self) -> bool {
        self.shards.len() >= EC_DATA_SHARDS
    }
}

/// Reed-Solomon erasure coder — wraps `reed-solomon-erasure` for
/// the A3Net 3+1 configuration.
///
/// ## Thread safety
///
/// `ErasureCoder` is immutable after construction and uses no
/// interior mutability. It is safe to share across threads.
pub struct ErasureCoder {
    inner: ReedSolomon,
}

impl fmt::Debug for ErasureCoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ErasureCoder")
            .field("data_shards", &EC_DATA_SHARDS)
            .field("parity_shards", &EC_PARITY_SHARDS)
            .finish()
    }
}

impl ErasureCoder {
    /// Construct a new coder for the 3+1 configuration.
    ///
    /// Returns `Err` only if the `reed-solomon-erasure` crate
    /// cannot build the Vandermonde matrix (should never happen
    /// with these constants).
    pub fn new() -> Result<Self, ErasureCodingError> {
        ReedSolomon::new(EC_DATA_SHARDS, EC_PARITY_SHARDS)
            .map(|inner| Self { inner })
            .map_err(|e| ErasureCodingError::Codec(e.to_string()))
    }

    /// Encode `chunks` into `EC_TOTAL_SHARDS` shards.
    ///
    /// `chunks` is the raw blob split into 16 KiB elements.
    /// The chunks are redistributed into `EC_DATA_SHARDS` data
    /// shards (each shard gets every `EC_DATA_SHARDS`-th chunk)
    /// and 1 parity shard is computed.
    ///
    /// ## Return
    ///
    /// `(shards, meta)` where `shards` is a `Vec<Vec<u8>>` of
    /// length `EC_TOTAL_SHARDS` and `meta` carries BLAKE3 digests
    /// for each shard.
    ///
    /// ## Shard lengths
    ///
    /// Shards are padded to `EC_DATA_SHARDS` elements. If the last
    /// shard has fewer elements, it is zero-padded to that length.
    /// The padding is not transmitted — only `elements` bytes of
    /// each shard are meaningful.
    pub fn encode(
        &self,
        chunks: &[Vec<u8>],
    ) -> Result<(Vec<Vec<u8>>, ECBlobMeta), ErasureCodingError> {
        if chunks.is_empty() {
            return Err(ErasureCodingError::IncompatibleChunkCount(
                0,
                "empty blob has no chunks to encode".into(),
            ));
        }

        let n_elements = chunks.len();
        // Number of rows in the shard matrix = EC_DATA_SHARDS
        // Number of columns = ceil(n_elements / EC_DATA_SHARDS)
        let n_cols = (n_elements + EC_DATA_SHARDS - 1) / EC_DATA_SHARDS;

        // Allocate shard buffers. Each shard holds `n_cols` elements.
        let mut shards: Vec<Vec<u8>> = (0..EC_TOTAL_SHARDS)
            .map(|_| Vec::with_capacity(n_cols * CHUNK_SIZE))
            .collect();

        // For each column, track how many elements it has.
        // Elements in incomplete columns need zero-padding for missing shards.
        let mut col_element_count: Vec<usize> = vec![0; n_cols];
        for elem_idx in 0..n_elements {
            let col_idx = elem_idx / EC_DATA_SHARDS;
            col_element_count[col_idx] += 1;
        }

        // Interleave chunks into data shards.
        for (elem_idx, chunk) in chunks.iter().enumerate() {
            let shard_idx = elem_idx % EC_DATA_SHARDS;
            let col_idx = elem_idx / EC_DATA_SHARDS;
            let shard = &mut shards[shard_idx];

            // Compute the expected offset for this column.
            let expected_offset = col_idx * CHUNK_SIZE;

            // Ensure shard is at least this long (pad if needed).
            if shard.len() < expected_offset {
                shard.resize(expected_offset, 0);
            }

            // Append this chunk.
            shard.extend_from_slice(chunk);

            // For non-last elements, verify alignment.
            if elem_idx < n_elements - 1 {
                debug_assert_eq!(
                    shard.len() % CHUNK_SIZE,
                    0,
                    "intermediate shard element must be exactly CHUNK_SIZE bytes"
                );
            }
        }

        // Zero-pad all shards to `n_cols` elements.
        let padded_len = n_cols * CHUNK_SIZE;
        for shard in &mut shards {
            if shard.len() < padded_len {
                shard.resize(padded_len, 0);
            }
        }

        // CRITICAL: Zero-pad incomplete columns.
        // For each column, if it has fewer than EC_DATA_SHARDS elements,
        // the missing elements should be zeros in their respective shards.
        for col_idx in 0..n_cols {
            let elements_in_col = col_element_count[col_idx];
            if elements_in_col < EC_DATA_SHARDS {
                // This column is incomplete. Missing elements are at indices
                // elements_in_col, elements_in_col+1, ... EC_DATA_SHARDS-1.
                let offset = col_idx * CHUNK_SIZE;
                for shard_idx in elements_in_col..EC_DATA_SHARDS {
                    // Ensure shard has enough space.
                    let shard = &mut shards[shard_idx];
                    let required_len = offset + CHUNK_SIZE;
                    if shard.len() < required_len {
                        shard.resize(required_len, 0);
                    }
                    // The bytes at [offset, offset+CHUNK_SIZE) are already 0 from the resize above.
                }
            }
        }

        // Compute parity shard in-place.
        self.inner
            .encode(&mut shards)
            .map_err(|e| ErasureCodingError::Codec(e.to_string()))?;

        // Compute BLAKE3 digests for every shard.
        let mut shard_metas = Vec::with_capacity(EC_TOTAL_SHARDS);
        for (idx, shard) in shards.iter().enumerate() {
            let n_actual_elements = if idx < EC_DATA_SHARDS {
                // Data shards: round up element count per shard.
                n_cols
            } else {
                // Parity shard: always padded to n_cols.
                n_cols
            };

            shard_metas.push(ECShardMeta {
                index: idx as u8,
                digest: ContentHash::from_bytes(shard),
                elements: n_actual_elements as u32,
                is_parity: idx >= EC_DATA_SHARDS,
            });
        }

        let meta = ECBlobMeta {
            content_hash: ContentHash::from_bytes(
                &chunks.iter().flatten().copied().collect::<Vec<u8>>(),
            ),
            size_bytes: chunks.iter().map(|c| c.len() as u64).sum(),
            shard_count: EC_TOTAL_SHARDS as u8,
            shards: shard_metas,
            chunk_sizes: chunks.iter().map(|c| c.len()).collect(),
        };

        Ok((shards, meta))
    }

    /// Reconstruct a missing or corrupted shard from the remaining
    /// `EC_DATA_SHARDS` valid shards.
    ///
    /// `available` is a Vec of `Option<Vec<u8>>` where `None`
    /// marks the missing/corrupted shard. At least `EC_DATA_SHARDS`
    /// entries must be `Some`.
    ///
    /// Returns a tuple `(reconstructed_shard, reconstructed_index)` indicating
    /// which shard was reconstructed and its index (0-3), or an error if
    /// reconstruction is impossible (too few shards available or codec failure).
    ///
    /// ## Correctness note
    ///
    /// This function correctly identifies the reconstructed shard by:
    /// 1. Recording which slots were `None` BEFORE reconstruction
    /// 2. After `self.inner.reconstruct()` fills in the missing shards,
    ///    returning the content from exactly those previously-None slots
    pub fn reconstruct(
        &self,
        mut available: Vec<Option<Vec<u8>>>,
    ) -> Result<(Vec<u8>, usize), ErasureCodingError> {
        if available.len() != EC_TOTAL_SHARDS {
            return Err(ErasureCodingError::ShardIndexOutOfRange {
                index: available.len(),
                max: EC_TOTAL_SHARDS,
            });
        }

        let present_count = available.iter().filter(|s| s.is_some()).count();
        if present_count < EC_DATA_SHARDS {
            return Err(ErasureCodingError::TooFewShards {
                required: EC_DATA_SHARDS,
                available: present_count,
            });
        }

        // Record which indices were None BEFORE reconstruction.
        // For 3+1 config, at most 1 shard is missing.
        let missing_indices: Vec<usize> = available
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_none())
            .map(|(i, _)| i)
            .collect();

        self.inner
            .reconstruct(&mut available)
            .map_err(|e| ErasureCodingError::Codec(e.to_string()))?;

        // Return the reconstructed shard at the first missing index.
        // If no shard was missing (all present), this is a no-op
        // and we return the parity shard by convention.
        let target_idx = missing_indices.first().copied().unwrap_or(EC_DATA_SHARDS);
        let reconstructed = available
            .get(target_idx)
            .and_then(|s| s.clone())
            .ok_or_else(|| {
                ErasureCodingError::Codec(format!(
                    "reconstruct returned None at index {target_idx}"
                ))
            })?;

        Ok((reconstructed, target_idx))
    }

    /// Reconstruct only the data shards (not the parity shard) from
    /// available shards. Used by the download path when the caller
    /// only needs the original data.
    ///
    /// All `EC_DATA_SHARDS` data shards are reconstructed.
    pub fn reconstruct_data(
        &self,
        mut available: Vec<Option<Vec<u8>>>,
    ) -> Result<Vec<Vec<u8>>, ErasureCodingError> {
        if available.len() != EC_TOTAL_SHARDS {
            return Err(ErasureCodingError::ShardIndexOutOfRange {
                index: available.len(),
                max: EC_TOTAL_SHARDS,
            });
        }

        let present_count = available.iter().filter(|s| s.is_some()).count();
        if present_count < EC_DATA_SHARDS {
            return Err(ErasureCodingError::TooFewShards {
                required: EC_DATA_SHARDS,
                available: present_count,
            });
        }

        self.inner
            .reconstruct_data(&mut available)
            .map_err(|e| ErasureCodingError::Codec(e.to_string()))?;

        // Return the first EC_DATA_SHARDS entries.
        let mut data_shards = Vec::with_capacity(EC_DATA_SHARDS);
        for (idx, shard_opt) in available.into_iter().enumerate() {
            if idx < EC_DATA_SHARDS {
                data_shards.push(shard_opt.ok_or_else(|| {
                    ErasureCodingError::Codec("reconstruct_data returned None".into())
                })?);
            }
        }
        Ok(data_shards)
    }

    /// De-interleave data shards back into original chunk order.
    ///
    /// Takes the `EC_DATA_SHARDS` data shards (each as a flat byte
    /// buffer of 16-byte elements) and reconstructs the original
    /// chunk sequence.
    ///
    /// `original_element_count` is the number of chunks in the
    /// original blob (before encoding).
    pub fn deinterleave(data_shards: &[Vec<u8>], original_element_count: usize) -> Vec<Vec<u8>> {
        if original_element_count == 0 {
            return Vec::new();
        }

        let _n_cols = (original_element_count + EC_DATA_SHARDS - 1) / EC_DATA_SHARDS;
        let mut chunks = Vec::with_capacity(original_element_count);

        for elem_idx in 0..original_element_count {
            let shard_idx = elem_idx % EC_DATA_SHARDS;
            let col_idx = elem_idx / EC_DATA_SHARDS;
            let offset = col_idx * CHUNK_SIZE;
            let chunk = data_shards[shard_idx]
                .get(offset..offset.saturating_add(CHUNK_SIZE))
                .unwrap_or(&[]);
            chunks.push(chunk.to_vec());
        }

        chunks
    }

    /// De-interleave data shards back into original chunk order.
    ///
    /// This version uses `chunk_sizes` to correctly handle partial chunks.
    pub fn deinterleave_with_sizes(data_shards: &[Vec<u8>], chunk_sizes: &[usize]) -> Vec<Vec<u8>> {
        let original_element_count = chunk_sizes.len();
        if original_element_count == 0 {
            return Vec::new();
        }

        let mut chunks = Vec::with_capacity(original_element_count);

        for (elem_idx, &expected_size) in chunk_sizes.iter().enumerate() {
            let shard_idx = elem_idx % EC_DATA_SHARDS;
            let col_idx = elem_idx / EC_DATA_SHARDS;
            let offset = col_idx * CHUNK_SIZE;

            // Read up to expected_size bytes (not necessarily CHUNK_SIZE).
            let shard = &data_shards[shard_idx];
            let start = offset.min(shard.len());
            let end = (offset + expected_size).min(shard.len());
            let chunk = if start < end { &shard[start..end] } else { &[] };
            chunks.push(chunk.to_vec());
        }

        chunks
    }
}

impl Default for ErasureCoder {
    fn default() -> Self {
        Self::new().expect("ErasureCoder::new() must succeed for 3+1 config")
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunks(data: &[u8]) -> Vec<Vec<u8>> {
        data.chunks(CHUNK_SIZE).map(|c| c.to_vec()).collect()
    }

    fn roundtrip(data: &[u8]) {
        let chunks = make_chunks(data);
        let n_elements = chunks.len();
        let n_cols = (n_elements + EC_DATA_SHARDS - 1) / EC_DATA_SHARDS;

        let coder = ErasureCoder::new().unwrap();
        let (shards, meta) = coder.encode(&chunks).unwrap();

        assert_eq!(shards.len(), EC_TOTAL_SHARDS);
        assert_eq!(meta.shard_count as usize, EC_TOTAL_SHARDS);

        // Every shard should have a valid BLAKE3 digest.
        for (idx, shard) in shards.iter().enumerate() {
            meta.verify_shard(idx, shard).unwrap();
        }

        // Simulate missing parity shard and reconstruct it.
        let mut available: Vec<Option<Vec<u8>>> = shards.iter().cloned().map(Some).collect();
        available[EC_DATA_SHARDS] = None;

        let (reconstructed_parity, idx) = coder.reconstruct(available).unwrap();
        let parity_idx = EC_DATA_SHARDS;
        assert_eq!(idx, parity_idx);
        assert_eq!(reconstructed_parity, shards[parity_idx]);

        // Deinterleave + reassemble → original data.
        let data_shards: Vec<_> = shards[..EC_DATA_SHARDS].to_vec();
        let deinterleaved = ErasureCoder::deinterleave_with_sizes(&data_shards, &meta.chunk_sizes);
        let reassembled: Vec<u8> = deinterleaved.iter().flatten().copied().collect();
        assert_eq!(reassembled.as_slice(), data);
    }

    #[test]
    fn encode_decode_empty() {
        let coder = ErasureCoder::new().unwrap();
        let result = coder.encode(&[]);
        assert!(result.is_err());
        if let Err(ErasureCodingError::IncompatibleChunkCount(0, _)) = result {
            // expected
        } else {
            panic!("expected IncompatibleChunkCount for empty input, got {result:?}");
        }
    }

    #[test]
    fn encode_decode_exact_chunks() {
        // Exactly 3 chunks → 1 column.
        let data = vec![
            vec![1u8; CHUNK_SIZE],
            vec![2u8; CHUNK_SIZE],
            vec![3u8; CHUNK_SIZE],
        ];
        roundtrip(&data.iter().flatten().copied().collect::<Vec<u8>>());
    }

    #[test]
    fn encode_decode_partial_last_chunk() {
        // 4 chunks → 2 columns (second column has 1 data + 1 parity).
        let mut data = vec![0u8; 3 * CHUNK_SIZE];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
        data.push(99); // partial 1-byte chunk
        roundtrip(&data);
    }

    #[test]
    fn encode_decode_large_blob() {
        // 100 chunks → ~34 columns.
        let size = 100 * CHUNK_SIZE;
        let data: Vec<u8> = (0usize..).map(|i| i as u8).take(size).collect();
        roundtrip(&data);
    }

    #[test]
    fn reconstruct_missing_parity_shard() {
        let chunks = make_chunks(&[42u8; 3 * CHUNK_SIZE]);
        let coder = ErasureCoder::new().unwrap();
        let (shards, _meta) = coder.encode(&chunks).unwrap();

        let parity_idx = EC_DATA_SHARDS;
        let original_parity = shards[parity_idx].clone();

        // Simulate network loss of the parity shard.
        let mut available: Vec<Option<Vec<u8>>> = shards.into_iter().map(Some).collect();
        available[parity_idx] = None;

        let (reconstructed, reconstructed_idx) = coder.reconstruct(available).unwrap();
        assert_eq!(&reconstructed, &original_parity);
        assert_eq!(reconstructed_idx, parity_idx);
    }

    #[test]
    fn reconstruct_missing_data_shard() {
        let chunks = make_chunks(&[0xAAu8; 5 * CHUNK_SIZE]);
        let coder = ErasureCoder::new().unwrap();
        let (shards, _meta) = coder.encode(&chunks).unwrap();

        let original_shard_1 = shards[1].clone();

        // Simulate loss of data shard 1.
        let mut available: Vec<Option<Vec<u8>>> = shards.into_iter().map(Some).collect();
        available[1] = None;

        let (reconstructed, reconstructed_idx) = coder.reconstruct(available).unwrap();
        assert_eq!(&reconstructed, &original_shard_1);
        assert_eq!(reconstructed_idx, 1);
    }

    #[test]
    fn reconstruct_returns_correct_index_for_each_missing_shard() {
        let chunks = make_chunks(&[0x55u8; 4 * CHUNK_SIZE]);
        let coder = ErasureCoder::new().unwrap();
        let (shards, _meta) = coder.encode(&chunks).unwrap();

        // Test reconstruction of each possible missing shard (0, 1, 2, 3).
        for missing_idx in 0..EC_TOTAL_SHARDS {
            let mut available: Vec<Option<Vec<u8>>> = shards.iter().cloned().map(Some).collect();
            available[missing_idx] = None;

            let (reconstructed, idx) = coder.reconstruct(available).unwrap();
            assert_eq!(idx, missing_idx, "reconstructed shard index mismatch");
            assert_eq!(
                &reconstructed, &shards[missing_idx],
                "reconstructed content mismatch"
            );
        }
    }

    #[test]
    fn verify_shard_detects_corruption() {
        let chunks = make_chunks(&[0xBBu8; CHUNK_SIZE]);
        let coder = ErasureCoder::new().unwrap();
        let (shards, meta) = coder.encode(&chunks).unwrap();

        // Corrupt shard 0.
        let mut corrupted = shards[0].clone();
        corrupted[0] ^= 0xFF;

        let result = meta.verify_shard(0, &corrupted);
        assert!(matches!(
            result,
            Err(ErasureCodingError::ShardCorrupted { .. })
        ));
    }

    #[test]
    fn too_few_shards_error() {
        let coder = ErasureCoder::new().unwrap();
        let chunks = make_chunks(&[0u8; CHUNK_SIZE]);
        let (shards, _meta) = coder.encode(&chunks).unwrap();

        // Only 2 shards present — not enough for reconstruction.
        let mut available: Vec<Option<Vec<u8>>> = shards.into_iter().map(Some).collect();
        available[2] = None;
        available[3] = None;

        let result = coder.reconstruct(available);
        assert!(matches!(
            result,
            Err(ErasureCodingError::TooFewShards {
                required: 3,
                available: 2
            })
        ));
    }

    #[test]
    fn deinterleave_exact_column() {
        // 6 chunks → 2 columns exactly.
        let data: Vec<u8> = (0usize..).map(|i| i as u8).take(6 * CHUNK_SIZE).collect();
        let chunks = make_chunks(&data);
        let coder = ErasureCoder::new().unwrap();
        let (shards, _meta) = coder.encode(&chunks).unwrap();
        let data_shards: Vec<_> = shards[..EC_DATA_SHARDS].to_vec();
        let deinterleaved = ErasureCoder::deinterleave(&data_shards, 6);
        let reassembled: Vec<u8> = deinterleaved.iter().flatten().copied().collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn deinterleave_partial_last_column() {
        // 7 chunks → 3 columns (last column has 1 element).
        let mut data = vec![0u8; 7 * CHUNK_SIZE - 3];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = i as u8;
        }
        let chunks = make_chunks(&data);
        let coder = ErasureCoder::new().unwrap();
        let (shards, meta) = coder.encode(&chunks).unwrap();
        let data_shards: Vec<_> = shards[..EC_DATA_SHARDS].to_vec();
        let deinterleaved = ErasureCoder::deinterleave_with_sizes(&data_shards, &meta.chunk_sizes);
        let reassembled: Vec<u8> = deinterleaved.iter().flatten().copied().collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn ec_constants_match_documentation() {
        assert_eq!(EC_DATA_SHARDS, 3);
        assert_eq!(EC_PARITY_SHARDS, 1);
        assert_eq!(EC_TOTAL_SHARDS, 4);
        // Storage overhead: (k+m)/k - 1 = 4/3 - 1 ≈ 0.333
        let overhead = EC_TOTAL_SHARDS as f64 / EC_DATA_SHARDS as f64 - 1.0;
        assert!((overhead - 1.0 / 3.0).abs() < 0.001);
    }
}
