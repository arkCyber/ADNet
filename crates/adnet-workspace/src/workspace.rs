//! Workspace state — file manifest + shared/inbox/outbox folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Gossip / CDN room id for workspace file announcements.
pub const WORKSPACE_ROOM_ID: &str = "ExodusWorkSpace";

/// Subdirectories inside the workspace root.
pub const DIR_SHARED: &str = "shared";
pub const DIR_INBOX: &str = "inbox";
pub const DIR_OUTBOX: &str = "outbox";

/// One file entry in the workspace manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileEntry {
    pub name: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub content_hash: Option<String>,
    pub added_at: u64,
}

/// Workspace manifest persisted as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    pub root: String,
    pub node_id: String,
    pub files: Vec<WorkspaceFileEntry>,
    pub updated_at: u64,
}

/// Managed workspace paths and manifest.
pub struct Workspace {
    root: PathBuf,
    manifest_path: PathBuf,
    manifest: Mutex<WorkspaceManifest>,
}

impl Workspace {
    /// Open or create workspace under `app_data_dir/ExodusWorkSpace`.
    ///
    /// Note: the on-disk folder name stays `ExodusWorkSpace` for
    /// compatibility with peers using the original code.
    pub fn new(app_data_dir: &Path, node_id: impl Into<String>) -> Result<Self, String> {
        let root = app_data_dir.join("ExodusWorkSpace");
        for sub in [DIR_SHARED, DIR_INBOX, DIR_OUTBOX] {
            fs::create_dir_all(root.join(sub)).map_err(|e| format!("Create {sub}: {e}"))?;
        }
        let manifest_path = root.join("workspace.json");
        let node_id = node_id.into();
        let manifest = if manifest_path.exists() {
            let raw = fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
            serde_json::from_str(&raw).unwrap_or_else(|e| {
                warn!("workspace.json was invalid ({e}); reseeding");
                WorkspaceManifest {
                    root: root.to_string_lossy().to_string(),
                    node_id: node_id.clone(),
                    files: Vec::new(),
                    updated_at: current_timestamp(),
                }
            })
        } else {
            WorkspaceManifest {
                root: root.to_string_lossy().to_string(),
                node_id,
                files: Vec::new(),
                updated_at: current_timestamp(),
            }
        };
        let ws = Self {
            root,
            manifest_path,
            manifest: Mutex::new(manifest),
        };
        ws.save_manifest()?;
        Ok(ws)
    }

    /// Absolute path to workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn shared_dir(&self) -> PathBuf {
        self.root.join(DIR_SHARED)
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join(DIR_INBOX)
    }

    pub fn outbox_dir(&self) -> PathBuf {
        self.root.join(DIR_OUTBOX)
    }

    /// Copy a local file into `shared/` and register in the manifest.
    pub fn publish_file(
        &self,
        source: &Path,
        content_hash: Option<String>,
    ) -> Result<WorkspaceFileEntry, String> {
        if !source.is_file() {
            return Err(format!("Not a file: {}", source.display()));
        }
        let file_name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let dest = self.resolve_unique_path(&self.shared_dir(), &file_name);
        fs::copy(source, &dest).map_err(|e| format!("Copy to workspace: {e}"))?;
        let meta = fs::metadata(&dest).map_err(|e| e.to_string())?;
        let stored_name = dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file_name)
            .to_string();
        let rel = format!("{DIR_SHARED}/{stored_name}");
        let entry = WorkspaceFileEntry {
            name: stored_name,
            relative_path: rel,
            size_bytes: meta.len(),
            content_hash,
            added_at: current_timestamp(),
        };
        {
            let mut m = self.manifest.lock().map_err(|e| e.to_string())?;
            m.files.retain(|f| f.relative_path != entry.relative_path);
            m.files.push(entry.clone());
            m.updated_at = current_timestamp();
        }
        self.save_manifest()?;
        Ok(entry)
    }

    /// Drop a file from the manifest; the file itself is left on disk.
    pub fn unpublish(&self, relative_path: &str) -> Result<bool, String> {
        let mut m = self.manifest.lock().map_err(|e| e.to_string())?;
        let before = m.files.len();
        m.files.retain(|f| f.relative_path != relative_path);
        let changed = m.files.len() != before;
        if changed {
            m.updated_at = current_timestamp();
        }
        drop(m);
        if changed {
            self.save_manifest()?;
        }
        Ok(changed)
    }

    /// List manifest entries.
    pub fn list_files(&self) -> Result<Vec<WorkspaceFileEntry>, String> {
        let m = self.manifest.lock().map_err(|e| e.to_string())?;
        Ok(m.files.clone())
    }

    /// Full manifest snapshot for UI.
    pub fn manifest_snapshot(&self) -> Result<WorkspaceManifest, String> {
        let m = self.manifest.lock().map_err(|e| e.to_string())?;
        Ok(m.clone())
    }

    /// Resolve a unique destination path when `name` already exists.
    pub fn resolve_unique_path(&self, dir: &Path, name: &str) -> PathBuf {
        let dest = dir.join(name);
        if !dest.exists() {
            return dest;
        }
        let (stem, ext) = split_name_ext(name);
        let mut n = 1u32;
        loop {
            let candidate = if ext.is_empty() {
                dir.join(format!("{stem}_{n}"))
            } else {
                dir.join(format!("{stem}_{n}.{ext}"))
            };
            if !candidate.exists() {
                return candidate;
            }
            n += 1;
        }
    }

    fn save_manifest(&self) -> Result<(), String> {
        let m = self.manifest.lock().map_err(|e| e.to_string())?;
        let raw = serde_json::to_string_pretty(&*m).map_err(|e| e.to_string())?;
        fs::write(&self.manifest_path, raw).map_err(|e| e.to_string())
    }
}

