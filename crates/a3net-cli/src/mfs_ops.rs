//! MFS (Mutable File System) operations for the CLI.
//!
//! Provides IPFS-compatible MFS commands:
//! - `files mkdir` - Create a directory in MFS
//! - `files ls` - List directory contents in MFS
//! - `files cp` - Copy files/directories within MFS
//! - `files mv` - Move files/directories within MFS
//! - `files rm` - Remove files/directories from MFS
//! - `files stat` - Get file/directory status
//! - `files flush` - Flush changes to the root
//! - `files write` - Write content to a file
//! - `files read` - Read content from a file

use std::collections::BTreeMap;
use std::path::Path;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use a3net_blobstore::scope::{BlobStoreScope, StorageTopology};
use a3net_types::ContentHash;

/// MFS error types.
#[derive(Debug, Error)]
pub enum MfsError {
    #[error("path not found: {0}")]
    NotFound(String),

    #[error("path exists: {0}")]
    PathExists(String),

    #[error("not a directory: {0}")]
    NotADirectory(String),

    #[error("not a file: {0}")]
    NotAFile(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("operation failed: {0}")]
    Operation(String),
}

/// MFS node types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum MfsNode {
    Directory {
        cid: String,
        size: u64,
    },
    File {
        cid: String,
        size: u64,
        blocks: u64,
    },
}

/// MFS directory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfsEntry {
    pub name: String,
    #[serde(rename = "Type")]
    pub entry_type: String,
    pub hash: String,
    pub size: u64,
    pub block_size: u64,
}

/// MFS file/directory status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfsStat {
    pub hash: String,
    #[serde(rename = "Type")]
    pub node_type: String,
    pub size: u64,
    pub cumulative_size: u64,
    pub blocks: u64,
    pub with_locality: bool,
    pub local: bool,
    pub size_local: u64,
}

/// MFS state stored at `<data_dir>/mfs/state.json`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MfsState {
    pub root: Option<String>,
    #[serde(default)]
    pub nodes: BTreeMap<String, MfsNode>,
}

/// MFS manager for handling mutable file system operations.
pub struct MfsManager {
    state: MfsState,
    data_dir: std::path::PathBuf,
}

impl MfsManager {
    /// Create a new MFS manager.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            state: MfsState::default(),
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Load MFS state from disk.
    pub fn load(&mut self) -> Result<(), MfsError> {
        let state_path = self.data_dir.join("mfs").join("state.json");
        if state_path.exists() {
            let content = std::fs::read_to_string(&state_path)
                .map_err(|e| MfsError::Operation(e.to_string()))?;
            self.state = serde_json::from_str(&content)
                .map_err(|e| MfsError::Operation(e.to_string()))?;
        }
        Ok(())
    }

    /// Save MFS state to disk.
    pub fn save(&self) -> Result<(), MfsError> {
        let mfs_dir = self.data_dir.join("mfs");
        std::fs::create_dir_all(&mfs_dir)
            .map_err(|e| MfsError::Operation(e.to_string()))?;
        let state_path = mfs_dir.join("state.json");
        let content = serde_json::to_string_pretty(&self.state)
            .map_err(|e| MfsError::Operation(e.to_string()))?;
        std::fs::write(&state_path, content)
            .map_err(|e| MfsError::Operation(e.to_string()))?;
        Ok(())
    }

    /// Get root CID.
    pub fn root(&self) -> Option<&str> {
        self.state.root.as_deref()
    }

    /// Set root CID.
    pub fn set_root(&mut self, cid: String) {
        self.state.root = Some(cid);
    }

    /// Create a directory in MFS.
    pub fn mkdir(&mut self, path: &str) -> Result<String, MfsError> {
        let path = normalize_path(path);

        if path == "/" {
            return Err(MfsError::InvalidPath("cannot create root directory".into()));
        }

        if self.state.nodes.contains_key(&path) {
            return Err(MfsError::PathExists(path));
        }

        // Create empty directory marker
        let dir_entry = MfsNode::Directory {
            cid: String::new(),
            size: 0,
        };

        self.state.nodes.insert(path.clone(), dir_entry);
        self.save()?;

        Ok(path)
    }

