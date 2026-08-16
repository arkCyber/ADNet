//! Async block storage traits for GraphSync integration.
//!
//! This module provides async versions of the block store traits
//! used by GraphSync, enabling integration with tokio-based
//! async runtimes.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use a3net_types::cid::Cid;
use a3net_types::graphsync::BlockStore as SyncBlockStore;

/// Errors for async block storage operations.
#[derive(Debug, thiserror::Error)]
pub enum AsyncBlockStoreError {
    #[error("block not found: {0}")]
    BlockNotFound(Cid),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("store error: {0}")]
    Store(String),
}

/// Result type for async block store operations.
pub type AsyncResult<T> = Pin<Box<dyn Future<Output = Result<T, AsyncBlockStoreError>> + Send>>;

/// Async block store trait for GraphSync.
pub trait AsyncBlockStore: Send + Sync {
    fn get(&self, cid: &Cid) -> AsyncResult<Option<Vec<u8>>>;
    fn put(&self, cid: &Cid, data: &[u8]) -> AsyncResult<()>;
    fn has(&self, cid: &Cid) -> AsyncResult<bool>;
    fn links(&self, cid: &Cid) -> AsyncResult<Vec<Cid>>;
    fn remove(&self, cid: &Cid) -> AsyncResult<bool>;
}

/// Adapter that wraps a synchronous BlockStore for async use.
pub struct AsyncBlockStoreAdapter<S: SyncBlockStore + 'static> {
    inner: Arc<S>,
}

impl<S: SyncBlockStore + 'static> AsyncBlockStoreAdapter<S> {
    pub fn new(inner: Arc<S>) -> Self {
        Self { inner }
    }
}

impl<S: SyncBlockStore + 'static> AsyncBlockStore for AsyncBlockStoreAdapter<S> {
    fn get(&self, cid: &Cid) -> AsyncResult<Option<Vec<u8>>> {
        let inner = self.inner.clone();
        let cid = cid.clone();
        Box::pin(async move {
            Ok(inner.get(&cid))
        })
    }

    fn put(&self, cid: &Cid, data: &[u8]) -> AsyncResult<()> {
        let inner = self.inner.clone();
        let cid = cid.clone();
        let data = data.to_vec();
        Box::pin(async move {
            inner.put(&cid, &data);
            Ok(())
        })
    }

    fn has(&self, cid: &Cid) -> AsyncResult<bool> {
        let inner = self.inner.clone();
        let cid = cid.clone();
        Box::pin(async move {
            Ok(inner.has(&cid))
        })
    }

    fn links(&self, cid: &Cid) -> AsyncResult<Vec<Cid>> {
        let inner = self.inner.clone();
        let cid = cid.clone();
        Box::pin(async move {
            Ok(inner.links(&cid))
        })
    }

    fn remove(&self, cid: &Cid) -> AsyncResult<bool> {
        Box::pin(async move {
            // Default: not supported for sync store
            Ok(false)
        })
    }
}

impl<S: SyncBlockStore + 'static> From<Arc<S>> for AsyncBlockStoreAdapter<S> {
    fn from(inner: Arc<S>) -> Self {
        Self::new(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct MockStore {
        blocks: std::sync::Mutex<std::collections::HashMap<Cid, Vec<u8>>>,
    }

    impl SyncBlockStore for MockStore {
        fn get(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.blocks.lock().unwrap().get(cid).cloned()
        }

        fn put(&self, cid: &Cid, block: &[u8]) {
            self.blocks.lock().unwrap().insert(cid.clone(), block.to_vec());
        }

        fn has(&self, cid: &Cid) -> bool {
            self.blocks.lock().unwrap().contains_key(cid)
        }

        fn links(&self, _cid: &Cid) -> Vec<Cid> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn test_async_adapter() {
        let store = Arc::new(MockStore::default());
        let cid = Cid::from_content_blake3(b"test");
        store.put(&cid, b"data");

        let adapter = AsyncBlockStoreAdapter::new(store.clone());
        assert!(adapter.has(&cid).await.unwrap());
        assert_eq!(adapter.get(&cid).await.unwrap(), Some(b"data".to_vec()));
    }

    #[tokio::test]
    async fn test_get_many() {
        let store = Arc::new(MockStore::default());
        let cid1 = Cid::from_content_blake3(b"test1");
        let cid2 = Cid::from_content_blake3(b"test2");

        store.put(&cid1, b"data1");
        store.put(&cid2, b"data2");

        let adapter = AsyncBlockStoreAdapter::new(store.clone());
        let mut results = HashMap::new();
        
        if let Ok(Some(data)) = adapter.get(&cid1).await {
            results.insert(cid1, data);
        }
        if let Ok(Some(data)) = adapter.get(&cid2).await {
            results.insert(cid2, data);
        }

        assert_eq!(results.len(), 2);
    }
}
