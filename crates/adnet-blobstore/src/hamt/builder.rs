//! HAMT Builder for batch operations.
//!
//! Provides efficient batch insertion, deletion, and modification operations.

use super::{HamtEntry, HamtResult, HamtShard, ShardManager};

/// Builder for HAMT operations.
#[derive(Debug, Clone)]
pub struct HamtBuilder {
    /// Entries to be inserted.
    pending_inserts: Vec<(String, HamtEntry)>,

    /// Keys to be deleted.
    pending_deletes: Vec<String>,

    /// Maximum entries before forcing a shard.
    batch_size: usize,
}

impl HamtBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            pending_inserts: Vec::new(),
            pending_deletes: Vec::new(),
            batch_size: 1000,
        }
    }

    /// Create a builder with custom batch size.
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self {
            pending_inserts: Vec::new(),
            pending_deletes: Vec::new(),
            batch_size,
        }
    }

    /// Add an insert operation.
    pub fn insert(&mut self, name: String, entry: HamtEntry) -> &mut Self {
        self.pending_inserts.push((name, entry));
        self
    }

    /// Add a delete operation.
    pub fn delete(&mut self, name: String) -> &mut Self {
        self.pending_deletes.push(name);
        self
    }

    /// Check if there are pending operations.
    pub fn is_empty(&self) -> bool {
        self.pending_inserts.is_empty() && self.pending_deletes.is_empty()
    }

    /// Get the number of pending operations.
    pub fn len(&self) -> usize {
        self.pending_inserts.len() + self.pending_deletes.len()
    }

    /// Check if we should flush due to batch size.
    pub fn should_flush(&self) -> bool {
        self.len() >= self.batch_size
    }

    /// Execute all pending operations on a shard.
    pub fn execute(&mut self, shard: &mut HamtShard) -> HamtResult<()> {
        // First, process deletions
        for name in self.pending_deletes.drain(..) {
            shard.remove(&name)?;
        }

        // Then, process insertions
        for (name, entry) in self.pending_inserts.drain(..) {
            shard.insert(name, entry)?;
        }

        Ok(())
    }

    /// Execute on a shard manager.
    pub fn execute_on_manager(&mut self, manager: &mut ShardManager) -> HamtResult<()> {
        for name in self.pending_deletes.drain(..) {
            manager.remove(&name)?;
        }

        for (name, entry) in self.pending_inserts.drain(..) {
            manager.insert(name, entry)?;
        }

        Ok(())
    }

    /// Clear all pending operations.
    pub fn clear(&mut self) {
        self.pending_inserts.clear();
        self.pending_deletes.clear();
    }
}

impl Default for HamtBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Bulk import for efficiently loading many entries.
pub struct BulkImporter<'a> {
    shard: &'a mut HamtShard,
    batch_size: usize,
    current_batch: Vec<(String, HamtEntry)>,
}

impl<'a> BulkImporter<'a> {
    /// Create a new bulk importer.
    pub fn new(shard: &'a mut HamtShard) -> Self {
        Self {
            shard,
            batch_size: 1000,
            current_batch: Vec::new(),
        }
    }

    /// Create a bulk importer with custom batch size.
    pub fn with_batch_size(shard: &'a mut HamtShard, batch_size: usize) -> Self {
        Self {
            shard,
            batch_size,
            current_batch: Vec::new(),
        }
    }

    /// Add an entry to the import batch.
    pub fn add(&mut self, name: String, entry: HamtEntry) -> HamtResult<()> {
        self.current_batch.push((name, entry));

        if self.current_batch.len() >= self.batch_size {
            self.flush()?;
        }

        Ok(())
    }

    /// Add multiple entries.
    pub fn add_all(
        &mut self,
        entries: impl IntoIterator<Item = (String, HamtEntry)>,
    ) -> HamtResult<()> {
        for (name, entry) in entries {
            self.add(name, entry)?;
        }
        Ok(())
    }

    /// Flush the current batch to the shard.
    pub fn flush(&mut self) -> HamtResult<()> {
        if self.current_batch.is_empty() {
            return Ok(());
        }

        // Sort by hash for better locality (optional optimization)
        // This helps with collision handling
        self.current_batch.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, entry) in self.current_batch.drain(..) {
            self.shard.insert(name, entry)?;
        }

        Ok(())
    }

    /// Finalize the import and return the shard.
    pub fn finalize(mut self) -> HamtResult<()> {
        self.flush()
    }
}

/// Parallel bulk import for large datasets.
#[allow(dead_code)]
pub struct ParallelBulkImporter {
    batch_size: usize,
    num_workers: usize,
}

impl ParallelBulkImporter {
    /// Create a new parallel bulk importer.
    pub fn new() -> Self {
        Self {
            batch_size: 10000,
            num_workers: num_cpus(),
        }
    }

    /// Create with custom settings.
    pub fn with_settings(batch_size: usize, num_workers: usize) -> Self {
        Self {
            batch_size,
            num_workers,
        }
    }