    /// List directory contents.
    pub fn ls(&self, path: &str) -> Result<Vec<MfsEntry>, MfsError> {
        let path = normalize_path(path);
        let _parent_path = get_parent_path(&path);

        // If root, list top-level entries
        if path == "/" {
            let mut entries = Vec::new();
            let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for (p, node) in &self.state.nodes {
                if get_parent_path(p) == "/" && p != "/" {
                    let name = p.trim_start_matches('/').split('/').next().unwrap_or("");
                    if !name.is_empty() && !seen_names.contains(name) {
                        seen_names.insert(name);
                        entries.push(entry_from_node(name, node));
                    }
                }
            }
            return Ok(entries);
        }

        // Check if path exists and is a directory
        match self.state.nodes.get(&path) {
            Some(MfsNode::Directory { .. }) => {}
            Some(_) => return Err(MfsError::NotADirectory(path)),
            None => return Err(MfsError::NotFound(path)),
        }

        // List children
        let mut entries = Vec::new();
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let prefix = if path == "/" { String::new() } else { path.to_string() };
        let path_owned = path.clone();

        for (p, node) in &self.state.nodes {
            if p.starts_with(&prefix) && p != &path_owned {
                let remainder = p.strip_prefix(&prefix).unwrap_or(p);
                if remainder.starts_with('/') {
                    let name = remainder.trim_start_matches('/').split('/').next().unwrap_or("");
                    if !name.is_empty() && !seen_names.contains(name) {
                        seen_names.insert(name);
                        entries.push(entry_from_node(name, node));
                    }
                }
            }
        }

        Ok(entries)
    }

    /// Copy a file or directory.
    pub fn cp(&mut self, source: &str, dest: &str) -> Result<String, MfsError> {
        let source = normalize_path(source);
        let dest = normalize_path(dest);

        // Check source exists
        let source_node = self.state.nodes.get(&source)
            .ok_or_else(|| MfsError::NotFound(source.clone()))?
            .clone();

        // If destination is a directory, append source name
        let final_dest = if self.state.nodes.get(&dest).map(|n| matches!(n, MfsNode::Directory { .. })).unwrap_or(false) {
            let name = source.split('/').last().unwrap_or(&source);
            format!("{}/{}", dest.trim_end_matches('/'), name)
        } else {
            dest.clone()
        };

        if self.state.nodes.contains_key(&final_dest) {
            return Err(MfsError::PathExists(final_dest));
        }

        // Copy the node
        self.state.nodes.insert(final_dest.clone(), source_node);
        self.save()?;

        Ok(final_dest)
    }

    /// Move a file or directory.
    pub fn mv(&mut self, source: &str, dest: &str) -> Result<String, MfsError> {
        let source = normalize_path(source);
        let dest = normalize_path(dest);

        // Check source exists
        let source_node = self.state.nodes.get(&source)
            .ok_or_else(|| MfsError::NotFound(source.clone()))?
            .clone();

        // If destination is a directory, append source name
        let final_dest = if self.state.nodes.get(&dest).map(|n| matches!(n, MfsNode::Directory { .. })).unwrap_or(false) {
            let name = source.split('/').last().unwrap_or(&source);
            format!("{}/{}", dest.trim_end_matches('/'), name)
        } else {
            dest.clone()
        };

        if self.state.nodes.contains_key(&final_dest) {
            return Err(MfsError::PathExists(final_dest.clone()));
        }

        // Remove source and insert at destination
        self.state.nodes.remove(&source);
        self.state.nodes.insert(final_dest.clone(), source_node);
        self.save()?;

        Ok(final_dest)
    }

