//! IPNS operations for the CLI.
//!
//! Provides commands for IPNS name management:
//! - `name publish` - Publish an IPNS name with a value
//! - `name resolve` - Resolve an IPNS name to its value
//! - `name local` - List local IPNS names
//! - `name export` - Export an IPNS key
//! - `name import` - Import an IPNS key
//! - `key gen` - Generate a new Ed25519 key pair
//! - `key list` - List local keys
//! - `key rm` - Remove a key
//! - `key rename` - Rename a key

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use a3net_blobstore::BlobStore;
use a3net_namespace::{
    Ed25519SecretKey, IpnRecord, IpnPublisher, IpnResolver,
    SecretKey, Verifier, IpnTransport,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

/// IPNS CLI errors.
#[derive(Debug, Error)]
pub enum IpnCliError {
    #[error("record not found: {0}")]
    NotFound(String),

    #[error("invalid key: {0}")]
    InvalidKey(String),

    #[error("signature verification failed")]
    InvalidSignature,

    #[error("operation failed: {0}")]
    Operation(String),

    #[error("key not found: {0}")]
    KeyNotFound(String),
}

/// Result of an IPNS publish operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResult {
    pub name: String,
    pub value: String,
    pub sequence: u64,
    pub ttl_secs: u64,
    pub signature: String,
    /// Whether this is an empty/placeholder namespace.
    #[serde(default)]
    pub is_empty: bool,
}

/// Result of an IPNS resolve operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveResult {
    pub path: String,
    pub remainder: Option<String>,
    /// Number of resolution steps taken.
    #[serde(default)]
    pub steps: usize,
    /// Whether the final value is an IPNS name (recursive resolution needed).
    #[serde(default)]
    pub is_ipns: bool,
}

/// Detailed record info for export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordInfo {
    pub name: String,
    pub value: String,
    pub sequence: u64,
    pub ttl_secs: u64,
    pub created: u64,
    pub expires: u64,
    pub signature: String,
    pub validity_type: String,
    pub validity_offset: u64,
    pub is_expired: bool,
}

/// Local IPNS key info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub id: String,
    pub name: String,
    pub key_type: String,
}

/// Key manager for storing and managing IPNS keys.
pub struct KeyManager {
    keys_dir: PathBuf,
    keys: RwLock<Vec<KeyInfo>>,
}

impl KeyManager {
    /// Create a new key manager.
    pub fn new(keys_dir: PathBuf) -> Self {
        Self {
            keys_dir,
            keys: RwLock::new(Vec::new()),
        }
    }

    /// Initialize the key manager and load existing keys.
    pub async fn init(&self) -> Result<(), IpnCliError> {
        // Ensure keys directory exists
        tokio::fs::create_dir_all(&self.keys_dir)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        // Load existing keys from disk
        self.load_keys().await
    }

    /// Load keys from disk.
    async fn load_keys(&self) -> Result<(), IpnCliError> {
        let mut keys = self.keys.write().await;
        keys.clear();

        let mut entries = tokio::fs::read_dir(&self.keys_dir)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| IpnCliError::Operation(e.to_string()))? {
            let path = entry.path();
            if path.extension().map(|e| e == "key").unwrap_or(false) {
                if let Ok(key_info) = self.load_key_info(&path).await {
                    keys.push(key_info);
                }
            }
        }

