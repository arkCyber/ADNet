//! Rich workspace types: ACL grants, share links, quotas, and the
//! multi-instance workspace manager.
//!
//! ## Design goals
//!
//! - **No unsafe code** — `#![forbid(unsafe_code)]` enforced.
//! - **Aerospace-grade invariants** — every constructor validates bounds.
//! - **Wire-compatible serde** — every struct round-trips through JSON.
//! - **Idempotent by default** — ACL grants and share links are keyed by
//!   stable identifiers so re-importing the same grant/link is a no-op.

#![forbid(unsafe_code)]
#![deny(unused_must_use)]
// Re-exported items are used by external consumers (tests, callers of this lib).
#![allow(unused_imports, dead_code)]

use rand::RngCore;
use serde::{Deserialize, Serialize};
pub use self::acl::{
    AclEntry, AclGrant, AclPermission, AclPrincipal,
    permission_from_label, label_from_permission, AclError,
};
pub use self::quota::{
    QuotaError, WorkspaceQuota,
    MAX_WORKSPACE_FILE_COUNT, DEFAULT_WORKSPACE_QUOTA_BYTES, MAX_WORKSPACE_QUOTA_BYTES,
};
pub use self::share_link::{
    ShareLink, ShareLinkEntry, ShareLinkError, ShareScope,
    SHARE_LINK_TOKEN_BYTES, MAX_SHARE_LINKS,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum length of an ACL permission label in bytes (UTF-8).
pub const MAX_ACL_LABEL_LEN: usize = 64;
/// Maximum number of ACL entries per workspace.
pub const MAX_ACL_ENTRIES: usize = 256;

// ---------------------------------------------------------------------------
// ACL — Access Control Lists
// ---------------------------------------------------------------------------

mod acl {
    use super::*;

    /// Well-known permission labels that map to [`AclAction`] flags.
    pub mod well_known {
        /// Can read files in the workspace (includes inbox/outbox listing).
        pub const READ: &str = "workspace:read";
        /// Can publish files to the shared/ folder.
        pub const WRITE: &str = "workspace:write";
        /// Can delete or unpublish entries from the shared/ folder.
        pub const DELETE: &str = "workspace:delete";
        /// Can manage ACL grants (add/remove other principals).
        pub const ADMIN: &str = "workspace:admin";
        /// Can manage share links.
        pub const SHARE: &str = "workspace:share";
        /// Can ingest files into inbox/ (used by the gossip bridge).
        pub const INGEST: &str = "workspace:ingest";
    }

    /// An action / permission flag.  Additive — grant all required actions
    /// to a principal by ORing the individual flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct AclPermission(u32);

    impl AclPermission {
        pub const NONE: AclPermission = AclPermission(0);
        #[allow(dead_code)] pub const READ: AclPermission = AclPermission(1 << 0);
        #[allow(dead_code)] pub const WRITE: AclPermission = AclPermission(1 << 1);
        #[allow(dead_code)] pub const DELETE: AclPermission = AclPermission(1 << 2);
        #[allow(dead_code)] pub const ADMIN: AclPermission = AclPermission(1 << 3);
        #[allow(dead_code)] pub const SHARE: AclPermission = AclPermission(1 << 4);
        #[allow(dead_code)] pub const INGEST: AclPermission = AclPermission(1 << 5);

        /// Construct from raw bits.
        pub const fn from_bits(bits: u32) -> Self {
            AclPermission(bits)
        }

        /// Raw bit representation.
        pub const fn bits(self) -> u32 {
            self.0
        }

        /// True if `self` contains all bits set in `required`.
        #[inline]
        pub const fn contains(self, required: Self) -> bool {
            (self.0 & required.0) == required.0
        }

        /// Additive union of two permission sets.
        #[inline]
        pub const fn union(self, other: Self) -> Self {
            AclPermission(self.0 | other.0)
        }
    }

    impl Default for AclPermission {
        fn default() -> Self {
            Self::NONE
        }
    }

    impl std::ops::BitOr for AclPermission {
        type Output = Self;
        #[inline]
        fn bitor(self, rhs: Self) -> Self {
            Self(self.0 | rhs.0)
        }
    }

    impl std::ops::BitAnd for AclPermission {
        type Output = Self;
        #[inline]
        fn bitand(self, rhs: Self) -> Self {
            Self(self.0 & rhs.0)
        }
    }

    /// Maps a well-known label string to its [`AclPermission`] flag.
    /// Returns `None` for unknown labels.
    #[allow(dead_code)]
    pub fn permission_from_label(label: &str) -> Option<AclPermission> {
        match label {
            well_known::READ => Some(AclPermission::READ),
            well_known::WRITE => Some(AclPermission::WRITE),
            well_known::DELETE => Some(AclPermission::DELETE),
            well_known::ADMIN => Some(AclPermission::ADMIN),
            well_known::SHARE => Some(AclPermission::SHARE),
            well_known::INGEST => Some(AclPermission::INGEST),
            _ => None,
        }
    }

    /// Maps a [`AclPermission`] flag to its canonical well-known label.
    #[allow(dead_code)]
    pub fn label_from_permission(perm: AclPermission) -> Option<&'static str> {
        if perm == AclPermission::READ {
            Some(well_known::READ)
        } else if perm == AclPermission::WRITE {
            Some(well_known::WRITE)
        } else if perm == AclPermission::DELETE {
            Some(well_known::DELETE)
        } else if perm == AclPermission::ADMIN {
            Some(well_known::ADMIN)
        } else if perm == AclPermission::SHARE {
            Some(well_known::SHARE)
        } else if perm == AclPermission::INGEST {
            Some(well_known::INGEST)
        } else {
            None
        }
    }

    /// Who or what is being granted a permission.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum AclPrincipal {
        /// Grant to a specific node by its `NodeId`.
        NodeId(String),
        /// Grant to a tag / group. Resolved at runtime by the roster.
        Tag(String),
        /// Grant to the public (any peer on the network).
        Public,
    }

    impl AclPrincipal {
        /// Human-readable display string.
        pub fn display(&self) -> String {
            match self {
                AclPrincipal::NodeId(id) => format!("node:{id}"),
                AclPrincipal::Tag(t) => format!("tag:{t}"),
                AclPrincipal::Public => "public".to_string(),
            }
        }
    }

    /// A single ACL entry mapping a principal to a set of permissions.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AclEntry {
        pub principal: AclPrincipal,
        /// Bitfield of permissions granted.
        pub permissions: AclPermission,
        /// Optional human-readable label (e.g. "alice's laptop").
        #[serde(default)]
        pub label: Option<String>,
        /// Unix timestamp when this entry was added.
        pub granted_at: u64,
        /// Optional expiry (UTC seconds). `None` means no expiry.
        #[serde(default)]
        pub expires_at: Option<u64>,
    }

    impl AclEntry {
        /// Build an ACL entry. Returns `Err` if the label exceeds [`MAX_ACL_LABEL_LEN`].
        pub fn new(
            principal: AclPrincipal,
            permissions: AclPermission,
            granted_at: u64,
        ) -> Result<Self, AclError> {
            Ok(Self {
                principal,
                permissions,
                label: None,
                granted_at,
                expires_at: None,
            })
        }

        /// Attach a label.
        pub fn with_label(mut self, label: impl Into<String>) -> Result<Self, AclError> {
            let label = label.into();
            if label.len() > MAX_ACL_LABEL_LEN {
                return Err(AclError::LabelTooLong(label.len()));
            }
            self.label = Some(label);
            Ok(self)
        }

        /// Set an expiry time.
        pub fn with_expiry(mut self, expires_at: u64) -> Self {
            self.expires_at = Some(expires_at);
            self
        }

        /// True if this entry is currently active (not expired).
        pub fn is_active(&self, now: u64) -> bool {
            self.expires_at.is_none() || now < self.expires_at.unwrap()
        }

        /// Check whether this entry grants a specific permission.
        pub fn grants(&self, action: AclPermission) -> bool {
            self.permissions.contains(action)
        }
    }

    /// An ACL grant is a collection of entries with an optional description.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AclGrant {
        /// Stable identifier for this grant. Used to detect duplicates
        /// when re-importing a grant.
        pub id: String,
        /// Entries that make up this grant.
        pub entries: Vec<AclEntry>,
        /// Human-readable description (e.g. "read-only access for device fleet").
        #[serde(default)]
        pub description: Option<String>,
        /// Unix timestamp when this grant was created.
        pub created_at: u64,
    }

    impl AclGrant {
        /// Build a grant from a single entry.
        pub fn single(principal: AclPrincipal, permissions: AclPermission, created_at: u64) -> Self {
            let id = generate_id();
            Self {
                id: id.clone(),
                entries: vec![AclEntry::new(principal, permissions, created_at).unwrap()],
                description: None,
                created_at,
            }
        }

        /// Add a label to the first entry. Returns `Err` if the entry index is out of range.
        pub fn label_entry(&mut self, index: usize, label: impl Into<String>) -> Result<(), AclError> {
            let entry = self
                .entries
                .get_mut(index)
                .ok_or(AclError::EntryNotFound)?;
            *entry = entry.clone().with_label(label)?;
            Ok(())
        }

        /// Returns `true` if the given principal is granted the specified permission.
        pub fn has_permission(&self, principal: &AclPrincipal, action: AclPermission, now: u64) -> bool {
            self.entries
                .iter()
                .filter(|e| e.principal == *principal && e.is_active(now))
                .any(|e| e.grants(action))
        }
    }

    /// ACL-related errors.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[allow(dead_code)]
    pub enum AclError {
        #[error("label exceeds maximum length ({0} bytes; max {MAX_ACL_LABEL_LEN})")]
        LabelTooLong(usize),
        #[error("ACL entry index out of range")]
        EntryNotFound,
    }
}