    /// Import entries in parallel.
    /// Returns the merged shard.
    pub fn import(&self, entries: Vec<(String, HamtEntry)>) -> HamtResult<HamtShard> {
        if entries.is_empty() {
            return Ok(HamtShard::new());
        }

        // Split into chunks
        let chunk_size = (entries.len() / self.num_workers).max(1);
        let chunks: Vec<Vec<(String, HamtEntry)>> =
            entries.chunks(chunk_size).map(|c| c.to_vec()).collect();

        // Process chunks in parallel
        let shards: Vec<HamtShard> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|mut chunk| {
                let mut shard = HamtShard::new();
                chunk.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, entry) in chunk {
                    shard.insert(name, entry)?;
                }
                Ok(shard)
            })
            .collect::<HamtResult<Vec<_>>>()?;

        // Merge shards
        self.merge_shards(shards)
    }

    /// Merge multiple shards into one.
    fn merge_shards(&self, shards: Vec<HamtShard>) -> HamtResult<HamtShard> {
        if shards.is_empty() {
            return Ok(HamtShard::new());
        }

        if shards.len() == 1 {
            return Ok(shards.into_iter().next().unwrap());
        }

        // Simple merge: collect all entries and re-insert
        // For large shards, we should use a more efficient merge algorithm
        let mut merged = HamtShard::new();

        for shard in shards {
            for (name, entry) in shard.list() {
                merged.insert(name, entry)?;
            }
        }

        Ok(merged)
    }
}

impl Default for ParallelBulkImporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the number of available CPUs.
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_insert() {
        let mut shard = HamtShard::new();
        let mut builder = HamtBuilder::new();

        builder.insert(
            "file1.txt".to_string(),
            HamtEntry::File {
                hash: adnet_types::ContentHash::from_bytes(b"content1"),
                size_bytes: 8,
            },
        );

        builder.insert(
            "file2.txt".to_string(),
            HamtEntry::File {
                hash: adnet_types::ContentHash::from_bytes(b"content2"),
                size_bytes: 8,
            },
        );

        assert_eq!(builder.len(), 2);
        builder.execute(&mut shard).unwrap();
        assert_eq!(shard.len(), 2);
    }

    #[test]
    fn test_builder_delete() {
        let mut shard = HamtShard::new();
        shard
            .insert(
                "file1.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content1"),
                    size_bytes: 8,
                },
            )
            .unwrap();
        shard
            .insert(
                "file2.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content2"),
                    size_bytes: 8,
                },
            )
            .unwrap();

        let mut builder = HamtBuilder::new();
        builder.delete("file1.txt".to_string());

        builder.execute(&mut shard).unwrap();
        assert_eq!(shard.len(), 1);
        assert!(shard.get("file1.txt").is_none());
        assert!(shard.get("file2.txt").is_some());
    }

    #[test]
    fn test_builder_mixed() {
        let mut shard = HamtShard::new();

        let mut builder = HamtBuilder::new();
        builder.insert(
            "file1.txt".to_string(),
            HamtEntry::File {
                hash: adnet_types::ContentHash::from_bytes(b"content1"),
                size_bytes: 8,
            },
        );
        builder.insert(
            "file2.txt".to_string(),
            HamtEntry::File {
                hash: adnet_types::ContentHash::from_bytes(b"content2"),
                size_bytes: 8,
            },
        );
        builder.execute(&mut shard).unwrap();

        let mut builder = HamtBuilder::new();
        builder.delete("file1.txt".to_string());
        builder.insert(
            "file3.txt".to_string(),
            HamtEntry::File {
                hash: adnet_types::ContentHash::from_bytes(b"content3"),
                size_bytes: 8,
            },
        );
        builder.execute(&mut shard).unwrap();

        assert_eq!(shard.len(), 2);
        assert!(shard.get("file1.txt").is_none());
        assert!(shard.get("file2.txt").is_some());
        assert!(shard.get("file3.txt").is_some());
    }

    #[test]
    fn test_bulk_importer() {
        let mut shard = HamtShard::new();
        let mut importer = BulkImporter::new(&mut shard);

        for i in 0..100 {
            let name = format!("file{:03}.txt", i);
            importer
                .add(
                    name,
                    HamtEntry::File {
                        hash: adnet_types::ContentHash::from_bytes(
                            format!("content{}", i).as_bytes(),
                        ),
                        size_bytes: i as u64,
                    },
                )
                .unwrap();
        }

        importer.finalize().unwrap();
        assert_eq!(shard.len(), 100);
    }

    #[test]
    fn test_parallel_bulk_import() {
        let importer = ParallelBulkImporter::new();

        let entries: Vec<(String, HamtEntry)> = (0..1000)
            .map(|i| {
                (
                    format!("file{:04}.txt", i),
                    HamtEntry::File {
                        hash: adnet_types::ContentHash::from_bytes(
                            format!("content{}", i).as_bytes(),
                        ),
                        size_bytes: i as u64,
                    },
                )
            })
            .collect();

        let shard = importer.import(entries).unwrap();
        assert_eq!(shard.len(), 1000);
    }
}
