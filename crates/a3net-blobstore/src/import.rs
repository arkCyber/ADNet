//! Asynchronous-friendly import helper built on top of [`BlobStore`].

use std::path::Path;

use a3net_types::ContentHash;

use crate::store::BlobStore;

/// Import a local file into the store, returning its content hash and size.
///
/// This wraps `import_file_sync` in `spawn_blocking` so it can be awaited
/// from an async context without blocking the runtime.
pub async fn import_file(store: &BlobStore, path: &Path) -> std::io::Result<(ContentHash, u64)> {
    let path = path.to_path_buf();
    let data_dir = store.data_dir().to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<(ContentHash, u64)> {
        // Reopen the store inside the blocking thread so it doesn't share
        // a mutable filesystem view with the runtime.
        let store = BlobStore::new(&data_dir)?;
        store.import_file_sync(&path)
    })
    .await
    .map_err(std::io::Error::other)?
}
