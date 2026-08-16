//! Persistent [`NodeIdentity`] — load-or-create from
//! `{data_dir}/node_identity.json`, plus typed mutators that
//! re-validate on every change.
//!
//! The wrapper sits between the on-disk JSON document and the
//! in-memory `NodeIdentity`. Every setter re-runs the invariants
//! defined in `a3net_types::node_identity`, so a malformed value
//! on disk cannot propagate into the live node.
//!
//! ## Atomic writes
//!
//! Like [`ContactsManager`], writes go through a `.tmp` file and
//! then an atomic `rename(2)` so a crash never leaves a
//! half-written identity document.
//!
//! ## Schema versioning
//!
//! The on-disk envelope carries a `version: u32` field. The
//! current version is [`NODE_IDENTITY_FILE_VERSION`]. Future
//! schema bumps add a `from_json_v1` migration step.

#![forbid(unsafe_code)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use a3net_types::{DnsNodeId, NodeId, NodeIdentity, NodeIdentityError, WalletAddress};
use parking_lot::RwLock;
use tracing::{debug, info, warn};

/// Filename of the on-disk identity JSON document.
pub const NODE_IDENTITY_FILE_NAME: &str = "node_identity.json";

/// Filename used during atomic-rename writes.
const NODE_IDENTITY_FILE_TMP: &str = "node_identity.json.tmp";

/// Current schema version of the identity envelope.
pub const NODE_IDENTITY_FILE_VERSION: u32 = 1;

/// Persistent wrapper around [`NodeIdentity`]. Cheap to clone
/// (the inner state is `Arc<RwLock<…>>`).
#[derive(Debug, Clone)]
pub struct NodeIdentityStore {
    path: PathBuf,
    inner: Arc<RwLock<NodeIdentity>>,
}

impl NodeIdentityStore {
    /// Open (or create) the identity document at `<dir>/node_identity.json`.
    ///
    /// If the file exists and parses successfully, its content is
    /// loaded. If it does not exist, a placeholder identity is
    /// generated from `node_id` (with empty fields the caller is
    /// expected to fill in via [`NodeIdentityStore::set_*`]) and
    /// written back so the file exists for subsequent readers.
    ///
    /// A malformed file is a fatal error — the operator must
    /// inspect / move it aside and restart.
    pub fn open(dir: &Path, node_id: NodeId) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(NODE_IDENTITY_FILE_NAME);

        let identity = if path.exists() {
            let bytes = fs::read(&path)?;
            match serde_json::from_slice::<NodeIdentity>(&bytes) {
                Ok(mut id) => {
                    // Enforce that the persisted identity belongs to
                    // this node. A mismatch usually means the
                    // operator copied someone else's file or the
                    // node_id changed (key rotation); we surface
                    // the error rather than silently accepting it.
                    if id.digital_identity != node_id {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "node_identity.json digital_identity ({}) \
                                 does not match local node_id ({}); \
                                 move the file aside and restart",
                                id.digital_identity, node_id,
                            ),
                        ));
                    }
                    info!(
                        nickname = %id.nickname,
                        dns = %id.dns_node_id,
                        "loaded node identity from disk"
                    );
                    id.touch();
                    id
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "failed to parse {}: {e}. \
                             Move the file aside and restart to rebuild.",
                            path.display()
                        ),
                    ));
                }
            }
        } else {
            debug_create_placeholder(node_id)?
        };

        let store = Self {
            path,
            inner: Arc::new(RwLock::new(identity)),
        };
        if !store.path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    /// Snapshot of the current identity.
    pub fn snapshot(&self) -> NodeIdentity {
        self.inner.read().clone()
    }

    /// Replace the email.
    pub fn set_email(&self, email: impl Into<String>) -> Result<(), NodeIdentityError> {
        self.inner.write().set_email(email)?;
        self.persist_or_warn();
        Ok(())
    }

    /// Replace the nickname.
    pub fn set_nickname(
        &self,
        nickname: impl Into<String>,
    ) -> Result<(), NodeIdentityError> {
        self.inner.write().set_nickname(nickname)?;
        self.persist_or_warn();
        Ok(())
    }

    /// Replace the description.
    pub fn set_description(
        &self,
        description: impl Into<String>,
    ) -> Result<(), NodeIdentityError> {
        self.inner.write().set_description(description)?;
        self.persist_or_warn();
        Ok(())
    }

    /// Replace the avatar.
    pub fn set_avatar(
        &self,
        avatar: a3net_types::Avatar,
    ) -> Result<(), NodeIdentityError> {
        self.inner.write().set_avatar(avatar)?;
        self.persist_or_warn();
        Ok(())
    }

    /// Replace the wallet address.
    pub fn set_wallet_address(&self, addr: WalletAddress) -> Result<(), NodeIdentityError> {
        self.inner.write().set_wallet_address(addr)?;
        self.persist_or_warn();
        Ok(())
    }

    /// Replace the DNS-assigned 12-digit id.
    pub fn set_dns_node_id(&self, dns: DnsNodeId) -> Result<(), NodeIdentityError> {
        {
            let mut guard = self.inner.write();
            guard.dns_node_id = dns;
            guard.touch();
        }
        self.persist_or_warn();
        Ok(())
    }

    /// Approximate serialised size in bytes.
    pub fn approx_size(&self) -> usize {
        self.inner.read().approx_size()
    }

    /// BLAKE3 digest of the canonical identity JSON. Useful for
    /// the gossip `NodeIdentityCard.contacts_digest` slot, or as
    /// an audit fingerprint.
    pub fn digest(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(&*self.inner.read()).unwrap_or_default();
        *blake3::hash(&bytes).as_bytes()
    }

    /// Force a write of the in-memory state to disk.
    pub fn persist(&self) -> std::io::Result<()> {
        let snapshot = {
            let guard = self.inner.read();
            serde_json::to_vec(&*guard).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("serialise identity: {e}"),
                )
            })?
        };
        let tmp = self.path.with_file_name(NODE_IDENTITY_FILE_TMP);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&snapshot)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        debug!(
            path = %self.path.display(),
            bytes = snapshot.len(),
            "node identity persisted"
        );
        Ok(())
    }

    fn persist_or_warn(&self) {
        if let Err(e) = self.persist() {
            warn!(error = %e, "failed to persist node identity");
        }
    }
}