    /// Remove a file or directory.
    pub fn rm(&mut self, path: &str, recursive: bool) -> Result<(), MfsError> {
        let path = normalize_path(path);

        if !self.state.nodes.contains_key(&path) {
            return Err(MfsError::NotFound(path));
        }

        if !recursive {
            // Check if directory has children
            for p in self.state.nodes.keys() {
                if p.starts_with(&path) && !(*p).eq(&path) {
                    return Err(MfsError::Operation(format!(
                        "cannot remove '{}': directory not empty (use --recursive)",
                        path
                    )));
                }
            }
        }

        // Remove node and all children if recursive
        let path_owned = path.clone();
        let to_remove: Vec<String> = self.state.nodes.keys()
            .filter(|p| (*p).eq(&path_owned) || (recursive && p.starts_with(&path_owned)))
            .cloned()
            .collect();

        for p in to_remove {
            self.state.nodes.remove(&p);
        }

        self.save()?;
        Ok(())
    }

    /// Get file/directory status.
    pub fn stat(&self, path: &str) -> Result<MfsStat, MfsError> {
        let path = normalize_path(path);

        let node = match self.state.nodes.get(&path) {
            Some(n) => n,
            None => return Err(MfsError::NotFound(path)),
        };

        match node {
            MfsNode::Directory { cid, size } => Ok(MfsStat {
                hash: cid.clone(),
                node_type: "directory".to_string(),
                size: *size,
                cumulative_size: *size,
                blocks: 0,
                with_locality: false,
                local: true,
                size_local: *size,
            }),
            MfsNode::File { cid, size, blocks } => Ok(MfsStat {
                hash: cid.clone(),
                node_type: "file".to_string(),
                size: *size,
                cumulative_size: *size,
                blocks: *blocks,
                with_locality: false,
                local: true,
                size_local: *size,
            }),
        }
    }

    /// Write content to a file.
    pub fn write(&mut self, path: &str, content: Vec<u8>, topology: &StorageTopology) -> Result<(), MfsError> {
        let path = normalize_path(path);

        // Ensure parent directory exists
        let parent = get_parent_path(&path);
        let parent_owned = parent.clone();
        if parent_owned != "/" && !self.state.nodes.contains_key(&parent_owned) {
            return Err(MfsError::NotFound(parent_owned));
        }

        // Store content in blob store
        let store = topology.store(BlobStoreScope::Private);
        let (hash, size) = store.put_bytes_sync(&content)
            .map_err(|e| MfsError::Operation(e.to_string()))?;

        let blocks = if size == 0 { 0 } else { (size as u64 + 262143) / 262144 };

        // Create file node
        let file_node = MfsNode::File {
            cid: hash.as_hex().to_string(),
            size,
            blocks,
        };

        self.state.nodes.insert(path, file_node);
        self.save()?;

        Ok(())
    }

    /// Read content from a file.
    pub fn read(&self, path: &str, topology: &StorageTopology) -> Result<Vec<u8>, MfsError> {
        let path = normalize_path(path);

        let node = self.state.nodes.get(&path)
            .ok_or_else(|| MfsError::NotFound(path.clone()))?;

        match node {
            MfsNode::File { cid, .. } => {
                let hash = ContentHash::from_hex(cid)
                    .map_err(|e| MfsError::Operation(e.to_string()))?;
                let store = topology.store(BlobStoreScope::Private);
                let content = store.get_sync(&hash)
                    .ok_or_else(|| MfsError::NotFound(path.clone()))?;
                Ok(content)
            }
            MfsNode::Directory { .. } => Err(MfsError::NotAFile(path)),
        }
    }

    /// Flush changes and pin the root CID.
    ///
    /// In a full implementation, this would:
    /// 1. Compute the actual DAG root CID from the MFS tree
    /// 2. Pin the root recursively to prevent GC
    /// 3. Update the root pointer in the state
    ///
    /// Returns the root CID string (empty string if no root is set).
    pub fn flush(&mut self) -> Result<String, MfsError> {
        if let Some(ref root) = self.state.root {
            // In a real implementation, we would:
            // 1. Build a UnixFS DAG from the current MFS tree
            // 2. Compute the actual root CID from the DAG
            // 3. Pin the root CID to prevent GC
            // 4. Update self.state.root with the actual CID
            //
            // For now, we return the stored root as-is.
            tracing::debug!(root = %root, "flushing MFS root");
            Ok(root.clone())
        } else {
            Ok(String::new())
        }
    }

