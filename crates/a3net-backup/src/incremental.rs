//! Incremental backup support.
//!
//! DO-178C SR-1: Efficient incremental backups reduce storage and time costs.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use a3net_error::{ErrorKind, IntoReport, Severity};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

/// Error types for incremental backup operations.
#[derive(Debug, Error)]
pub enum IncrementalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("base backup not found: {0}")]
    BaseBackupNotFound(String),
    #[error("manifest not found: {0}")]
    ManifestNotFound(String),
    #[error("checksum mismatch: {path} expected {expected}, got {got}")]
    ChecksumMismatch { path: String, expected: String, got: String },
    #[error("invalid backup chain: {0}")]
    InvalidChain(String),
}

// P0-5: unified error reporting.  Codes `BKP-101`..`BKP-106` are
// the incremental layer's slice — distinct from `BackupError`'s
// `BKP-001`..`BKP-006` so dashboards can tell the two failure
// surfaces apart.
impl IntoReport for IncrementalError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "BKP-101",
            Self::Serialization(_) => "BKP-102",
            Self::BaseBackupNotFound(_) => "BKP-103",
            Self::ManifestNotFound(_) => "BKP-104",
            Self::ChecksumMismatch { .. } => "BKP-105",
            Self::InvalidChain(_) => "BKP-106",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            // `BaseBackupNotFound` and `ManifestNotFound` are caller
            // errors — they asked to restore an id we don't have.
            Self::BaseBackupNotFound(_) | Self::ManifestNotFound(_) => ErrorKind::NotFound,
            // The chain is broken — operator action required but not
            // an immediate data-loss event.
            Self::InvalidChain(_) => ErrorKind::Internal,
            // Everything else is either an I/O fault or a bytes-on-disk
            // mismatch: retriable / transient in the I/O case, definite
            // corruption in the checksum case.
            Self::Io(_) | Self::Serialization(_) => ErrorKind::Unavailable,
            Self::ChecksumMismatch { .. } => ErrorKind::DataLoss,
        }
    }

    fn severity(&self) -> Severity {
        match self {
            // A missing backup id is a user error — warn, don't page.
            Self::BaseBackupNotFound(_) | Self::ManifestNotFound(_) => Severity::Warn,
            // Real problems that need an operator.
            _ => Severity::Error,
        }
    }
}

/// Type of change detected for a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// File was added (new).
    Added,
    /// File was modified.
    Modified,
    /// File was deleted.
    Deleted,
    /// File was renamed.
    Renamed { from: String },
}

/// Entry in an incremental backup manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncrementalEntry {
    /// Root-relative path of the file.
    pub path: String,
    /// Type of change.
    pub change_type: ChangeType,
    /// File size in bytes.
    pub size: u64,
    /// BLAKE3 hash of the file contents.
    pub blake3: String,
    /// Previous BLAKE3 hash (for modified files).
    pub previous_blake3: Option<String>,
    /// Unix timestamp of the change.
    pub changed_at: i64,
}

/// Manifest for an incremental backup chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncrementalManifest {
    /// Unique identifier for this backup.
    pub id: String,
    /// Sequence number in the chain.
    pub sequence: u64,
    /// Parent backup ID (None for base backup).
    pub parent_id: Option<String>,
    /// Unix timestamp when this backup was created.
    pub created_at: i64,
    /// Number of files in this backup (changed files only).
    pub file_count: usize,
    /// Total size of changed files.
    pub total_bytes: u64,
    /// Changes in this backup.
    pub changes: Vec<IncrementalEntry>,
    /// Cumulative stats from base to this backup.
    pub cumulative: CumulativeStats,
}

/// Cumulative statistics across the backup chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeStats {
    /// Total files in the full backup set.
    pub total_files: usize,
    /// Total bytes in the full backup set.
    pub total_bytes: u64,
    /// Number of backups in the chain.
    pub chain_length: u64,
}

/// Incremental backup manager.
#[derive(Debug)]
pub struct IncrementalBackup {
    /// Base backup directory.
    base_dir: PathBuf,
    /// Backup chain manifest storage.
    chain_dir: PathBuf,
    /// Current backup state.
    current_manifest: Option<IncrementalManifest>,
}