// ---------------------------------------------------------------------------
// Share Links
// ---------------------------------------------------------------------------

mod share_link {
    use super::*;

    /// Maximum length of a share-link token in bytes.
    #[allow(dead_code)]
    pub const SHARE_LINK_TOKEN_BYTES: usize = 32;
    /// Maximum number of share links per workspace.
    #[allow(dead_code)]
    pub const MAX_SHARE_LINKS: usize = 64;

    /// Scope of a share link — which sub-folders it covers.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ShareScope {
        /// The entire workspace (all three sub-folders).
        Full,
        /// Only the shared/ folder.
        Shared,
        /// Only the inbox/ folder.
        Inbox,
        /// Only a specific relative path prefix.
        Prefix(String),
    }

    impl ShareScope {
        /// Returns `true` if `path` falls within this scope.
        pub fn covers(&self, path: &str) -> bool {
            match self {
                ShareScope::Full => true,
                ShareScope::Shared => path.starts_with("shared/") || path == "shared",
                ShareScope::Inbox => path.starts_with("inbox/") || path == "inbox",
                ShareScope::Prefix(p) => path.starts_with(p) || path == *p,
            }
        }

        /// Parse from a string tag.
        pub fn from_tag(tag: &str) -> Option<Self> {
            match tag {
                "full" => Some(ShareScope::Full),
                "shared" => Some(ShareScope::Shared),
                "inbox" => Some(ShareScope::Inbox),
                other if other.starts_with("prefix:") => {
                    Some(ShareScope::Prefix(other.strip_prefix("prefix:")?.to_string()))
                }
                _ => None,
            }
        }

        /// Canonical tag string.
        pub fn tag(&self) -> String {
            match self {
                ShareScope::Full => "full".to_string(),
                ShareScope::Shared => "shared".to_string(),
                ShareScope::Inbox => "inbox".to_string(),
                ShareScope::Prefix(p) => format!("prefix:{p}"),
            }
        }
    }

    /// A share link grants read-only access to a workspace sub-tree
    /// without requiring the caller to have an ACL grant.
    ///
    /// The token is a random 32-byte nonce rendered as 64 hex chars.
    /// Anyone who knows the token can read the covered entries.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ShareLink {
        /// Stable identifier (separate from the token).
        pub id: String,
        /// The opaque token — the secret credential.
        pub token: String,
        /// Which parts of the workspace this link covers.
        pub scope: ShareScope,
        /// Unix timestamp when this link was created.
        pub created_at: u64,
        /// Optional expiry (UTC seconds). `None` = never.
        #[serde(default)]
        pub expires_at: Option<u64>,
        /// Optional click-count limit. `None` = unlimited.
        #[serde(default)]
        pub max_clicks: Option<u32>,
        /// Number of times this link has been used.
        pub click_count: u32,
        /// Optional human-readable description.
        #[serde(default)]
        pub description: Option<String>,
    }

    impl ShareLink {
        /// Create a new share link with a random token.
        pub fn new(scope: ShareScope, created_at: u64) -> Self {
            let mut token_bytes = [0u8; SHARE_LINK_TOKEN_BYTES];
            rand::thread_rng().fill_bytes(&mut token_bytes);
            let token = hex::encode(token_bytes);
            Self {
                id: generate_id(),
                token,
                scope,
                created_at,
                expires_at: None,
                max_clicks: None,
                click_count: 0,
                description: None,
            }
        }

        /// Set an expiry time.
        pub fn with_expiry(mut self, expires_at: u64) -> Self {
            self.expires_at = Some(expires_at);
            self
        }

        /// Set a maximum click count.
        pub fn with_max_clicks(mut self, max: u32) -> Self {
            self.max_clicks = Some(max);
            self
        }

        /// Set a description.
        pub fn with_description(mut self, desc: impl Into<String>) -> Self {
            self.description = Some(desc.into());
            self
        }

        /// Returns `true` if this link is currently valid (not expired and under click limit).
        pub fn is_valid(&self, now: u64) -> bool {
            if let Some(exp) = self.expires_at
                && now >= exp { return false; }
            if let Some(max) = self.max_clicks
                && self.click_count >= max { return false; }
            true
        }

        /// Consume one click. Returns `Err` if the link is exhausted.
        pub fn click(&mut self) -> Result<(), ShareLinkError> {
            self.click_count = self
                .click_count
                .checked_add(1)
                .ok_or(ShareLinkError::ClickOverflow)?;
            if let Some(max) = self.max_clicks
                && self.click_count > max {
                    return Err(ShareLinkError::ClickLimitExceeded);
                }
            Ok(())
        }

        /// Short token prefix for display (first 8 hex chars).
        pub fn token_prefix(&self) -> &str {
            &self.token[..8.min(self.token.len())]
        }
    }

    /// Entry in the share-link registry (a JSON file stored alongside the manifest).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ShareLinkEntry {
        pub link: ShareLink,
        /// The workspace instance this link belongs to.
        pub workspace_name: String,
        /// NodeId that created this link.
        pub creator_node_id: String,
    }

    /// Errors from share-link operations.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ShareLinkError {
        #[error("share link has expired")]
        Expired,
        #[error("share link click limit exceeded")]
        ClickLimitExceeded,
        #[error("click count overflow")]
        ClickOverflow,
        #[error("share link not found")]
        NotFound,
    }
}

