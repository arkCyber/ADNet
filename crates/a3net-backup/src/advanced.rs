//! Advanced backup features: incremental, encrypted, and remote backup support.
//!
//! DO-178C DAL-B: Enhanced backup functionality for production deployments.
//!
//! ## Features
//!
//! - **Incremental Backups**: Only backup files that have changed since the last backup
//! - **Encryption**: AES-256-GCM encryption with XChaCha20Poly1305 for backup files
//! - **Remote Upload**: S3, IPFS, and custom remote storage support

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Re-export types from the parent crate level for convenience
pub use crate::incremental::{IncrementalBackup, IncrementalManifest, ChangeType};
pub use crate::encryption::{EncryptedBackup, EncryptionKey, KeyDerivation};
pub use crate::remote::{RemoteBackend, S3Backend, IpfsBackend, HttpBackend};
pub use crate::incremental_store::BackupStore as IncrementalBackupStore;