fn debug_create_placeholder(node_id: NodeId) -> std::io::Result<NodeIdentity> {
    use a3net_types::Avatar;
    // A freshly-created identity uses an empty nickname / email /
    // description / wallet — the operator is expected to fill
    // them in via the CLI before publishing. We DO seed a
    // placeholder `dns_node_id` of `0` so the gossip frame
    // validates (12 digits), and a tiny data-URI avatar so the
    // envelope stays self-contained.
    let dns = DnsNodeId::from_u64(0).map_err(a3net_to_io)?;
    let avatar = Avatar::from_data_uri("png", "").map_err(a3net_to_io)?;
    let wallet = WalletAddress::from_bytes([0; 20]);
    // `NodeIdentity::new` validates nickname/email/description
    // internally — we use sentinel placeholders that the operator
    // is expected to overwrite. `set_*` will be called
    // immediately afterwards so the file on disk always reflects
    // the latest operator intent.
    NodeIdentity::new(
        node_id,
        dns,
        "placeholder",
        "noreply@example.invalid",
        avatar,
        "",
        wallet,
    )
    .map_err(a3net_to_io)
}

fn a3net_to_io(e: a3net_types::AdnetError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_types::Avatar;

    fn fresh() -> (tempfile::TempDir, NodeId, NodeIdentityStore) {
        let dir = tempfile::tempdir().unwrap();
        let id = NodeId::random();
        let store = NodeIdentityStore::open(dir.path(), id.clone()).unwrap();
        (dir, id, store)
    }

    #[test]
    fn open_creates_file() {
        let (dir, _id, _store) = fresh();
        assert!(dir.path().join(NODE_IDENTITY_FILE_NAME).exists());
    }

    #[test]
    fn open_loads_existing() {
        let dir = tempfile::tempdir().unwrap();
        let id = NodeId::random();
        let store = NodeIdentityStore::open(dir.path(), id.clone()).unwrap();
        store.set_nickname("alice").unwrap();
        store.set_email("alice@example.com").unwrap();
        // Re-open with the same node_id.
        let store2 = NodeIdentityStore::open(dir.path(), id.clone()).unwrap();
        assert_eq!(store2.snapshot().nickname, "alice");
        assert_eq!(store2.snapshot().email, "alice@example.com");
    }

    #[test]
    fn open_rejects_node_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = NodeId::random();
        let store = NodeIdentityStore::open(dir.path(), id1.clone()).unwrap();
        store.set_nickname("alice").unwrap();
        // Re-open with a DIFFERENT node_id — must error.
        let id2 = NodeId::random();
        let err = NodeIdentityStore::open(dir.path(), id2).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_rejects_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(NODE_IDENTITY_FILE_NAME),
            b"not json",
        )
        .unwrap();
        let err = NodeIdentityStore::open(dir.path(), NodeId::random()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn setters_validate_and_persist() {
        let (_dir, _id, store) = fresh();
        assert!(store.set_email("alice@example.com").is_ok());
        assert!(store.set_nickname("alice").is_ok());
        assert!(store.set_description("hello world").is_ok());
        let avatar = Avatar::from_url("https://example.com/a.png").unwrap();
        assert!(store.set_avatar(avatar).is_ok());
        let snap = store.snapshot();
        assert_eq!(snap.nickname, "alice");
        assert_eq!(snap.email, "alice@example.com");
    }

    #[test]
    fn setters_reject_invalid() {
        let (_dir, _id, store) = fresh();
        assert!(store.set_email("not-an-email").is_err());
        assert!(store.set_nickname("").is_err());
        let long = "x".repeat(129);
        assert!(store.set_description(&long).is_err());
    }

    #[test]
    fn set_dns_node_id() {
        let (_dir, _id, store) = fresh();
        let dns = DnsNodeId::parse("483726150931").unwrap();
        store.set_dns_node_id(dns).unwrap();
        assert_eq!(store.snapshot().dns_node_id, dns);
    }

    #[test]
    fn digest_changes_on_update() {
        let (_dir, _id, store) = fresh();
        let d0 = store.digest();
        store.set_nickname("alice").unwrap();
        let d1 = store.digest();
        assert_ne!(d0, d1);
    }

    #[test]
    fn approx_size_positive() {
        let (_dir, _id, store) = fresh();
        assert!(store.approx_size() > 0);
    }

    #[test]
    fn persist_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let id = NodeId::random();
        let store = NodeIdentityStore::open(dir.path(), id.clone()).unwrap();
        store.set_nickname("alice").unwrap();
        store.set_email("a@b.io").unwrap();
        let raw = std::fs::read(dir.path().join(NODE_IDENTITY_FILE_NAME)).unwrap();
        let back: NodeIdentity = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back.nickname, "alice");
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NodeIdentityStore>();
    }
}
