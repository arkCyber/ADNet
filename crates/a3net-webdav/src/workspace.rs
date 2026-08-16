//! Workspace namespace adapter for WebDAV.
//!
//! This module bridges `a3net-workspace` (shared/inbox/outbox folders)
//! with the WebDAV server, allowing Finder/Explorer to browse and
//! manipulate workspace files over the WebDAV protocol.
//!
//! The adapter maps the workspace directory structure to WebDAV paths:
//! - `/workspace/shared/` → `shared/` folder
//! - `/workspace/inbox/`  → `inbox/` folder
//! - `/workspace/outbox/` → `outbox/` folder

use std::path::PathBuf;

use a3net_workspace::{Workspace, WorkspaceFileEntry, WorkspaceManifest};

/// Workspace namespace that maps workspace paths to WebDAV paths.
/// 
/// WebDAV paths:
/// - `GET /workspace/shared/<file>` → read file from shared folder
/// - `PUT /workspace/shared/<file>` → upload to shared folder  
/// - `GET /workspace/inbox/<file>` → read file from inbox
/// - `GET /workspace/outbox/<file>` → read file from outbox
pub struct WorkspaceNamespace {
    workspace: Workspace,
}

impl WorkspaceNamespace {
    /// Create a new workspace namespace from a workspace.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    /// Open a workspace at the given data directory.
    pub fn open(data_dir: &PathBuf, node_id: &str) -> Result<Self, String> {
        let workspace = Workspace::new(data_dir, node_id)?;
        Ok(Self { workspace })
    }

    /// Get the underlying workspace.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// List files in a folder (shared, inbox, or outbox).
    pub fn list_folder(&self, folder: &str) -> Result<Vec<WorkspaceFileEntry>, String> {
        let manifest = self.workspace.manifest_snapshot()?;
        let prefix = format!("{}/", folder);
        let files: Vec<_> = manifest.files
            .iter()
            .filter(|e| e.relative_path.starts_with(&prefix))
            .cloned()
            .collect();
        Ok(files)
    }

    /// List all files in the workspace.
    pub fn list_all(&self) -> Result<Vec<WorkspaceFileEntry>, String> {
        self.workspace.list_files()
    }

    /// Get a file entry by relative path.
    pub fn get_file(&self, relative_path: &str) -> Option<WorkspaceFileEntry> {
        let manifest = self.workspace.manifest_snapshot().ok()?;
        manifest.files.into_iter().find(|e| e.relative_path == relative_path)
    }

    /// Publish a file to the workspace shared folder.
    pub fn publish(&self, source: &PathBuf, content_hash: Option<String>) -> Result<WorkspaceFileEntry, String> {
        self.workspace.publish_file(source, content_hash)
    }

    /// Unpublish a file from the workspace.
    pub fn unpublish(&self, relative_path: &str) -> Result<bool, String> {
        self.workspace.unpublish(relative_path)
    }

    /// Get the local file path for a workspace entry.
    pub fn local_path(&self, relative_path: &str) -> Option<PathBuf> {
        let full_path = self.workspace.root().join(relative_path);
        if full_path.is_file() {
            Some(full_path)
        } else {
            None
        }
    }
}

/// Convert a WebDAV path to a workspace relative path.
/// 
/// Examples:
/// - `/workspace/shared/doc.pdf` → `shared/doc.pdf`
/// - `/workspace/inbox/file.txt` → `inbox/file.txt`
pub fn webdav_to_workspace(webdav_path: &str) -> Option<String> {
    let path = webdav_path.trim_start_matches('/');
    if path.starts_with("workspace/") {
        Some(path.strip_prefix("workspace/")?.to_string())
    } else {
        None
    }
}

/// Convert a workspace relative path to a WebDAV path.
pub fn workspace_to_webdav(relative_path: &str) -> String {
    format!("/workspace/{}", relative_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webdav_to_workspace() {
        assert_eq!(
            webdav_to_workspace("/workspace/shared/doc.pdf"),
            Some("shared/doc.pdf".to_string())
        );
        assert_eq!(
            webdav_to_workspace("/workspace/inbox/file.txt"),
            Some("inbox/file.txt".to_string())
        );
        assert_eq!(
            webdav_to_workspace("/other/path"),
            None
        );
    }

    #[test]
    fn test_workspace_to_webdav() {
        assert_eq!(
            workspace_to_webdav("shared/doc.pdf"),
            "/workspace/shared/doc.pdf".to_string()
        );
    }
}