/// Split a file name into (stem, ext) where ext excludes the dot.
///
/// Files without a dot, or whose dot is at position 0 (".hidden"), are
/// returned as `(name, "")`.
pub fn split_name_ext(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), e.to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn creates_workspace_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = Workspace::new(tmp.path(), "node-test").expect("workspace");
        assert!(ws.shared_dir().is_dir());
        assert!(ws.inbox_dir().is_dir());
        assert!(ws.outbox_dir().is_dir());
        let m = ws.manifest_snapshot().expect("snapshot");
        assert_eq!(m.node_id, "node-test");
        assert!(m.files.is_empty());
    }

    #[test]
    fn publish_file_updates_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = Workspace::new(tmp.path(), "n1").expect("workspace");
        let src = tmp.path().join("hello.txt");
        let mut f = fs::File::create(&src).expect("create");
        writeln!(f, "hello workspace").expect("write");
        let entry = ws
            .publish_file(&src, Some("abc123".into()))
            .expect("publish");
        assert!(entry.relative_path.starts_with("shared/"));
        let list = ws.list_files().expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].content_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn publish_duplicate_name_creates_unique_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = Workspace::new(tmp.path(), "n").expect("ws");
        let src = tmp.path().join("a.txt");
        fs::write(&src, b"first").unwrap();
        let e1 = ws.publish_file(&src, None).unwrap();
        // Same source published again — name collides on disk, must resolve.
        let e2 = ws.publish_file(&src, None).unwrap();
        assert_ne!(e1.relative_path, e2.relative_path);
        assert!(e2.relative_path.contains("a_1.txt"));
    }

    #[test]
    fn unpublish_drops_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = Workspace::new(tmp.path(), "n").expect("ws");
        let src = tmp.path().join("x.txt");
        fs::write(&src, b"x").unwrap();
        let e = ws.publish_file(&src, None).unwrap();
        assert!(ws.unpublish(&e.relative_path).unwrap());
        assert!(ws.list_files().unwrap().is_empty());
        // Unpublishing again is a no-op.
        assert!(!ws.unpublish(&e.relative_path).unwrap());
    }

    #[test]
    fn split_name_ext_handles_dotted_names() {
        assert_eq!(split_name_ext("a.txt"), ("a".into(), "txt".into()));
        assert_eq!(
            split_name_ext("archive.tar.gz"),
            ("archive.tar".into(), "gz".into())
        );
        assert_eq!(split_name_ext("noext"), ("noext".into(), "".into()));
        assert_eq!(split_name_ext(".hidden"), (".hidden".into(), "".into()));
    }

    #[test]
    fn workspace_root_isolated_per_app_data_dir() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ws_a = Workspace::new(a.path(), "id-a").unwrap();
        let ws_b = Workspace::new(b.path(), "id-b").unwrap();
        assert_eq!(ws_a.manifest_snapshot().unwrap().node_id, "id-a");
        assert_eq!(ws_b.manifest_snapshot().unwrap().node_id, "id-b");
    }

    #[test]
    fn topic_follows_adnet_room_convention() {
        use crate::workspace_room_topic;
        assert_eq!(workspace_room_topic(), "adnet-room-ExodusWorkSpace");
    }
}