impl IncrementalBackup {
    /// Create a new incremental backup manager.
    pub fn new(base_dir: impl AsRef<Path>, chain_dir: impl AsRef<Path>) -> Self {
        fs::create_dir_all(&chain_dir).ok();
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
            chain_dir: chain_dir.as_ref().to_path_buf(),
            current_manifest: None,
        }
    }

    /// Get the path to the chain manifest file.
    fn chain_manifest_path(&self) -> PathBuf {
        self.chain_dir.join("chain_manifest.json")
    }

    /// Load the chain manifest from disk.
    pub fn load_chain(&self) -> Result<Vec<IncrementalManifest>, IncrementalError> {
        let path = self.chain_manifest_path();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&path)?;
        let manifests: Vec<IncrementalManifest> = serde_json::from_str(&content)?;
        Ok(manifests)
    }

    /// Save the chain manifest to disk.
    pub fn save_chain(&self, manifests: &[IncrementalManifest]) -> Result<(), IncrementalError> {
        let content = serde_json::to_string_pretty(manifests)?;
        let path = self.chain_manifest_path();

        // P0-2: Use raw File + BufWriter so we can call sync_all().
        // fs::write() does not guarantee durability on crash — a process
        // kill after the kernel has buffered the data but before it has
        // been flushed to disk leaves a zero-length or partial file.
        let file = std::fs::File::create(&path)?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(content.as_bytes())?;
        writer.flush()?;
        writer.get_ref().sync_all()?;

        Ok(())
    }

    /// Get the latest backup in the chain.
    pub fn get_latest(&self) -> Result<Option<IncrementalManifest>, IncrementalError> {
        let chain = self.load_chain()?;
        Ok(chain.into_iter().last())
    }

    /// Create a new base backup (full backup).
    ///
    /// DO-178C SR-1: Base backup establishes the chain.
    pub fn create_base_backup(&self) -> Result<IncrementalManifest, IncrementalError> {
        let entries = self.scan_directory(&self.base_dir)?;
        let file_count = entries.len();
        let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

        let manifest = IncrementalManifest {
            id: generate_backup_id(),
            sequence: 0,
            parent_id: None,
            created_at: Utc::now().timestamp(),
            file_count,
            total_bytes,
            changes: entries,
            cumulative: CumulativeStats {
                total_files: file_count,
                total_bytes,
                chain_length: 0,
            },
        };

        // Save backup data
        self.save_backup_data(&manifest)?;
        
        // Update chain
        let mut chain = self.load_chain()?;
        chain.push(manifest.clone());
        self.save_chain(&chain)?;

        info!(
            id = %manifest.id,
            files = file_count,
            bytes = total_bytes,
            "Base backup created"
        );

        Ok(manifest)
    }

    /// Create an incremental backup based on the latest backup.
    ///
    /// DO-178C SR-1: Incremental backup captures only changes.
    pub fn create_incremental(&self) -> Result<IncrementalManifest, IncrementalError> {
        let chain = self.load_chain()?;
        let parent = chain.last().ok_or_else(|| {
            IncrementalError::InvalidChain("No base backup exists".to_string())
        })?;

        // Compute changes since last backup
        let changes = self.compute_changes(parent)?;
        let file_count = changes.len();
        let total_bytes: u64 = changes.iter().map(|e| e.size).sum();

        let manifest = IncrementalManifest {
            id: generate_backup_id(),
            sequence: parent.sequence + 1,
            parent_id: Some(parent.id.clone()),
            created_at: Utc::now().timestamp(),
            file_count,
            total_bytes,
            changes,
            cumulative: CumulativeStats {
                total_files: parent.cumulative.total_files,
                total_bytes: parent.cumulative.total_bytes,
                chain_length: parent.cumulative.chain_length + 1,
            },
        };

        // Save backup data
        self.save_backup_data(&manifest)?;

        // Update chain
        let mut chain = self.load_chain()?;
        chain.push(manifest.clone());
        self.save_chain(&chain)?;

        info!(
            id = %manifest.id,
            sequence = manifest.sequence,
            files = file_count,
            bytes = total_bytes,
            "Incremental backup created"
        );

        Ok(manifest)
    }

    /// Scan directory and create entries for all files.
    fn scan_directory(&self, dir: &Path) -> Result<Vec<IncrementalEntry>, IncrementalError> {
        let mut entries = Vec::new();

        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let rel_path = path
                .strip_prefix(dir)
                .map_err(|e| IncrementalError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))?
                .to_string_lossy()
                .replace('\\', "/");

            let metadata = fs::metadata(path)?;
            let bytes = fs::read(path)?;
            let blake3_hash = blake3::hash(&bytes).to_hex().to_string();

            entries.push(IncrementalEntry {
                path: rel_path,
                change_type: ChangeType::Added,
                size: metadata.len(),
                blake3: blake3_hash,
                previous_blake3: None,
                changed_at: metadata.modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0)
                    })
                    .unwrap_or(0),
            });
        }

        Ok(entries)
    }

    /// Compute changes between current state and last backup.
    fn compute_changes(&self, parent: &IncrementalManifest) -> Result<Vec<IncrementalEntry>, IncrementalError> {
        let mut changes = Vec::new();
        let mut current_files: HashMap<String, (u64, String)> = HashMap::new();

        // Scan current directory
        for entry in walkdir::WalkDir::new(&self.base_dir)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let rel_path = path
                .strip_prefix(&self.base_dir)
                .map_err(|e| IncrementalError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))?
                .to_string_lossy()
                .replace('\\', "/");

            let bytes = fs::read(path)?;
            let blake3_hash = blake3::hash(&bytes).to_hex().to_string();
            let size = bytes.len() as u64;

            current_files.insert(rel_path.clone(), (size, blake3_hash));
        }

        // Build a map of the previous backup state
        let mut previous_files: HashMap<String, String> = HashMap::new();
        let mut previous_state: HashMap<String, (u64, String)> = HashMap::new();

        // Load all previous manifests to reconstruct state
        let chain = self.load_chain()?;
        for manifest in &chain {
            for change in &manifest.changes {
                match change.change_type {
                    ChangeType::Added | ChangeType::Modified => {
                        previous_files.insert(change.path.clone(), change.blake3.clone());
                        previous_state.insert(change.path.clone(), (change.size, change.blake3.clone()));
                    }
                    ChangeType::Deleted => {
                        previous_files.remove(&change.path);
                        previous_state.remove(&change.path);
                    }
                    ChangeType::Renamed { ref from } => {
                        previous_files.remove(from);
                        previous_state.remove(from);
                        previous_files.insert(change.path.clone(), change.blake3.clone());
                        previous_state.insert(change.path.clone(), (change.size, change.blake3.clone()));
                    }
                }
            }
        }

        // Find deleted files
        for (path, _) in &previous_files {
            if !current_files.contains_key(path) {
                changes.push(IncrementalEntry {
                    path: path.clone(),
                    change_type: ChangeType::Deleted,
                    size: 0,
                    blake3: String::new(),
                    previous_blake3: previous_files.get(path).cloned(),
                    changed_at: Utc::now().timestamp(),
                });
            }
        }

        // Find added and modified files
        for (path, (size, blake3)) in &current_files {
            let previous = previous_files.get(path);
            if previous.is_none() {
                // New file
                changes.push(IncrementalEntry {
                    path: path.clone(),
                    change_type: ChangeType::Added,
                    size: *size,
                    blake3: blake3.clone(),
                    previous_blake3: None,
                    changed_at: Utc::now().timestamp(),
                });
            } else if previous.unwrap() != blake3 {
                // Modified file
                changes.push(IncrementalEntry {
                    path: path.clone(),
                    change_type: ChangeType::Modified,
                    size: *size,
                    blake3: blake3.clone(),
                    previous_blake3: previous.cloned(),
                    changed_at: Utc::now().timestamp(),
                });
            }
        }

        Ok(changes)
    }

    /// Save backup data to disk.
    fn save_backup_data(&self, manifest: &IncrementalManifest) -> Result<(), IncrementalError> {
        let backup_dir = self.chain_dir.join(&manifest.id);
        fs::create_dir_all(&backup_dir)?;

        // Save manifest
        let manifest_path = backup_dir.join("manifest.json");
        let content = serde_json::to_string_pretty(manifest)?;
        fs::write(manifest_path, content)?;

        // Save changed files
        for entry in &manifest.changes {
            match entry.change_type {
                ChangeType::Deleted => {}
                _ => {
                    let source = self.base_dir.join(&entry.path);
                    if source.exists() {
                        let dest = backup_dir.join(&entry.path);
                        if let Some(parent) = dest.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        // P0-2: Copy the file and sync it.  The source file is
                        // already durable (it's the running node's data); the
                        // copy must be made durable before the chain manifest
                        // references this backup, otherwise a crash between the
                        // copy and the manifest update can leave an orphaned
                        // backup dir that the chain doesn't know about.
                        std::fs::copy(&source, &dest)?;
                        // P0-2: sync the copied file so the backup dir's
                        // contents are durable before the manifest references them.
                        let dest_file = std::fs::OpenOptions::new()
                            .write(true)
                            .open(&dest)?;
                        dest_file.sync_all()?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Restore to a specific backup point.
    ///
    /// DO-178C SR-1: Restore reconstructs full state from backup chain.
    pub fn restore(&self, target_id: &str) -> Result<(), IncrementalError> {
        let chain = self.load_chain()?;
        
        // Find target in chain
        let target_idx = chain.iter().position(|m| m.id == target_id)
            .ok_or_else(|| IncrementalError::BaseBackupNotFound(target_id.to_string()))?;

        // Rebuild state by applying backups sequentially
        let mut state: HashMap<String, Vec<u8>> = HashMap::new();

        for manifest in chain.iter().take(target_idx + 1) {
            let backup_dir = self.chain_dir.join(&manifest.id);
            
            for entry in &manifest.changes {
                match entry.change_type {
                    ChangeType::Added | ChangeType::Modified | ChangeType::Renamed { .. } => {
                        let source = backup_dir.join(&entry.path);
                        if source.exists() {
                            state.insert(entry.path.clone(), fs::read(&source)?);
                        }
                    }
                    ChangeType::Deleted => {
                        state.remove(&entry.path);
                    }
                }
            }
        }

        // Apply state to restore directory
        let restore_dir = self.base_dir.join("restored");
        fs::create_dir_all(&restore_dir)?;

        for (path, contents) in &state {
            let dest = restore_dir.join(path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&dest, contents)?;
        }

        info!(
            target = %target_id,
            files = state.len(),
            "Restore completed"
        );

        Ok(())
    }
}

/// Generate a unique backup ID.
fn generate_backup_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_change_type_serialization() {
        let ct = ChangeType::Added;
        let json = serde_json::to_string(&ct).unwrap();
        assert!(json.contains("added"));

        let ct = ChangeType::Modified;
        let json = serde_json::to_string(&ct).unwrap();
        assert!(json.contains("modified"));

        let ct = ChangeType::Deleted;
        let json = serde_json::to_string(&ct).unwrap();
        assert!(json.contains("deleted"));
    }

    #[test]
    fn test_incremental_backup_creation() {
        let base = tempfile::tempdir().unwrap();
        let chain = tempfile::tempdir().unwrap();

        // Write initial files
        std::fs::write(base.path().join("file1.txt"), b"hello").unwrap();
        std::fs::write(base.path().join("file2.txt"), b"world").unwrap();

        let backup = IncrementalBackup::new(base.path(), chain.path());

        // Create base backup
        let manifest = backup.create_base_backup().unwrap();
        assert_eq!(manifest.sequence, 0);
        assert!(manifest.parent_id.is_none());
        assert_eq!(manifest.file_count, 2);
    }

    // ── IntoReport pin tests (P0-5) ───────────────────────────────────────

    #[test]
    fn incremental_error_codes_are_stable() {
        use a3net_error::{ErrorKind, Severity};

        let pairs: Vec<(IncrementalError, &str, ErrorKind, Severity)> = vec![
            (
                IncrementalError::BaseBackupNotFound("x".into()),
                "BKP-103",
                ErrorKind::NotFound,
                Severity::Warn,
            ),
            (
                IncrementalError::ManifestNotFound("x".into()),
                "BKP-104",
                ErrorKind::NotFound,
                Severity::Warn,
            ),
            (
                IncrementalError::ChecksumMismatch {
                    path: "p".into(),
                    expected: "e".into(),
                    got: "g".into(),
                },
                "BKP-105",
                ErrorKind::DataLoss,
                Severity::Error,
            ),
            (
                IncrementalError::InvalidChain("x".into()),
                "BKP-106",
                ErrorKind::Internal,
                Severity::Error,
            ),
        ];
        for (err, code, kind, sev) in pairs {
            assert_eq!(err.code(), code, "code for {err:?}");
            assert_eq!(err.kind(), kind, "kind for {err:?}");
            assert_eq!(err.severity(), sev, "severity for {err:?}");
        }
    }

    #[test]
    fn incremental_error_into_report_emits() {
        let e = IncrementalError::InvalidChain("chain broken".into());
        let report = e.into_report("a3net-backup");
        assert_eq!(report.code, "BKP-106");
        assert_eq!(
            report.details.get("crate").and_then(|v| v.as_str()),
            Some("a3net-backup"),
        );
        assert!(report.cause.as_deref().unwrap_or("").contains("chain broken"));
    }
}