        Ok(())
    }

    /// Load key info from a file.
    async fn load_key_info(&self, path: &PathBuf) -> Result<KeyInfo, IpnCliError> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        let key_info: KeyInfo = serde_json::from_str(&content)
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        Ok(key_info)
    }

    /// Save key info to a file.
    async fn save_key_info(&self, key_info: &KeyInfo) -> Result<(), IpnCliError> {
        let path = self.keys_dir.join(format!("{}.key", key_info.name));
        let content = serde_json::to_string_pretty(key_info)
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        tokio::fs::write(&path, content)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        Ok(())
    }

    /// Generate a new key pair.
    pub async fn generate_key(&self, name: &str) -> Result<Ed25519SecretKey, IpnCliError> {
        let secret_key = Ed25519SecretKey::generate();
        let public_key_bytes = secret_key.public_key_bytes();

        // Create key info
        let key_info = KeyInfo {
            id: blake3::hash(&public_key_bytes).to_hex().to_string(),
            name: name.to_string(),
            key_type: "ed25519".to_string(),
        };

        // Save key info
        self.save_key_info(&key_info).await?;

        // Persist the *private* key bytes (bincode-serialized so we
        // can round-trip through `Ed25519SecretKey::from_bytes` on
        // load). Writing the public key here would silently produce
        // an unrelated signing key on reload — see `test_key_loading`.
        let key_path = self.keys_dir.join(format!("{}.pem", name));
        let private_key_bytes = secret_key.to_bytes();
        tokio::fs::write(&key_path, &private_key_bytes)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        // Add to in-memory list
        let mut keys = self.keys.write().await;
        keys.push(key_info);

        Ok(secret_key)
    }

    /// Load a key by name.
    pub async fn load_key(&self, name: &str) -> Result<Ed25519SecretKey, IpnCliError> {
        let key_path = self.keys_dir.join(format!("{}.pem", name));

        if !key_path.exists() {
            return Err(IpnCliError::KeyNotFound(name.to_string()));
        }

        let key_bytes = tokio::fs::read(&key_path)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        if key_bytes.len() != 32 {
            return Err(IpnCliError::InvalidKey("invalid key size".to_string()));
        }

        let key_array: [u8; 32] = key_bytes.try_into()
            .map_err(|_| IpnCliError::InvalidKey("invalid key format".to_string()))?;

        Ed25519SecretKey::from_bytes(&key_array)
            .map_err(|e| IpnCliError::InvalidKey(e.to_string()))
    }

    /// List all keys.
    pub async fn list_keys(&self) -> Vec<KeyInfo> {
        self.keys.read().await.clone()
    }

    /// Remove a key.
    pub async fn remove_key(&self, name: &str) -> Result<(), IpnCliError> {
        let mut keys = self.keys.write().await;

        // Check if key exists
        if !keys.iter().any(|k| k.name == name) {
            return Err(IpnCliError::KeyNotFound(name.to_string()));
        }

        // Remove from in-memory list
        keys.retain(|k| k.name != name);

        // Remove files
        let key_info_path = self.keys_dir.join(format!("{}.key", name));
        let key_pem_path = self.keys_dir.join(format!("{}.pem", name));

        tokio::fs::remove_file(&key_info_path).await.ok();
        tokio::fs::remove_file(&key_pem_path).await.ok();

        Ok(())
    }

    /// Rename a key.
    pub async fn rename_key(&self, old_name: &str, new_name: &str) -> Result<KeyInfo, IpnCliError> {
        let mut keys = self.keys.write().await;

        // Find the key
        let key_info = keys.iter_mut()
            .find(|k| k.name == old_name)
            .ok_or_else(|| IpnCliError::KeyNotFound(old_name.to_string()))?;

        // Update name
        key_info.name = new_name.to_string();

        // Rename files
        let old_key_info_path = self.keys_dir.join(format!("{}.key", old_name));
        let old_key_pem_path = self.keys_dir.join(format!("{}.pem", old_name));
        let new_key_info_path = self.keys_dir.join(format!("{}.key", new_name));
        let new_key_pem_path = self.keys_dir.join(format!("{}.pem", new_name));

        tokio::fs::rename(&old_key_info_path, &new_key_info_path).await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;
        tokio::fs::rename(&old_key_pem_path, &new_key_pem_path).await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        Ok(key_info.clone())
    }
}

/// IPNS operations handler.
pub struct IpnOps {
    publisher: Arc<IpnPublisher>,
    resolver: Arc<IpnResolver>,
    /// Optional transport for network resolution.
    transport: Option<Arc<dyn IpnTransport>>,
    /// Maximum depth for recursive resolution.
    max_resolve_depth: usize,
}

impl IpnOps {
    /// Create a new IPNS operations handler.
    pub fn new(secret_key: Arc<dyn SecretKey>) -> Self {
        Self {
            publisher: Arc::new(IpnPublisher::new(secret_key)),
            resolver: Arc::new(IpnResolver::new(std::time::Duration::from_secs(3600))),
            transport: None,
            max_resolve_depth: 32,
        }
    }

    /// Create with custom max resolve depth.
    pub fn with_max_depth(secret_key: Arc<dyn SecretKey>, max_depth: usize) -> Self {
        Self {
            publisher: Arc::new(IpnPublisher::new(secret_key)),
            resolver: Arc::new(IpnResolver::new(std::time::Duration::from_secs(3600))),
            transport: None,
            max_resolve_depth: max_depth,
        }
    }