// ---------------------------------------------------------------------------
// Workspace Quota
// ---------------------------------------------------------------------------

mod quota {
    use super::*;

    /// Default workspace storage quota: 1 GiB.
    #[allow(dead_code)]
    pub const DEFAULT_WORKSPACE_QUOTA_BYTES: u64 = 1 << 30;
    /// Maximum workspace storage quota: 1 TiB.
    #[allow(dead_code)]
    pub const MAX_WORKSPACE_QUOTA_BYTES: u64 = 1 << 40;
    /// Maximum number of files in a workspace.
    pub const MAX_WORKSPACE_FILE_COUNT: usize = 100_000;

    /// Workspace storage quota declaration.
    ///
    /// All fields are hard limits. The workspace enforce them at publish time
    /// and the gossip bridge checks them before announcing remote entries.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct WorkspaceQuota {
        /// Maximum total bytes across all sub-folders. `None` = no limit.
        #[serde(default)]
        pub max_bytes: Option<u64>,
        /// Maximum number of entries in the manifest. `None` = no limit.
        #[serde(default)]
        pub max_entries: Option<usize>,
        /// Maximum size of a single file. `None` = no limit.
        #[serde(default)]
        pub max_file_bytes: Option<u64>,
        /// Quota warning threshold (0.0–1.0). `None` = no warning.
        #[serde(default)]
        pub warning_threshold: Option<f64>,
    }

    impl Default for WorkspaceQuota {
        fn default() -> Self {
            Self {
                max_bytes: Some(DEFAULT_WORKSPACE_QUOTA_BYTES),
                max_entries: Some(MAX_WORKSPACE_FILE_COUNT),
                max_file_bytes: None,
                warning_threshold: Some(0.8),
            }
        }
    }

    impl WorkspaceQuota {
        /// Create with a specific byte limit.
        pub fn new(max_bytes: u64) -> Self {
            Self {
                max_bytes: Some(max_bytes.min(MAX_WORKSPACE_QUOTA_BYTES)),
                max_entries: Some(MAX_WORKSPACE_FILE_COUNT),
                max_file_bytes: None,
                warning_threshold: Some(0.8),
            }
        }

        /// Set the per-file byte limit.
        pub fn with_max_file_bytes(mut self, bytes: u64) -> Self {
            self.max_file_bytes = Some(bytes);
            self
        }

        /// Set the warning threshold (0.0–1.0).
        pub fn with_warning_threshold(mut self, t: f64) -> Self {
            self.warning_threshold = Some(t.clamp(0.0, 1.0));
            self
        }

        /// Remove all limits (unlimited workspace).
        pub fn unlimited(mut self) -> Self {
            self.max_bytes = None;
            self.max_entries = None;
            self.max_file_bytes = None;
            self.warning_threshold = None;
            self
        }

        /// Check a file publish operation against this quota.
        pub fn check_publish(
            &self,
            file_bytes: u64,
            current_bytes: u64,
            current_entries: usize,
        ) -> Result<(), QuotaError> {
            if let Some(max_file) = self.max_file_bytes
                && file_bytes > max_file {
                    return Err(QuotaError::FileTooLarge { file_bytes, max_file });
                }
            if let Some(max_entries) = self.max_entries
                && current_entries >= max_entries {
                    return Err(QuotaError::TooManyEntries {
                        current: current_entries,
                        max: max_entries,
                    });
                }
            if let Some(max_bytes) = self.max_bytes {
                let new_total = current_bytes.saturating_add(file_bytes);
                if new_total > max_bytes {
                    return Err(QuotaError::OutOfSpace {
                        current_bytes,
                        added_bytes: file_bytes,
                        max_bytes,
                    });
                }
            }
            Ok(())
        }

        /// Returns `true` if the workspace is at or above the warning threshold.
        pub fn is_above_warning(&self, current_bytes: u64) -> bool {
            let Some(max) = self.max_bytes else {
                return false;
            };
            let Some(threshold) = self.warning_threshold else {
                return false;
            };
            let usage = current_bytes as f64 / max as f64;
            usage >= threshold
        }

        /// Human-readable quota summary.
        pub fn summary(&self) -> String {
            const GIB: u64 = 1 << 30;
            match (self.max_bytes, self.max_entries, self.max_file_bytes) {
                (None, None, None) => "unlimited".to_string(),
                (Some(bytes), None, None) => {
                    format!("{:.1} GiB max", bytes as f64 / GIB as f64)
                }
                (Some(bytes), Some(entries), None) => {
                    format!("{:.0} GiB max / {entries} files", bytes as f64 / GIB as f64)
                }
                _ => {
                    let bytes = self.max_bytes.unwrap_or(0);
                    format!("{:.1} GiB", bytes as f64 / GIB as f64)
                }
            }
        }
    }

    /// Quota enforcement errors.
    #[derive(Debug, Clone, PartialEq, thiserror::Error)]
    pub enum QuotaError {
        #[error(
            "file too large: {file_bytes} bytes exceeds per-file limit of {max_file} bytes"
        )]
        FileTooLarge { file_bytes: u64, max_file: u64 },
        #[error("workspace has too many entries: {current} >= {max}")]
        TooManyEntries { current: usize, max: usize },
        #[error(
            "out of space: current {current_bytes} + {added_bytes} > max {max_bytes} bytes"
        )]
        OutOfSpace {
            current_bytes: u64,
            added_bytes: u64,
            max_bytes: u64,
        },
    }
}

