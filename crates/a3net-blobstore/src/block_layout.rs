//! Block layout — slice a blob into 256 KiB `blocks` whose
//! BLAKE3 hashes become the unit of replication.
//!
//! This is the network-level block size (IPFS parity: 256 KiB).
//! The on-disk store continues to use 16 KiB chunks — the
//! block layout is built on top of the chunk-only store so the
//! existing storage layer is untouched.

use a3net_types::ContentHash;

use crate::chunked::CHUNK_SIZE;

/// Network block size — 256 KiB. Same as IPFS UnixFS default.
pub const BLOCK_SIZE: usize = 256 * 1024;

/// 256 KiB / 16 KiB = 16 chunks per block on the storage layer.
pub const CHUNKS_PER_BLOCK: usize = BLOCK_SIZE / CHUNK_SIZE;

/// Compute the block hashes for a blob of `size` bytes.
///
/// This is the *planning* API — used by the replicator to know
/// how many blocks need replication without reading the data.
/// The actual block hashes are computed lazily by re-hashing
/// each `BLOCK_SIZE` slice of the blob in [`split_into_blocks`].
pub fn block_count_for(size: u64) -> u32 {
    if size == 0 {
        0
    } else {
        ((size - 1) / BLOCK_SIZE as u64 + 1) as u32
    }
}

/// Compute the list of block hashes for a blob of `size` bytes
/// WITHOUT reading the chunk data. Each block hash is
/// `ContentHash::from_bytes` of zero bytes of the appropriate
/// length — this is only valid for the *empty* block case.
/// Real submissions call [`split_into_blocks`] which reads
/// the chunks and re-hashes.
pub fn split_into_blocks_size(size: u64) -> Vec<ContentHash> {
    let count = block_count_for(size);
    (0..count)
        .map(|i| {
            let mut bytes = vec![0u8; BLOCK_SIZE];
            bytes[0..4].copy_from_slice(&i.to_le_bytes());
            ContentHash::from_bytes(&bytes)
        })
        .collect()
}

/// Compute the list of block hashes for an actual blob's bytes
/// by reading from the supplied chunked source. `chunks` is
/// a closure that returns the `i`-th chunk; the result is a
/// `Vec<ContentHash>` with one entry per 256 KiB block.
pub fn split_into_blocks_from_chunks<F>(n_chunks: u32, mut chunk: F) -> Vec<ContentHash>
where
    F: FnMut(u32) -> Vec<u8>,
{
    let mut out = Vec::new();
    if n_chunks == 0 {
        return out;
    }
    let n_blocks = (n_chunks as usize).div_ceil(CHUNKS_PER_BLOCK);
    out.reserve(n_blocks);
    for block_idx in 0..n_blocks {
        let start_chunk = block_idx * CHUNKS_PER_BLOCK;
        let end_chunk = (start_chunk + CHUNKS_PER_BLOCK).min(n_chunks as usize);
        let mut hasher = blake3::Hasher::new();
        for c in start_chunk..end_chunk {
            let bytes = chunk(c as u32);
            hasher.update(&bytes);
        }
        let digest = hasher.finalize();
        out.push(
            ContentHash::from_hex(digest.to_hex().as_ref()).expect("blake3 hex is always 64 chars"),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_count_for_zero() {
        assert_eq!(block_count_for(0), 0);
    }

    #[test]
    fn block_count_for_one_byte() {
        assert_eq!(block_count_for(1), 1);
    }

    #[test]
    fn block_count_for_exactly_one_block() {
        assert_eq!(block_count_for(BLOCK_SIZE as u64), 1);
    }

    #[test]
    fn block_count_for_two_blocks() {
        assert_eq!(block_count_for(BLOCK_SIZE as u64 + 1), 2);
    }

    #[test]
    fn block_count_for_16_blocks() {
        // 16 blocks × 256 KiB = 4 MiB.
        assert_eq!(block_count_for(4 * 1024 * 1024), 16);
    }

    #[test]
    fn split_into_blocks_from_chunks_empty() {
        let v = split_into_blocks_from_chunks(0, |_| panic!("no chunks"));
        assert!(v.is_empty());
    }

    #[test]
    fn split_into_blocks_from_chunks_single_block() {
        // 1 chunk → 1 block.
        let bytes = vec![0xAA; CHUNK_SIZE];
        let v = split_into_blocks_from_chunks(1, |_| bytes.clone());
        assert_eq!(v.len(), 1);
        let expected = ContentHash::from_bytes(&bytes);
        assert_eq!(v[0], expected);
    }

    #[test]
    fn split_into_blocks_from_chunks_full_block() {
        // 16 chunks → 1 block (every block is 256 KiB = 16 chunks).
        let n = CHUNKS_PER_BLOCK as u32;
        let v = split_into_blocks_from_chunks(n, |_| vec![0xBB; CHUNK_SIZE]);
        assert_eq!(v.len(), 1);
        let mut hasher = blake3::Hasher::new();
        for _ in 0..n {
            hasher.update(&vec![0xBB; CHUNK_SIZE]);
        }
        let expected = ContentHash::from_hex(hasher.finalize().to_hex().as_ref()).unwrap();
        assert_eq!(v[0], expected);
    }

    #[test]
    fn split_into_blocks_from_chunks_partial_block() {
        // 17 chunks → 2 blocks: first full (16 chunks), second
        // partial (1 chunk).
        let v = split_into_blocks_from_chunks(17, |i| vec![i as u8; CHUNK_SIZE]);
        assert_eq!(v.len(), 2);
        let mut hasher = blake3::Hasher::new();
        for i in 0..16 {
            hasher.update(&vec![i as u8; CHUNK_SIZE]);
        }
        let expected1 = ContentHash::from_hex(hasher.finalize().to_hex().as_ref()).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&vec![16u8; CHUNK_SIZE]);
        let expected2 = ContentHash::from_hex(hasher.finalize().to_hex().as_ref()).unwrap();
        assert_eq!(v[0], expected1);
        assert_eq!(v[1], expected2);
    }
}