    /// Set the transport for network resolution.
    pub fn with_transport(mut self, transport: Arc<dyn IpnTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Publish a value under an IPNS name.
    pub async fn publish(
        &self,
        name: &str,
        value: String,
        ttl_secs: Option<u64>,
    ) -> Result<PublishResult, IpnCliError> {
        let ttl = std::time::Duration::from_secs(ttl_secs.unwrap_or(86400));

        let record = self.publisher
            .publish(name, value, ttl)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        // Cache locally
        self.resolver.cache_record(record.clone());

        Ok(PublishResult {
            name: record.name.clone(),
            value: record.value.clone(),
            sequence: record.sequence,
            ttl_secs: record.ttl_secs,
            signature: hex::encode(&record.signature),
            is_empty: record.is_empty(),
        })
    }

    /// Resolve an IPNS name (simple, non-recursive).
    pub async fn resolve(&self, name: &str) -> Result<ResolveResult, IpnCliError> {
        self.resolve_impl(name, 0).await
    }

    /// Internal recursive resolution.
    async fn resolve_impl(&self, name: &str, current_depth: usize) -> Result<ResolveResult, IpnCliError> {
        if current_depth >= self.max_resolve_depth {
            return Err(IpnCliError::Operation(format!(
                "max resolution depth ({}) exceeded", self.max_resolve_depth
            )));
        }

        // Try local cache first
        if let Some(record) = self.resolver.get_cached(name) {
            if !record.is_expired() {
                return Ok(ResolveResult {
                    path: record.value.clone(),
                    remainder: None,
                    steps: current_depth + 1,
                    is_ipns: record.value.starts_with("/ipns/"),
                });
            }
        }

        // Try from publisher's local records
        if let Some(record) = self.publisher.get_local(name) {
            if !record.is_expired() {
                return Ok(ResolveResult {
                    path: record.value.clone(),
                    remainder: None,
                    steps: current_depth + 1,
                    is_ipns: record.value.starts_with("/ipns/"),
                });
            }
        }

        Err(IpnCliError::NotFound(name.to_string()))
    }

    /// Resolve with recursive IPNS chain following.
    /// If the resolved value points to another IPNS name, it will be followed.
    pub async fn resolve_recursive(&self, name: &str) -> Result<ResolveResult, IpnCliError> {
        let mut current_name = name.to_string();
        let mut steps = 0;

        loop {
            if steps >= self.max_resolve_depth {
                return Err(IpnCliError::Operation(format!(
                    "max resolution steps ({}) exceeded", self.max_resolve_depth
                )));
            }
            // Count every IPNS-resolution step we perform, including
            // the final hop that lands on the target value (e.g.
            // resolving `name1 -> /ipns/name2 -> /ipfs/QmFinal` is
            // two steps: the chain link + the leaf).
            steps += 1;

            let mut found_record: Option<IpnRecord> = None;

            // Try local cache first
            if let Some(record) = self.resolver.get_cached(&current_name) {
                if !record.is_expired() {
                    found_record = Some(record);
                }
            }

            // Try from publisher's local records if not found
            if found_record.is_none() {
                if let Some(record) = self.publisher.get_local(&current_name) {
                    if !record.is_expired() {
                        found_record = Some(record);
                    }
                }
            }

            // Try network transport if not found
            if found_record.is_none() {
                if let Some(ref transport) = self.transport {
                    match transport.resolve_now(&current_name).await {
                        Ok(record) => {
                            // Cache the record
                            self.resolver.cache_record(record.clone());
                            found_record = Some(record);
                        }
                        Err(e) => {
                            tracing::debug!(name = %current_name, error = %e, "network resolution failed");
                        }
                    }
                }
            }

            // Process the found record
            if let Some(record) = found_record {
                if record.value.starts_with("/ipns/") {
                    // Follow the chain
                    current_name = record.value.trim_start_matches("/ipns/").to_string();
                    continue;
                }
                return Ok(ResolveResult {
                    path: record.value.clone(),
                    remainder: None,
                    steps,
                    is_ipns: false,
                });
            }

            return Err(IpnCliError::NotFound(current_name));
        }
    }

    /// List local IPNS records.
    pub fn list_local(&self) -> Vec<(String, IpnRecord)> {
        self.publisher.list_local()
    }

    /// Get detailed record info for export.
    pub fn get_record_info(&self, name: &str) -> Result<RecordInfo, IpnCliError> {
        let record = self.publisher.get_local(name)
            .ok_or_else(|| IpnCliError::NotFound(name.to_string()))?;

        Ok(RecordInfo {
            name: record.name.clone(),
            value: record.value.clone(),
            sequence: record.sequence,
            ttl_secs: record.ttl_secs,
            created: record.created,
            expires: record.expires,
            signature: hex::encode(&record.signature),
            validity_type: record.validity_type.clone(),
            validity_offset: record.validity_offset,
            is_expired: record.is_expired(),
        })
    }

    /// Export a record as JSON.
    pub fn export_record(&self, name: &str) -> Result<String, IpnCliError> {
        let record = self.publisher.get_local(name)
            .ok_or_else(|| IpnCliError::NotFound(name.to_string()))?;

        serde_json::to_string_pretty(&record)
            .map_err(|e| IpnCliError::Operation(e.to_string()))
    }

    /// Export record with verification info.
    pub fn export_record_detailed(&self, name: &str) -> Result<String, IpnCliError> {
        let info = self.get_record_info(name)?;
        serde_json::to_string_pretty(&info)
            .map_err(|e| IpnCliError::Operation(e.to_string()))
    }

    /// Import a record from JSON.
    pub fn import_record(&self, json: &str) -> Result<PublishResult, IpnCliError> {
        let record: IpnRecord = serde_json::from_str(json)
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        // Verify signature (length check for Ed25519)
        if record.signature.len() != 64 {
            return Err(IpnCliError::InvalidSignature);
        }

        // Cache the record
        self.resolver.cache_record(record.clone());

        Ok(PublishResult {
            name: record.name.clone(),
            value: record.value.clone(),
            sequence: record.sequence,
            ttl_secs: record.ttl_secs,
            signature: hex::encode(&record.signature),
            is_empty: record.is_empty(),
        })
    }

    /// Get the IPNS name for the publisher's key.
    pub fn local_name(&self) -> Option<String> {
        self.publisher.get_local("self").map(|r| r.name)
    }

    /// Create an empty namespace (placeholder).
    pub async fn create_empty_namespace(
        &self,
        name: &str,
        ttl_secs: Option<u64>,
    ) -> Result<PublishResult, IpnCliError> {
        let ttl = std::time::Duration::from_secs(ttl_secs.unwrap_or(86400));

        let record = self.publisher
            .create_empty_namespace(name, ttl)
            .await
            .map_err(|e| IpnCliError::Operation(e.to_string()))?;

        Ok(PublishResult {
            name: record.name.clone(),
            value: record.value.clone(),
            sequence: record.sequence,
            ttl_secs: record.ttl_secs,
            signature: hex::encode(&record.signature),
            is_empty: true,
        })
    }
}

// ---------------------------------------------------------------------------
// Top-level CLI dispatcher
// ---------------------------------------------------------------------------

use crate::cli::{KeyCmd as CliKeyCmd, NameCmd as CliNameCmd};

/// Top-level dispatcher for `a3net name <sub>`. Offline — does not require
/// a running node.
pub fn run_name(sub: &CliNameCmd, data_dir: &std::path::Path) -> anyhow::Result<()> {
    // Name operations need the IPNS subsystem which requires a running node
    // for network resolution. For local operations, we delegate to the
    // standalone service.
    let keys_dir = data_dir.join("ipns_keys");
    let manager = KeyManager::new(keys_dir);

    // Initialize synchronously via block_on
    let _ = futures::executor::block_on(manager.init());

    match sub {
        CliNameCmd::Publish { path, lifetime, json } => {
            // Publishing requires the node's secret key. The
            // CLI's offline path can only stamp a local record;
            // for wire-traversal, the operator must run the
            // node-side `init_ipns` path via `a3net node` or
            // by speaking the IPC protocol. Surface the
            // contract clearly so the operator knows what to do.
            println!("name publish: {} (lifetime={:?})", path, lifetime);
            let msg = "name publish requires a running node. \
                       Run `a3net node init_ipns` on the daemon, then \
                       re-issue this command via the IPC client. \
                       The offline `a3net key gen` command can mint a \
                       local Ed25519 keypair in the meantime.";
            if *json {
                println!("{{\"error\": \"{msg}\"}}");
            } else {
                println!("(IPNS publish: {msg})");
            }
        }
        CliNameCmd::Resolve { name, recursive, json } => {
            println!("name resolve: {} (recursive={})", name, recursive);
            let msg = "name resolve requires a running node. \
                       Run `a3net node init_ipns` on the daemon, then \
                       re-issue this command via the IPC client. \
                       The cache-only fast path is available via \
                       `a3net ipns lookup`; the local-only path is \
                       via `a3net ipns local`.";
            if *json {
                println!("{{\"error\": \"{msg}\"}}");
            } else {
                println!("(IPNS resolution: {msg})");
            }
        }
        CliNameCmd::Local { json } => {
            if *json {
                println!("{{\"error\": \"local names require a running node\"}}");
            } else {
                println!("(local IPNS names not yet implemented)");
            }
        }
    }
    Ok(())
}

/// Top-level dispatcher for `a3net key <sub>`. Offline — does not require
/// a running node.
pub fn run_key(sub: &CliKeyCmd, data_dir: &std::path::Path) -> anyhow::Result<()> {
    let keys_dir = data_dir.join("ipns_keys");

    // Initialize synchronously via block_on
    futures::executor::block_on(async {
        let manager = KeyManager::new(keys_dir);
        manager.init().await?;

        match sub {
            CliKeyCmd::Gen { name, key_type: _, json } => {
                match manager.generate_key(name).await {
                    Ok(key) => {
                        let name_hex = key.ipns_name();
                        if *json {
                            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                                "name": name,
                                "id": name_hex,
                                "type": "ed25519"
                            }))?);
                        } else {
                            println!("generated key '{name}'");
                            println!("  name: {}", name);
                            println!("  id:   {}", name_hex);
                        }
                    }
                    Err(e) => {
                        eprintln!("failed to generate key '{name}': {e}");
                    }
                }
            }
            CliKeyCmd::List { json } => {
                let keys = manager.list_keys().await;
                if *json {
                    println!("{}", serde_json::to_string_pretty(&keys)?);
                } else {
                    if keys.is_empty() {
                        println!("(no keys)");
                    } else {
                        println!("{:<24} {:<32} {}", "NAME", "ID", "TYPE");
                        for k in keys {
                            println!("{:<24} {:<32} {}", k.name, k.id, k.key_type);
                        }
                    }
                }
            }
            CliKeyCmd::Rm { name, force: _ } => {
                match manager.remove_key(name).await {
                    Ok(()) => println!("removed key '{name}'"),
                    Err(IpnCliError::KeyNotFound(_)) => {
                        eprintln!("key not found: {name}");
                    }
                    Err(e) => {
                        eprintln!("failed to remove key '{name}': {e}");
                    }
                }
            }
            CliKeyCmd::Rename { old_name, new_name } => {
                match manager.rename_key(old_name, new_name).await {
                    Ok(info) => {
                        println!("renamed key '{old_name}' -> '{new_name}'");
                        println!("  id: {}", info.id);
                    }
                    Err(IpnCliError::KeyNotFound(_)) => {
                        eprintln!("key not found: {old_name}");
                    }
                    Err(e) => {
                        eprintln!("failed to rename key: {e}");
                    }
                }
            }
            CliKeyCmd::Export { name, output } => {
                // Export: print the key info as JSON
                let keys = manager.list_keys().await;
                if let Some(info) = keys.iter().find(|k| k.name == *name) {
                    if let Some(path) = output {
                        std::fs::write(path, serde_json::to_string_pretty(info)?)?;
                        println!("exported key '{name}' to {path}");
                    } else {
                        println!("{}", serde_json::to_string_pretty(info)?);
                    }
                } else {
                    eprintln!("key not found: {name}");
                }
            }
            CliKeyCmd::Import { name, input } => {
                // Import: read key info from file or stdin
                let content = if let Some(path) = input {
                    std::fs::read_to_string(path)?
                } else {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                };
                let info: KeyInfo = serde_json::from_str(&content)
                    .map_err(|e| anyhow::anyhow!("parse key JSON: {}", e))?;
                let manager2 = KeyManager::new(data_dir.join("ipns_keys"));
                manager2.init().await?;
                // We can't import the private key without the raw bytes,
                // so we just note the info for now.
                println!("imported key info for '{name}': {}", info.id);
                println!("(full key import not yet implemented)");
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_manager_init() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = KeyManager::new(temp_dir.path().to_path_buf());
        assert!(temp_dir.path().exists());
    }

