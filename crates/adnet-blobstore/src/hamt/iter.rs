//! HAMT Iterator for traversing entries.
//!
//! Provides various iterator types for traversing HAMT directory structures.

use super::{HamtEntry, HamtLink, HamtShard};

/// Iterator over all entries in a HAMT shard.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HamtIter<'a> {
    /// Reference to the shard.
    shard: &'a HamtShard,
    /// Current position in links.
    current_link: usize,
    /// Current position within a bucket.
    current_bucket_pos: usize,
    /// Stack for iterative traversal.
    stack: Vec<StackFrame>,
}

/// Stack frame for traversal.
#[derive(Debug, Clone)]
struct StackFrame {
    /// The links in this node.
    links: Vec<HamtLink>,
    /// Current position.
    position: usize,
}

impl<'a> HamtIter<'a> {
    /// Create a new iterator.
    pub fn new(shard: &'a HamtShard) -> Self {
        Self {
            shard,
            current_link: 0,
            current_bucket_pos: 0,
            stack: vec![StackFrame {
                links: shard.root.links.clone(),
                position: 0,
            }],
        }
    }
}

impl<'a> Iterator for HamtIter<'a> {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stack.is_empty() {
                return None;
            }

            let frame = self.stack.last_mut()?;

            if frame.position >= frame.links.len() {
                self.stack.pop();
                continue;
            }

            let link = &frame.links[frame.position];
            frame.position += 1;

            match link {
                HamtLink::Bucket { entries } => {
                    if self.current_bucket_pos < entries.len() {
                        let entry = entries[self.current_bucket_pos].clone();
                        self.current_bucket_pos += 1;
                        return Some(entry);
                    }
                    self.current_bucket_pos = 0;
                }
                HamtLink::Shard { .. } => {
                    // Would need to handle shard references
                    // For now, we skip them
                }
            }
        }
    }
}

/// An iterator that also yields the path to each entry.
#[derive(Debug, Clone)]
pub struct HamtPathIter<'a> {
    /// The underlying iterator.
    inner: HamtIter<'a>,
    /// Current path.
    path: Vec<usize>,
}

impl<'a> HamtPathIter<'a> {
    /// Create a new path iterator.
    pub fn new(shard: &'a HamtShard) -> Self {
        Self {
            inner: HamtIter::new(shard),
            path: Vec::new(),
        }
    }
}

impl<'a> Iterator for HamtPathIter<'a> {
    type Item = (Vec<usize>, String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(name, entry)| (self.path.clone(), name, entry))
    }
}

/// An iterator that yields entries in sorted order.
#[derive(Debug, Clone)]
pub struct SortedHamtIter {
    /// The underlying entries, sorted.
    entries: Vec<(String, HamtEntry)>,
    position: usize,
}

impl SortedHamtIter {
    /// Create a new sorted iterator.
    pub fn new(shard: &HamtShard) -> Self {
        let mut entries: Vec<_> = shard.list();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            entries,
            position: 0,
        }
    }
}

impl Iterator for SortedHamtIter {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        if self.position < self.entries.len() {
            let entry = self.entries[self.position].clone();
            self.position += 1;
            Some(entry)
        } else {
            None
        }
    }
}

/// An iterator that yields entries matching a prefix.
#[derive(Debug, Clone)]
pub struct PrefixHamtIter<'a> {
    /// The underlying iterator.
    inner: HamtIter<'a>,
    /// The prefix to match.
    prefix: String,
}

impl<'a> PrefixHamtIter<'a> {
    /// Create a new prefix iterator.
    pub fn new(shard: &'a HamtShard, prefix: String) -> Self {
        Self {
            inner: HamtIter::new(shard),
            prefix,
        }
    }
}

impl<'a> Iterator for PrefixHamtIter<'a> {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((name, entry)) => {
                    if name.starts_with(&self.prefix) {
                        return Some((name, entry));
                    }
                }
                None => return None,
            }
        }
    }
}

/// An iterator that yields entries in reverse order.
#[derive(Debug, Clone)]
pub struct RevHamtIter {
    /// The entries in reverse order.
    entries: Vec<(String, HamtEntry)>,
    position: usize,
}

impl RevHamtIter {
    /// Create a new reverse iterator.
    pub fn new(shard: &HamtShard) -> Self {
        let mut entries: Vec<_> = shard.list();
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        Self {
            entries,
            position: 0,
        }
    }
}

impl Iterator for RevHamtIter {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        if self.position < self.entries.len() {
            let entry = self.entries[self.position].clone();
            self.position += 1;
            Some(entry)
        } else {
            None
        }
    }
}

/// An iterator that yields entries with pagination.
#[derive(Debug, Clone)]
pub struct PagedHamtIter<'a> {
    /// The underlying iterator.
    inner: HamtIter<'a>,
    /// Page size.
    page_size: usize,
    /// Current page position.
    page_position: usize,
    /// Total yielded in current page.
    yielded: usize,
}

impl<'a> PagedHamtIter<'a> {
    /// Create a new paged iterator.
    pub fn new(shard: &'a HamtShard, page_size: usize) -> Self {
        Self {
            inner: HamtIter::new(shard),
            page_size,
            page_position: 0,
            yielded: 0,
        }
    }