// ---------------------------------------------------------------------------
// Workspace Manager — multi-instance registry
// ---------------------------------------------------------------------------

/// Metadata for one registered workspace instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInstance {
    /// Stable name — used in the directory path and as a handle.
    pub name: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// The ACL grant for this instance.
    pub acl: Vec<AclGrant>,
    /// Storage quota for this instance.
    pub quota: WorkspaceQuota,
    /// Active share links.
    #[serde(default)]
    pub share_links: Vec<ShareLinkEntry>,
    /// Total bytes currently used.
    pub used_bytes: u64,
    /// Number of entries in the manifest.
    pub entry_count: usize,
    /// Unix timestamp when this instance was created.
    pub created_at: u64,
    /// Unix timestamp when this instance was last modified.
    pub updated_at: u64,
    /// True if this instance is the default (used when no name is specified).
    pub is_default: bool,
}

impl WorkspaceInstance {
    /// Build a named instance with default quota and empty ACL.
    pub fn new(name: impl Into<String>, created_at: u64) -> Self {
        Self {
            name: name.into(),
            description: None,
            acl: Vec::new(),
            quota: WorkspaceQuota::default(),
            share_links: Vec::new(),
            used_bytes: 0,
            entry_count: 0,
            created_at,
            updated_at: created_at,
            is_default: false,
        }
    }