    #[tokio::test]
    async fn test_key_generation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = KeyManager::new(temp_dir.path().to_path_buf());
        manager.init().await.unwrap();

        let key = manager.generate_key("test-key").await.unwrap();
        let name = key.ipns_name();

        let keys = manager.list_keys().await;
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].name, "test-key");
        assert_eq!(keys[0].id, name);
    }

    #[tokio::test]
    async fn test_key_loading() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = KeyManager::new(temp_dir.path().to_path_buf());
        manager.init().await.unwrap();

        let original_key = manager.generate_key("my-key").await.unwrap();

        let loaded_key = manager.load_key("my-key").await.unwrap();
        assert_eq!(original_key.public_key_bytes(), loaded_key.public_key_bytes());
    }

    #[tokio::test]
    async fn test_key_removal() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = KeyManager::new(temp_dir.path().to_path_buf());
        manager.init().await.unwrap();

        manager.generate_key("to-remove").await.unwrap();
        assert_eq!(manager.list_keys().await.len(), 1);

        manager.remove_key("to-remove").await.unwrap();
        assert_eq!(manager.list_keys().await.len(), 0);
    }

    #[tokio::test]
    async fn test_key_rename() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = KeyManager::new(temp_dir.path().to_path_buf());
        manager.init().await.unwrap();

        manager.generate_key("old-name").await.unwrap();
        let renamed = manager.rename_key("old-name", "new-name").await.unwrap();

        assert_eq!(renamed.name, "new-name");
        assert_eq!(manager.list_keys().await.len(), 1);
        assert_eq!(manager.list_keys().await[0].name, "new-name");
    }

    #[tokio::test]
    async fn test_ipns_publish_resolve() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        let result = ops.publish("test-name", "/ipfs/QmTest".to_string(), Some(3600)).await.unwrap();
        assert_eq!(result.name, "test-name");
        assert_eq!(result.value, "/ipfs/QmTest");
        assert_eq!(result.sequence, 1);

        // Resolve should work
        let resolved = ops.resolve("test-name").await.unwrap();
        assert_eq!(resolved.path, "/ipfs/QmTest");
    }

    #[tokio::test]
    async fn test_ipns_update_sequence() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        ops.publish("test-name", "/ipfs/QmV1".to_string(), None).await.unwrap();
        let result2 = ops.publish("test-name", "/ipfs/QmV2".to_string(), None).await.unwrap();

        assert_eq!(result2.sequence, 2);
        assert_eq!(result2.value, "/ipfs/QmV2");
    }

    #[tokio::test]
    async fn test_ipns_export_import() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        ops.publish("export-test", "/ipfs/QmExport".to_string(), None).await.unwrap();
        let json = ops.export_record("export-test").unwrap();

        let result = ops.import_record(&json).unwrap();
        assert_eq!(result.name, "export-test");
        assert_eq!(result.value, "/ipfs/QmExport");
    }

    #[tokio::test]
    async fn test_ipns_empty_namespace() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        let result = ops.create_empty_namespace("empty-test", Some(3600)).await.unwrap();
        assert!(result.is_empty);
        assert!(result.value.is_empty());
        assert_eq!(result.sequence, 1);
    }

    #[tokio::test]
    async fn test_ipns_recursive_resolution() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        // Create a chain: name1 -> /ipns/name2 -> /ipfs/QmFinal
        ops.publish("name1", "/ipns/name2".to_string(), Some(3600)).await.unwrap();
        ops.publish("name2", "/ipfs/QmFinal".to_string(), Some(3600)).await.unwrap();

        // Simple resolve returns the IPNS path
        let resolved = ops.resolve("name1").await.unwrap();
        assert_eq!(resolved.path, "/ipns/name2");
        assert!(resolved.is_ipns);

        // Recursive resolve follows the chain
        let resolved = ops.resolve_recursive("name1").await.unwrap();
        assert_eq!(resolved.path, "/ipfs/QmFinal");
        assert_eq!(resolved.steps, 2);
    }

    #[tokio::test]
    async fn test_ipns_record_info() {
        let secret_key = Arc::new(Ed25519SecretKey::generate());
        let ops = IpnOps::new(secret_key);

        ops.publish("info-test", "/ipfs/QmInfo".to_string(), Some(3600)).await.unwrap();
        let info = ops.get_record_info("info-test").unwrap();

        assert_eq!(info.name, "info-test");
        assert_eq!(info.value, "/ipfs/QmInfo");
        assert_eq!(info.sequence, 1);
        assert!(!info.is_expired);
        assert_eq!(info.signature.len(), 128); // hex-encoded 64 bytes
    }
}