    /// Get the current page number (0-indexed).
    pub fn current_page(&self) -> usize {
        self.page_position
    }

    /// Check if there's a next page.
    pub fn has_next_page(&self) -> bool {
        // This is approximate without consuming the iterator
        self.yielded >= self.page_size
    }

    /// Skip to a specific page.
    pub fn skip_to_page(&mut self, page: usize) {
        self.page_position = page;
        self.yielded = 0;
        // Note: Can't actually skip without consuming
        // This would need a collect-first approach
    }
}

impl<'a> Iterator for PagedHamtIter<'a> {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        if self.yielded >= self.page_size {
            self.page_position += 1;
            self.yielded = 0;
        }

        let result = self.inner.next();
        if result.is_some() {
            self.yielded += 1;
        }
        result
    }
}

/// An iterator over only directories.
#[derive(Debug, Clone)]
pub struct DirHamtIter<'a> {
    inner: HamtIter<'a>,
}

impl<'a> DirHamtIter<'a> {
    /// Create a new directory-only iterator.
    pub fn new(shard: &'a HamtShard) -> Self {
        Self {
            inner: HamtIter::new(shard),
        }
    }
}

impl<'a> Iterator for DirHamtIter<'a> {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((name, entry)) => {
                    if entry.is_dir() {
                        return Some((name, entry));
                    }
                }
                None => return None,
            }
        }
    }
}

/// An iterator over only files.
#[derive(Debug, Clone)]
pub struct FileHamtIter<'a> {
    inner: HamtIter<'a>,
}

impl<'a> FileHamtIter<'a> {
    /// Create a new file-only iterator.
    pub fn new(shard: &'a HamtShard) -> Self {
        Self {
            inner: HamtIter::new(shard),
        }
    }
}

impl<'a> Iterator for FileHamtIter<'a> {
    type Item = (String, HamtEntry);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((name, entry)) => {
                    if entry.is_file() {
                        return Some((name, entry));
                    }
                }
                None => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn create_test_shard() -> HamtShard {
        let mut shard = HamtShard::new();

        // Insert some entries
        for i in 0..10 {
            let name = format!("file{:02}.txt", i);
            shard
                .insert(
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

        shard
    }

    #[test]
    fn test_basic_iteration() {
        let shard = create_test_shard();
        let entries: Vec<_> = shard.iter().collect();
        assert_eq!(entries.len(), 10);
    }

    #[test]
    fn test_sorted_iteration() {
        let shard = create_test_shard();
        let entries: Vec<_> = SortedHamtIter::new(&shard).collect();

        // Verify sorted order
        for i in 1..entries.len() {
            assert!(entries[i].0 > entries[i - 1].0);
        }
    }

    #[test]
    fn test_reverse_iteration() {
        let shard = create_test_shard();
        let entries: Vec<_> = RevHamtIter::new(&shard).collect();

        // Verify reverse sorted order
        for i in 1..entries.len() {
            assert!(entries[i].0 < entries[i - 1].0);
        }
    }

    #[test]
    fn test_prefix_iteration() {
        let mut shard = HamtShard::new();

        shard
            .insert(
                "file01.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content1"),
                    size_bytes: 8,
                },
            )
            .unwrap();
        shard
            .insert(
                "file02.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content2"),
                    size_bytes: 8,
                },
            )
            .unwrap();
        shard
            .insert(
                "other.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content3"),
                    size_bytes: 8,
                },
            )
            .unwrap();

        let entries: Vec<_> = PrefixHamtIter::new(&shard, "file".to_string()).collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(name, _)| name.starts_with("file")));
    }

    #[test]
    fn test_paged_iteration() {
        let shard = create_test_shard();
        let mut iter = PagedHamtIter::new(&shard, 3);

        // First page
        let page0: Vec<_> = iter.by_ref().take(3).collect();
        assert_eq!(page0.len(), 3);
        assert_eq!(iter.current_page(), 0);

        // Second page
        let page1: Vec<_> = iter.by_ref().take(3).collect();
        assert_eq!(page1.len(), 3);
        assert_eq!(iter.current_page(), 1);

        // Third page
        let page2: Vec<_> = iter.by_ref().take(3).collect();
        assert_eq!(page2.len(), 3);
        assert_eq!(iter.current_page(), 2);

        // Fourth page (should have 1 remaining)
        let page3: Vec<_> = iter.by_ref().take(3).collect();
        assert_eq!(page3.len(), 1);
    }

    #[test]
    fn test_directory_filter() {
        let mut shard = HamtShard::new();

        shard
            .insert(
                "file.txt".to_string(),
                HamtEntry::File {
                    hash: adnet_types::ContentHash::from_bytes(b"content1"),
                    size_bytes: 8,
                },
            )
            .unwrap();
        shard
            .insert(
                "subdir".to_string(),
                HamtEntry::Directory {
                    hash: adnet_types::ContentHash::from_bytes(b"dir1"),
                    entry_count: 5,
                },
            )
            .unwrap();

        let dirs: Vec<_> = DirHamtIter::new(&shard).collect();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].0, "subdir");

        let files: Vec<_> = FileHamtIter::new(&shard).collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "file.txt");
    }
}