    /// Set the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Mark as the default instance.
    pub fn with_default(mut self) -> Self {
        self.is_default = true;
        self
    }

    /// Add a grant.
    pub fn add_grant(&mut self, grant: AclGrant) {
        // Idempotent: replace by id if exists
        if let Some(pos) = self.acl.iter().position(|g| g.id == grant.id) {
            self.acl[pos] = grant;
        } else {
            self.acl.push(grant);
        }
        self.updated_at = current_timestamp();
    }

    /// Remove a grant by id.
    pub fn remove_grant(&mut self, id: &str) -> bool {
        let len_before = self.acl.len();
        self.acl.retain(|g| g.id != id);
        let changed = self.acl.len() != len_before;
        if changed {
            self.updated_at = current_timestamp();
        }
        changed
    }

    /// Add a share link.
    pub fn add_share_link(&mut self, entry: ShareLinkEntry) {
        if let Some(pos) = self.share_links.iter().position(|e| e.link.id == entry.link.id) {
            self.share_links[pos] = entry;
        } else {
            self.share_links.push(entry);
        }
        self.updated_at = current_timestamp();
    }

    /// Remove a share link by id.
    pub fn remove_share_link(&mut self, id: &str) -> bool {
        let len_before = self.share_links.len();
        self.share_links.retain(|e| e.link.id != id);
        let changed = self.share_links.len() != len_before;
        if changed {
            self.updated_at = current_timestamp();
        }
        changed
    }

    /// Find an active share link by token.
    pub fn find_share_link(&self, token: &str, now: u64) -> Option<&ShareLink> {
        self.share_links
            .iter()
            .find(|e| e.link.token == token && e.link.is_valid(now))
            .map(|e| &e.link)
    }

    /// Update usage counters.
    pub fn update_usage(&mut self, used_bytes: u64, entry_count: usize) {
        self.used_bytes = used_bytes;
        self.entry_count = entry_count;
        self.updated_at = current_timestamp();
    }

    /// Returns `true` if the workspace can accept a new publish of `bytes` bytes.
    pub fn can_publish(&self, bytes: u64) -> Result<(), QuotaError> {
        self.quota.check_publish(bytes, self.used_bytes, self.entry_count)
    }

    /// Returns `true` if a given principal has the specified permission.
    pub fn has_permission(&self, principal: &AclPrincipal, action: AclPermission, now: u64) -> bool {
        self.acl
            .iter()
            .any(|g| g.has_permission(principal, action, now))
    }