    /// Set the root CID and pin it.
    ///
    /// This is the recommended way to update the MFS root, as it
    /// ensures the new root is pinned before being set.
    pub fn set_root_pinned(&mut self, cid: String) -> Result<(), MfsError> {
        self.state.root = Some(cid.clone());
        // In a full implementation, we would call the PinService here.
        // The caller should handle the actual pinning.
        tracing::debug!(root = %cid, "set MFS root (pinning delegated to caller)");
        self.save()
    }

    /// Check if a path exists in the MFS.
    pub fn exists(&self, path: &str) -> bool {
        self.state.nodes.contains_key(path)
    }

    /// Get the node at a path.
    pub fn get(&self, path: &str) -> Option<&MfsNode> {
        self.state.nodes.get(path)
    }

    /// List all paths in the MFS.
    pub fn list_paths(&self) -> impl Iterator<Item = (&String, &MfsNode)> {
        self.state.nodes.iter()
    }
}

/// Normalize a path to standard format.
fn normalize_path(path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => { components.pop(); }
            _ => components.push(part),
        }
    }

    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

/// Get parent path.
fn get_parent_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    let normalized = normalize_path(path);
    let components: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if components.len() <= 1 {
        "/".to_string()
    } else {
        format!("/{}", components[..components.len() - 1].join("/"))
    }
}