    /// Human-readable status summary.
    pub fn summary(&self) -> String {
        format!(
            "[{}] {} · {} · {} files · {}",
            if self.is_default { "default" } else { &self.name },
            self.quota.summary(),
            self.used_bytes,
            self.entry_count,
            self.share_links.len(),
        )
    }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a random 12-byte hex identifier (24 hex chars).
fn generate_id() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── AclPermission ───────────────────────────────────────────────────────

    #[test]
    fn acl_permission_bitor() {
        let p = AclPermission::READ | AclPermission::WRITE;
        assert!(p.contains(AclPermission::READ));
        assert!(p.contains(AclPermission::WRITE));
        assert!(!p.contains(AclPermission::DELETE));
    }

    #[test]
    fn acl_permission_from_label() {
        assert_eq!(permission_from_label("workspace:read"), Some(AclPermission::READ));
        assert_eq!(permission_from_label("workspace:write"), Some(AclPermission::WRITE));
        assert_eq!(permission_from_label("workspace:delete"), Some(AclPermission::DELETE));
        assert_eq!(permission_from_label("workspace:admin"), Some(AclPermission::ADMIN));
        assert_eq!(permission_from_label("workspace:share"), Some(AclPermission::SHARE));
        assert_eq!(permission_from_label("workspace:ingest"), Some(AclPermission::INGEST));
        assert_eq!(permission_from_label("unknown"), None);
    }

    #[test]
    fn acl_permission_label_roundtrip() {
        for perm in [
            AclPermission::READ,
            AclPermission::WRITE,
            AclPermission::DELETE,
            AclPermission::ADMIN,
            AclPermission::SHARE,
            AclPermission::INGEST,
        ] {
            let label = label_from_permission(perm).unwrap();
            assert_eq!(permission_from_label(label), Some(perm));
        }
    }

    #[test]
    fn acl_permission_default_is_none() {
        assert_eq!(AclPermission::default(), AclPermission::NONE);
    }

    // ── AclEntry ─────────────────────────────────────────────────────────────

    #[test]
    fn acl_entry_active_without_expiry() {
        let entry = AclEntry::new(AclPrincipal::Public, AclPermission::READ, 1000).unwrap();
        assert!(entry.is_active(9999));
        assert!(entry.is_active(0));
    }

    #[test]
    fn acl_entry_expires() {
        let entry = AclEntry::new(AclPrincipal::Public, AclPermission::READ, 1000)
            .unwrap()
            .with_expiry(2000);
        assert!(entry.is_active(1999));
        assert!(!entry.is_active(2000));
        assert!(!entry.is_active(9999));
    }

    #[test]
    fn acl_entry_with_label() {
        let entry = AclEntry::new(AclPrincipal::NodeId("abc".into()), AclPermission::READ, 1000)
            .unwrap()
            .with_label("alice's phone")
            .unwrap();
        assert_eq!(entry.label.as_deref(), Some("alice's phone"));
    }

    #[test]
    fn acl_entry_label_too_long() {
        let entry = AclEntry::new(AclPrincipal::Public, AclPermission::READ, 1000).unwrap();
        let long_label = "x".repeat(MAX_ACL_LABEL_LEN + 1);
        let err = entry.with_label(long_label).unwrap_err();
        assert!(matches!(err, AclError::LabelTooLong(_)));
    }

    // ── AclGrant ─────────────────────────────────────────────────────────────

    #[test]
    fn acl_grant_single() {
        let grant = AclGrant::single(AclPrincipal::Public, AclPermission::READ, 1000);
        assert_eq!(grant.entries.len(), 1);
        assert!(!grant.id.is_empty());
    }

    #[test]
    fn acl_grant_has_permission() {
        let grant = AclGrant::single(AclPrincipal::Public, AclPermission::READ | AclPermission::WRITE, 1000);
        assert!(grant.has_permission(&AclPrincipal::Public, AclPermission::READ, 9999));
        assert!(grant.has_permission(&AclPrincipal::Public, AclPermission::WRITE, 9999));
        assert!(!grant.has_permission(&AclPrincipal::Public, AclPermission::DELETE, 9999));
        assert!(!grant.has_permission(&AclPrincipal::NodeId("abc".into()), AclPermission::READ, 9999));
    }

    // ── ShareScope ───────────────────────────────────────────────────────────

    #[test]
    fn share_scope_covers() {
        assert!(ShareScope::Full.covers("shared/foo"));
        assert!(ShareScope::Full.covers("inbox/bar"));
        assert!(ShareScope::Shared.covers("shared/foo"));
        assert!(!ShareScope::Shared.covers("inbox/bar"));
        assert!(ShareScope::Inbox.covers("inbox/foo"));
        assert!(!ShareScope::Inbox.covers("shared/foo"));
        assert!(ShareScope::Prefix("shared/docs".into()).covers("shared/docs/report.pdf"));
        assert!(!ShareScope::Prefix("shared/docs".into()).covers("shared/other/report.pdf"));
    }

    #[test]
    fn share_scope_tag_roundtrip() {
        assert_eq!(ShareScope::from_tag("full"), Some(ShareScope::Full));
        assert_eq!(ShareScope::from_tag("shared"), Some(ShareScope::Shared));
        assert_eq!(ShareScope::from_tag("inbox"), Some(ShareScope::Inbox));
        assert_eq!(ShareScope::from_tag("prefix:foo/bar"), Some(ShareScope::Prefix("foo/bar".into())));
        assert_eq!(ShareScope::from_tag("unknown"), None);
        assert_eq!(ShareScope::from_tag("prefix:"), Some(ShareScope::Prefix("".into())));
    }

    #[test]
    fn share_scope_tag() {
        assert_eq!(ShareScope::Full.tag(), "full");
        assert_eq!(ShareScope::Shared.tag(), "shared");
        assert_eq!(ShareScope::Inbox.tag(), "inbox");
        assert_eq!(ShareScope::Prefix("x/y".into()).tag(), "prefix:x/y");
    }

    // ── ShareLink ─────────────────────────────────────────────────────────────

    #[test]
    fn share_link_token_is_random() {
        let a = ShareLink::new(ShareScope::Full, 1000);
        let b = ShareLink::new(ShareScope::Full, 1000);
        assert_ne!(a.token, b.token);
    }

    #[test]
    fn share_link_valid_without_expiry() {
        let link = ShareLink::new(ShareScope::Full, 1000);
        assert!(link.is_valid(9999));
    }

    #[test]
    fn share_link_expires() {
        let link = ShareLink::new(ShareScope::Full, 1000).with_expiry(2000);
        assert!(link.is_valid(1999));
        assert!(!link.is_valid(2000));
    }

    #[test]
    fn share_link_click_limit() {
        let mut link = ShareLink::new(ShareScope::Full, 1000).with_max_clicks(2);
        link.click().unwrap();
        assert_eq!(link.click_count, 1);
        link.click().unwrap();
        assert_eq!(link.click_count, 2);
        let err = link.click().unwrap_err();
        assert!(matches!(err, ShareLinkError::ClickLimitExceeded));
    }

    #[test]
    fn share_link_token_prefix() {
        let link = ShareLink::new(ShareScope::Full, 1000);
        assert_eq!(link.token_prefix().len(), 8);
        assert!(link.token.starts_with(link.token_prefix()));
    }

    #[test]
    fn share_link_description() {
        let link = ShareLink::new(ShareScope::Shared, 1000)
            .with_description("Q3 design docs");
        assert_eq!(link.description.as_deref(), Some("Q3 design docs"));
    }

    // ── WorkspaceQuota ────────────────────────────────────────────────────────

    #[test]
    fn workspace_quota_default() {
        let q = WorkspaceQuota::default();
        assert!(q.max_bytes.is_some());
        assert!(q.warning_threshold.is_some());
    }

    #[test]
    fn workspace_quota_unlimited() {
        let q = WorkspaceQuota::default().unlimited();
        assert!(q.max_bytes.is_none());
        assert!(q.max_entries.is_none());
        assert!(q.max_file_bytes.is_none());
        assert!(q.warning_threshold.is_none());
    }

    #[test]
    fn workspace_quota_check_publish_ok() {
        let q = WorkspaceQuota::default();
        q.check_publish(1024, 0, 0).unwrap();
    }

    #[test]
    fn workspace_quota_check_publish_file_too_large() {
        let q = WorkspaceQuota::default().with_max_file_bytes(100);
        let err = q.check_publish(200, 0, 0).unwrap_err();
        assert!(matches!(err, QuotaError::FileTooLarge { .. }));
    }

    #[test]
    fn workspace_quota_check_publish_out_of_space() {
        let q = WorkspaceQuota::new(1000);
        let err = q.check_publish(500, 600, 0).unwrap_err();
        assert!(matches!(err, QuotaError::OutOfSpace { .. }));
    }

    #[test]
    fn workspace_quota_check_publish_too_many_entries() {
        let q = WorkspaceQuota::default();
        let err = q.check_publish(10, 0, MAX_WORKSPACE_FILE_COUNT).unwrap_err();
        assert!(matches!(err, QuotaError::TooManyEntries { .. }));
    }

    #[test]
    fn workspace_quota_warning_threshold() {
        let q = WorkspaceQuota::new(1000).with_warning_threshold(0.5);
        assert!(!q.is_above_warning(400)); // 40%
        assert!(q.is_above_warning(600));  // 60%
        assert!(q.is_above_warning(1000)); // 100%
    }

    #[test]
    fn workspace_quota_summary() {
        assert_eq!(WorkspaceQuota::default().summary(), "1 GiB max / 100000 files");
        assert_eq!(WorkspaceQuota::default().unlimited().summary(), "unlimited");
        // WorkspaceQuota::new() also sets max_entries, so it hits the entries arm.
        assert_eq!(WorkspaceQuota::new(10 << 30).summary(), "10 GiB max / 100000 files");
    }

    // ── WorkspaceInstance ────────────────────────────────────────────────────