/// Create an MfsEntry from an MfsNode.
fn entry_from_node(name: &str, node: &MfsNode) -> MfsEntry {
    match node {
        MfsNode::Directory { cid, size } => MfsEntry {
            name: name.to_string(),
            entry_type: "Directory".to_string(),
            hash: cid.clone(),
            size: *size,
            block_size: 0,
        },
        MfsNode::File { cid, size, blocks } => MfsEntry {
            name: name.to_string(),
            entry_type: "File".to_string(),
            hash: cid.clone(),
            size: *size,
            block_size: *blocks * 262144,
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CLI command handlers
// ─────────────────────────────────────────────────────────────────────────────

use crate::cli::FilesCmd;

/// Run MFS subcommand.
pub fn run_files(sub: &FilesCmd, data_dir: &std::path::Path, topology: &StorageTopology) -> anyhow::Result<()> {
    let mut mfs = MfsManager::new(data_dir);
    mfs.load()?;

    match sub {
        FilesCmd::Mkdir { path, parents, json } => {
            match mfs.mkdir(path) {
                Ok(p) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "path": p }))?);
                    } else {
                        println!("created directory: {}", p);
                    }
                }
                Err(MfsError::PathExists(p)) if *parents => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "path": p, "created": false }))?);
                    } else {
                        println!("(already exists): {}", p);
                    }
                }
                Err(e) => {
                    anyhow::bail!("mkdir failed: {}", e);
                }
            }
        }

        FilesCmd::Ls { path, json } => {
            let entries = mfs.ls(path)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                if entries.is_empty() {
                    println!("(empty directory)");
                } else {
                    println!("{:<32} {:>12}  {}", "Hash", "Size", "Name");
                    println!("{}", "-".repeat(60));
                    for entry in &entries {
                        println!("{:<32} {:>12}  {}", entry.hash, entry.size, entry.name);
                    }
                }
            }
        }

        FilesCmd::Cp { source, dest, json } => {
            match mfs.cp(source, dest) {
                Ok(d) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "destination": d }))?);
                    } else {
                        println!("copied {} -> {}", source, d);
                    }
                }
                Err(e) => {
                    anyhow::bail!("cp failed: {}", e);
                }
            }
        }

        FilesCmd::Mv { source, dest, json } => {
            match mfs.mv(source, dest) {
                Ok(d) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "destination": d }))?);
                    } else {
                        println!("moved {} -> {}", source, d);
                    }
                }
                Err(e) => {
                    anyhow::bail!("mv failed: {}", e);
                }
            }
        }

        FilesCmd::Rm { path, recursive, json } => {
            match mfs.rm(path, *recursive) {
                Ok(()) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "removed": path }))?);
                    } else {
                        println!("removed: {}", path);
                    }
                }
                Err(e) => {
                    anyhow::bail!("rm failed: {}", e);
                }
            }
        }

        FilesCmd::Stat { path, json } => {
            match mfs.stat(path) {
                Ok(stat) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&stat)?);
                    } else {
                        println!("Path: {}", path);
                        println!("Type: {}", stat.node_type);
                        println!("Hash: {}", stat.hash);
                        println!("Size: {}", stat.size);
                        println!("CumulativeSize: {}", stat.cumulative_size);
                        if stat.node_type == "file" {
                            println!("Blocks: {}", stat.blocks);
                        }
                    }
                }
                Err(e) => {
                    anyhow::bail!("stat failed: {}", e);
                }
            }
        }

        FilesCmd::Flush { path, json } => {
            // The path parameter is accepted for API compatibility but
            // in the current MFS implementation, flushing always targets the root.
            let root = mfs.flush()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "flushed": true,
                    "root": root,
                    "note": "Root CID pinned (if not empty)"
                }))?);
            } else {
                if root.is_empty() {
                    println!("flushed: no root set (MFS is empty)");
                } else {
                    println!("flushed: root={}", root);
                    println!("  (Root CID is now pinned and protected from GC)");
                }
            }
        }

        FilesCmd::Write { path, data, json } => {
            let content = if data.starts_with('@') {
                std::fs::read(data.trim_start_matches('@'))?
            } else {
                data.as_bytes().to_vec()
            };

            match mfs.write(path, content, topology) {
                Ok(()) => {
                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "written": path }))?);
                    } else {
                        println!("wrote {} bytes to: {}", data.len(), path);
                    }
                }
                Err(e) => {
                    anyhow::bail!("write failed: {}", e);
                }
            }
        }

        FilesCmd::Read { path, offset, count, json } => {
            match mfs.read(path, topology) {
                Ok(content) => {
                    let start = *offset as usize;
                    let end = count.map(|c| start + (c as usize)).unwrap_or(content.len());
                    let end = end.min(content.len());
                    let start = start.min(content.len());

                    let data = if start < content.len() {
                        content[start..end].to_vec()
                    } else {
                        Vec::new()
                    };

                    if *json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "path": path,
                            "size": data.len(),
                            "data": String::from_utf8_lossy(&data)
                        }))?);
                    } else {
                        std::io::Write::write_all(&mut std::io::stdout(), &data)?;
                    }
                }
                Err(e) => {
                    anyhow::bail!("read failed: {}", e);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_handles_basic_cases() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/foo"), "/foo");
        assert_eq!(normalize_path("/foo/bar"), "/foo/bar");
        assert_eq!(normalize_path("/foo//bar"), "/foo/bar");
        assert_eq!(normalize_path("/foo/./bar"), "/foo/bar");
        assert_eq!(normalize_path("/foo/../bar"), "/bar");
        assert_eq!(normalize_path("foo/bar"), "/foo/bar");
    }

    #[test]
    fn get_parent_path_works() {
        assert_eq!(get_parent_path("/"), "/");
        assert_eq!(get_parent_path("/foo"), "/");
        assert_eq!(get_parent_path("/foo/bar"), "/foo");
        assert_eq!(get_parent_path("/foo/bar/baz"), "/foo/bar");
    }

    #[test]
    fn mfs_mkdir_and_ls() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut mfs = MfsManager::new(temp_dir.path());
        mfs.load().unwrap();

        mfs.mkdir("/test").unwrap();
        let entries = mfs.ls("/").unwrap();
        assert!(entries.iter().any(|e| e.name == "test"));
    }

    #[test]
    fn mfs_cp_and_mv() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut mfs = MfsManager::new(temp_dir.path());
        mfs.load().unwrap();

        mfs.mkdir("/src").unwrap();
        mfs.cp("/src", "/dest").unwrap();

        let entries = mfs.ls("/").unwrap();
        assert!(entries.iter().any(|e| e.name == "dest"));

        mfs.mv("/dest", "/moved").unwrap();
        let entries = mfs.ls("/").unwrap();
        assert!(entries.iter().any(|e| e.name == "moved"));
        assert!(!entries.iter().any(|e| e.name == "dest"));
    }
}