    #[test]
    fn workspace_instance_add_grant() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        let grant = AclGrant::single(AclPrincipal::Public, AclPermission::READ, 1000);
        inst.add_grant(grant.clone());
        assert_eq!(inst.acl.len(), 1);
        // Adding same id replaces
        inst.add_grant(grant);
        assert_eq!(inst.acl.len(), 1);
    }

    #[test]
    fn workspace_instance_remove_grant() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        let grant = AclGrant::single(AclPrincipal::Public, AclPermission::READ, 1000);
        let id = grant.id.clone();
        inst.add_grant(grant);
        assert!(inst.remove_grant(&id));
        assert!(inst.acl.is_empty());
        assert!(!inst.remove_grant(&id)); // already gone
    }

    #[test]
    fn workspace_instance_find_share_link() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        let entry = ShareLinkEntry {
            link: ShareLink::new(ShareScope::Full, 1000),
            workspace_name: "work".into(),
            creator_node_id: "creator".into(),
        };
        let token = entry.link.token.clone();
        inst.add_share_link(entry);
        let found = inst.find_share_link(&token, 9999).unwrap();
        assert_eq!(found.token, token);
    }

    #[test]
    fn workspace_instance_can_publish() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        inst.update_usage(100, 10);
        inst.quota = WorkspaceQuota::new(500).with_max_file_bytes(1000);
        inst.can_publish(200).unwrap();
        let err = inst.can_publish(500).unwrap_err();
        assert!(matches!(err, QuotaError::OutOfSpace { .. }));
    }

    #[test]
    fn workspace_instance_has_permission() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        inst.add_grant(AclGrant::single(AclPrincipal::Public, AclPermission::READ, 1000));
        assert!(inst.has_permission(&AclPrincipal::Public, AclPermission::READ, 9999));
        assert!(!inst.has_permission(&AclPrincipal::Public, AclPermission::WRITE, 9999));
    }

    #[test]
    fn workspace_instance_is_default() {
        let inst = WorkspaceInstance::new("default", 1000).with_default();
        assert!(inst.is_default);
    }

    #[test]
    fn workspace_instance_update_usage() {
        let mut inst = WorkspaceInstance::new("work", 1000);
        let before = inst.updated_at;
        std::thread::sleep(std::time::Duration::from_secs(1));
        inst.update_usage(1000, 50);
        assert_eq!(inst.used_bytes, 1000);
        assert_eq!(inst.entry_count, 50);
        assert!(inst.updated_at >= before);
    }

    // ── Serde round-trips ──────────────────────────────────────────────────

    #[test]
    fn serde_acl_grant_json() {
        let grant = AclGrant::single(AclPrincipal::NodeId("abc".into()), AclPermission::READ, 1000);
        let json = serde_json::to_string(&grant).unwrap();
        let back: AclGrant = serde_json::from_str(&json).unwrap();
        assert_eq!(grant.id, back.id);
        assert_eq!(grant.entries.len(), back.entries.len());
    }

    #[test]
    fn serde_share_link_json() {
        let link = ShareLink::new(ShareScope::Shared, 1000)
            .with_expiry(2000)
            .with_max_clicks(5)
            .with_description("design docs");
        let json = serde_json::to_string(&link).unwrap();
        let back: ShareLink = serde_json::from_str(&json).unwrap();
        assert_eq!(link.id, back.id);
        assert_eq!(link.token, back.token);
        assert_eq!(link.scope.tag(), back.scope.tag());
    }

    #[test]
    fn serde_quota_json() {
        let q = WorkspaceQuota::new(10 << 30).with_max_file_bytes(1 << 20);
        let json = serde_json::to_string(&q).unwrap();
        let back: WorkspaceQuota = serde_json::from_str(&json).unwrap();
        assert_eq!(q.max_bytes, back.max_bytes);
        assert_eq!(q.max_file_bytes, back.max_file_bytes);
    }

    #[test]
    fn serde_workspace_instance_json() {
        let inst = WorkspaceInstance::new("test-workspace", 1000)
            .with_description("My test workspace")
            .with_default();
        let json = serde_json::to_string(&inst).unwrap();
        let back: WorkspaceInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(inst.name, back.name);
        assert_eq!(inst.is_default, back.is_default);
    }

    // ── Constants ───────────────────────────────────────────────────────────

    #[test]
    fn constants() {
        assert_eq!(MAX_ACL_LABEL_LEN, 64);
        assert_eq!(MAX_ACL_ENTRIES, 256);
        assert_eq!(SHARE_LINK_TOKEN_BYTES, 32);
        assert_eq!(MAX_SHARE_LINKS, 64);
        assert_eq!(DEFAULT_WORKSPACE_QUOTA_BYTES, 1 << 30);
        assert_eq!(MAX_WORKSPACE_QUOTA_BYTES, 1 << 40);
        assert_eq!(MAX_WORKSPACE_FILE_COUNT, 100_000);
    }
}
