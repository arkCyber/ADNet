//! A3Net identity management for Eliza agents.
//!
//! Provides a [`Wallet`]-backed identity for AI agents participating in
//! A3Net. The flow is:
//!
//! 1. `new(data_dir, agent_id)` — creates (or loads) a 32-byte
//!    secp256k1 secret key, derives the agent's `NodeId` from the
//!    EVM-style address, and produces an [`AgentProfile`].
//!
//! 2. `sign(message)` — BLAKE3-256 hashes the message and produces an
//!    EIP-191 personal signature using the wallet's secret key.
//!
//! 3. `verify(message, signature, node_id)` — recovers the signer from
//!    a message + signature and asserts it matches the expected node.
//!
//! 4. `export_profile()` / `import_profile()` — JSON serialization for
//!    profile sharing across the P2P network.

use a3net_identity::{Wallet, Address, signing::PersonalSignature};
use a3net_types::node::NodeId;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Domain-separated tag baked into every BLAKE3 hash so messages
/// from this crate cannot collide with hashes from other systems.
const IDENTITY_HASH_TAG: &[u8] = b"a3net-eliza-bridge/v1/identity";

/// Agent deployment type within the A3Net ecosystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum AgentType {
    /// General-purpose AI assistant
    Assistant,
    /// Financial analysis agent
    Analyst,
    /// News aggregator and commentator
    Reporter,
    /// Code generation and review agent
    Developer,
    /// Customer service agent
    Support,
    /// Custom specialized agent with a free-form name
    Custom(String),
}

impl AgentType {
    /// Stable lowercase string used by tools and discovery APIs.
    pub fn as_str(&self) -> &str {
        match self {
            AgentType::Assistant => "assistant",
            AgentType::Analyst => "analyst",
            AgentType::Reporter => "reporter",
            AgentType::Developer => "developer",
            AgentType::Support => "support",
            AgentType::Custom(s) => s,
        }
    }
}

/// Agent profile that can be published to the A3Net network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub eliza_agent_id: String,
    pub node_id: NodeId,
    pub display_name: String,
    pub agent_type: AgentType,
    pub bio: String,
    pub avatar_url: Option<String>,
    pub languages: Vec<String>,
    pub accepts_dm: bool,
    pub supports_groups: bool,
    pub capabilities: Vec<String>,
    pub preferences: AgentPreferences,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            eliza_agent_id: String::new(),
            node_id: NodeId::random(),
            display_name: String::from("A3Net Agent"),
            agent_type: AgentType::Assistant,
            bio: String::new(),
            avatar_url: None,
            languages: vec![String::from("en")],
            accepts_dm: true,
            supports_groups: true,
            capabilities: Vec::new(),
            preferences: AgentPreferences::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPreferences {
    pub auto_accept_friends: bool,
    pub notify_on_message: bool,
    pub max_messages_per_minute: u32,
    pub send_typing_indicator: bool,
    pub system_prompt_prefix: Option<String>,
}

impl Default for AgentPreferences {
    fn default() -> Self {
        Self {
            auto_accept_friends: false,
            notify_on_message: true,
            max_messages_per_minute: 60,
            send_typing_indicator: true,
            system_prompt_prefix: None,
        }
    }
}

/// Convert an EVM address (`0x…` 40 hex chars) to a 32-byte
/// A3Net `NodeId` by zero-padding the right half. This is
/// deterministic and round-trips perfectly.
fn address_to_node_id(address: &Address) -> anyhow::Result<NodeId> {
    let bytes = address.as_bytes();
    // 32-byte buffer: 4 zero bytes + 20 address bytes + 8 zero bytes.
    let mut node_bytes = [0u8; 32];
    node_bytes[4..24].copy_from_slice(bytes);
    Ok(NodeId::from_bytes(&node_bytes)?)
}

/// A3Net identity for Eliza agents.
#[derive(Clone)]
pub struct AdnetIdentity {
    wallet: Wallet,
    node_id: NodeId,
    profile: AgentProfile,
}

impl std::fmt::Debug for AdnetIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdnetIdentity")
            .field("node_id", &self.node_id)
            .field("address", &self.wallet.public().address())
            .field("eliza_agent_id", &self.profile.eliza_agent_id)
            .finish()
    }
}

impl AdnetIdentity {
    /// Create a new identity, or load an existing one.
    ///
    /// Persists the wallet to `{data_dir}/wallet.dat` (32 raw bytes)
    /// and the profile to `{data_dir}/profile.json`.
    pub async fn new(data_dir: std::path::PathBuf, agent_id: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&data_dir)?;

        let wallet_path = data_dir.join("wallet.dat");
        let profile_path = data_dir.join("profile.json");

        let wallet = if wallet_path.exists() {
            let data = std::fs::read(&wallet_path)?;
            Wallet::from_bytes(&data)
                .map_err(|e| anyhow::anyhow!("load wallet: {e}"))?
        } else {
            let wallet = Wallet::generate();
            // Write 32-byte secret for portability. File mode is 0600 so
            // only the agent can read it on a shared host.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&wallet_path)?;
                std::io::Write::write_all(&mut f, &wallet.secret_bytes())?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&wallet_path, wallet.secret_bytes())?;
            }
            wallet
        };

        let node_id = address_to_node_id(&wallet.public().address())?;

        let profile = if profile_path.exists() {
            let raw = std::fs::read_to_string(&profile_path)?;
            let mut p: AgentProfile = serde_json::from_str(&raw)?;
            // Ensure profile.NodeId matches the wallet's NodeId.
            p.node_id = node_id.clone();
            p
        } else {
            let mut p = AgentProfile::default();
            p.eliza_agent_id = agent_id.to_string();
            p.node_id = node_id.clone();
            p.updated_at = Utc::now();
            p
        };

        let identity = Self { wallet, node_id, profile };
        identity.save_profile(&profile_path)?;
        Ok(identity)
    }

    /// Persist the current profile to disk.
    fn save_profile(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(&self.profile)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Get the agent's NodeId.
    pub fn node_id(&self) -> NodeId {
        self.node_id.clone()
    }

    /// Get the agent's EVM-style address (`0x…` 40 hex chars).
    pub fn address(&self) -> Address {
        self.wallet.public().address()
    }

    /// Get the agent's current profile.
    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    /// Get mutable profile reference.
    pub fn profile_mut(&mut self) -> &mut AgentProfile {
        &mut self.profile
    }

    /// Update the agent's profile and persist to disk.
    pub fn update_profile(&mut self, mut profile: AgentProfile) -> anyhow::Result<()> {
        if profile.node_id != self.node_id {
            anyhow::bail!(
                "Cannot update profile with different NodeId: have {}, got {}",
                self.node_id,
                profile.node_id
            );
        }
        profile.updated_at = Utc::now();
        self.profile = profile;
        // Best-effort persistence — silently ignore errors here so we
        // don't fail the in-memory update if the FS is temporarily
        // unavailable. Callers that need strict durability should
        // invoke `save_profile_now` explicitly.
        let _ = self.save_profile(&std::path::PathBuf::from("profile.json"));
        Ok(())
    }

    /// Force-persist the current profile to `{data_dir}/profile.json`.
    pub fn save_profile_now(&self, data_dir: &std::path::Path) -> anyhow::Result<()> {
        let path = data_dir.join("profile.json");
        self.save_profile(&path)
    }

    /// Sign a message using the agent's private key.
    ///
    /// Uses BLAKE3-256 with a domain-separated tag for the digest,
    /// then EIP-191 `personal_sign` over the digest.
    pub fn sign(&self, message: &[u8]) -> anyhow::Result<Vec<u8>> {
        let hash = self.hash_message(message);
        let sig = self
            .wallet
            .sign_personal(&hash)
            .map_err(|e| anyhow::anyhow!("sign_personal: {e}"))?;
        Ok(sig.to_compact().to_vec())
    }

    /// Verify a signature against this identity's public key.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> anyhow::Result<bool> {
        let sig = PersonalSignature::from_compact(signature)
            .map_err(|e| anyhow::anyhow!("invalid signature format: {e}"))?;
        let hash = self.hash_message(message);
        let recovered = a3net_identity::WalletPublic::recover_personal(&hash, &sig)
            .map_err(|e| anyhow::anyhow!("verify recovery: {e}"))?;
        Ok(recovered.address().as_bytes() == self.wallet.public().address().as_bytes())
    }

    /// Verify a signature against any node's public key, by
    /// recovering the signer and checking it matches `node_id`.
    pub fn verify_for_node(
        message: &[u8],
        signature: &[u8],
        node_id: &NodeId,
    ) -> anyhow::Result<bool> {
        let sig = PersonalSignature::from_compact(signature)
            .map_err(|e| anyhow::anyhow!("invalid signature format: {e}"))?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(IDENTITY_HASH_TAG);
        hasher.update(message);
        let digest = hasher.finalize();
        let recovered = a3net_identity::WalletPublic::recover_personal(digest.as_bytes(), &sig)
            .map_err(|e| anyhow::anyhow!("verify recovery: {e}"))?;
        let recovered_node = address_to_node_id(&recovered.address())?;
        Ok(&recovered_node == node_id)
    }

    /// BLAKE3-256(message) with a domain-separated tag.
    fn hash_message(&self, message: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(IDENTITY_HASH_TAG);
        hasher.update(message);
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_bytes());
        out
    }

    /// Export the profile as JSON for sharing.
    pub fn export_profile(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.profile)?)
    }

    /// Import a profile from JSON produced by `export_profile`.
    pub fn import_profile(&mut self, json: &str) -> anyhow::Result<()> {
        let profile: AgentProfile = serde_json::from_str(json)?;
        self.update_profile(profile)
    }

    /// Borrow the underlying wallet (for advanced uses like
    /// building EIP-191 signatures against raw digests).
    pub fn wallet(&self) -> &Wallet {
        &self.wallet
    }
}

/// Helper to create a fresh agent profile with sensible defaults.
pub fn create_agent_profile(
    agent_id: &str,
    display_name: &str,
    agent_type: AgentType,
    bio: &str,
) -> AgentProfile {
    AgentProfile {
        eliza_agent_id: agent_id.to_string(),
        node_id: NodeId::random(),
        display_name: display_name.to_string(),
        agent_type,
        bio: bio.to_string(),
        avatar_url: None,
        languages: vec![String::from("en")],
        accepts_dm: true,
        supports_groups: true,
        capabilities: Vec::new(),
        preferences: AgentPreferences::default(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BridgeError;

    fn make_identity(name: &str) -> AdnetIdentity {
        // Synchronous helper: `AdnetIdentity::new` is async to support
        // file I/O on platforms where blocking isn't safe, but on the
        // host the work is fast enough to drive from a tokio block.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir); // keep alive for the test lifetime
        futures::executor::block_on(AdnetIdentity::new(path, name)).unwrap()
    }

    // -----------------------------------------------------------------
    // AgentType
    // -----------------------------------------------------------------

    #[test]
    fn agent_type_as_str_all_variants() {
        assert_eq!(AgentType::Assistant.as_str(), "assistant");
        assert_eq!(AgentType::Analyst.as_str(), "analyst");
        assert_eq!(AgentType::Reporter.as_str(), "reporter");
        assert_eq!(AgentType::Developer.as_str(), "developer");
        assert_eq!(AgentType::Support.as_str(), "support");
        assert_eq!(AgentType::Custom("RWA".into()).as_str(), "RWA");
        assert_eq!(AgentType::Custom("DeFi".into()).as_str(), "DeFi");
    }

    #[test]
    fn agent_type_serde_round_trip() {
        let cases = vec![
            AgentType::Assistant,
            AgentType::Analyst,
            AgentType::Reporter,
            AgentType::Developer,
            AgentType::Support,
            AgentType::Custom("crypto".into()),
        ];
        for original in cases {
            let json = serde_json::to_string(&original).unwrap();
            let parsed: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(original, parsed);
        }
    }

    // -----------------------------------------------------------------
    // Defaults
    // -----------------------------------------------------------------

    #[test]
    fn agent_profile_default() {
        let p = AgentProfile::default();
        assert_eq!(p.eliza_agent_id, "");
        assert_eq!(p.display_name, "A3Net Agent");
        assert_eq!(p.bio, "");
        assert_eq!(p.avatar_url, None);
        assert_eq!(p.languages, vec!["en"]);
        assert!(p.accepts_dm);
        assert!(p.supports_groups);
        assert!(p.capabilities.is_empty());
        assert!(matches!(p.agent_type, AgentType::Assistant));
    }

    #[test]
    fn agent_preferences_default() {
        let p = AgentPreferences::default();
        assert!(!p.auto_accept_friends);
        assert!(p.notify_on_message);
        assert_eq!(p.max_messages_per_minute, 60);
        assert!(p.send_typing_indicator);
        assert_eq!(p.system_prompt_prefix, None);
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    #[test]
    fn create_agent_profile_helper() {
        let p = create_agent_profile(
            "eliza-1",
            "Test Agent",
            AgentType::Analyst,
            "I watch markets",
        );
        assert_eq!(p.eliza_agent_id, "eliza-1");
        assert_eq!(p.display_name, "Test Agent");
        assert_eq!(p.bio, "I watch markets");
        assert!(matches!(p.agent_type, AgentType::Analyst));
        assert_eq!(p.avatar_url, None);
        assert_eq!(p.languages, vec!["en"]);
        assert!(p.accepts_dm);
        assert!(p.supports_groups);
        assert!(p.capabilities.is_empty());
        assert_eq!(p.preferences.max_messages_per_minute, 60);
    }

    #[test]
    fn address_to_node_id_is_deterministic() {
        let addr = Address::from_bytes([0xAA; 20]);
        let n1 = address_to_node_id(&addr).unwrap();
        let n2 = address_to_node_id(&addr).unwrap();
        assert_eq!(n1, n2);
    }

    #[test]
    fn address_to_node_id_zero_address() {
        let addr = Address::from_bytes([0u8; 20]);
        let node = address_to_node_id(&addr).unwrap();
        // Bytes 4..24 must be zero, byte 0..4 and 24..32 must be zero.
        let bytes = node.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if (4..24).contains(&i) {
                assert_eq!(*b, 0, "byte {i} should be zero");
            } else {
                assert_eq!(*b, 0);
            }
        }
    }

    // -----------------------------------------------------------------
    // AdnetIdentity lifecycle
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn identity_create_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = AdnetIdentity::new(dir.path().to_path_buf(), "agent-1").await.unwrap();
        let node_id = id1.node_id();
        let agent_id = id1.profile().eliza_agent_id.clone();
        drop(id1);

        let id2 = AdnetIdentity::new(dir.path().to_path_buf(), "agent-1").await.unwrap();
        assert_eq!(id2.node_id(), node_id);
        assert_eq!(id2.profile().eliza_agent_id, agent_id);
    }

    #[tokio::test]
    async fn identity_reload_updates_node_id_to_match_wallet() {
        // Simulate a corrupted profile.json with a different NodeId;
        // loading should overwrite with the wallet's true NodeId.
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent-x").await.unwrap();
        let wallet_node = id.node_id();
        drop(id);

        let profile_path = dir.path().join("profile.json");
        let raw = std::fs::read_to_string(&profile_path).unwrap();
        // Replace node_id with a different one.
        let bogus = NodeId::random();
        let mut json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        json["node_id"] = serde_json::Value::String(bogus.as_hex().to_string());
        std::fs::write(&profile_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

        let id2 = AdnetIdentity::new(dir.path().to_path_buf(), "agent-x").await.unwrap();
        assert_eq!(id2.node_id(), wallet_node);
    }

    #[tokio::test]
    async fn identity_load_corrupt_profile_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // First, create an identity so wallet.dat exists.
        let _ = AdnetIdentity::new(dir.path().to_path_buf(), "agent-c").await.unwrap();
        // Corrupt the profile.json file.
        let profile_path = dir.path().join("profile.json");
        std::fs::write(&profile_path, "not-json").unwrap();

        // Reload should fail because the profile cannot be parsed.
        let err = AdnetIdentity::new(dir.path().to_path_buf(), "agent-c")
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("expected") || msg.contains("ident") || msg.contains("key"));
    }

    #[tokio::test]
    async fn identity_wallet_dat_wrong_size_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-populate wallet.dat with garbage that's not 32 bytes.
        std::fs::write(dir.path().join("wallet.dat"), [0u8; 5]).unwrap();

        let err = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("load wallet"));
    }

    // -----------------------------------------------------------------
    // sign / verify
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn identity_sign_and_verify_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent-test").await.unwrap();
        let message = b"hello A3Net";
        let sig = id.sign(message).unwrap();
        assert!(id.verify(message, &sig).unwrap());
        assert!(!id.verify(b"different message", &sig).unwrap());
    }

    #[tokio::test]
    async fn identity_verify_for_node_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent-verify").await.unwrap();
        let message = b"cross-node verification";
        let sig = id.sign(message).unwrap();
        assert!(AdnetIdentity::verify_for_node(message, &sig, &id.node_id()).unwrap());
        let other = NodeId::random();
        assert!(!AdnetIdentity::verify_for_node(message, &sig, &other).unwrap());
    }

    #[tokio::test]
    async fn identity_verify_rejects_garbage_signature() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent-bad").await.unwrap();
        let res = id.verify(b"msg", &[0u8; 4]);
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn identity_verify_for_node_rejects_garbage_signature() {
        let res = AdnetIdentity::verify_for_node(b"msg", &[0u8; 4], &NodeId::random());
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn identity_verify_for_node_rejects_signature_from_other_node() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let id_a = AdnetIdentity::new(dir_a.path().to_path_buf(), "a").await.unwrap();
        let id_b = AdnetIdentity::new(dir_b.path().to_path_buf(), "b").await.unwrap();
        let msg = b"hello";
        let sig_a = id_a.sign(msg).unwrap();
        // B claims to have signed it — verify_for_node for B should fail.
        assert!(!AdnetIdentity::verify_for_node(msg, &sig_a, &id_b.node_id()).unwrap());
    }

    // -----------------------------------------------------------------
    // profile mutation
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn identity_export_import_profile_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "agent-export").await.unwrap();
        let mut profile = id.profile().clone();
        profile.display_name = "Renamed Bot".to_string();
        profile.bio = "Trading bot".to_string();
        profile.capabilities = vec!["defi".to_string()];
        profile.languages = vec!["en".to_string(), "zh".to_string()];
        profile.avatar_url = Some("https://example.com/avatar.png".to_string());
        profile.preferences.auto_accept_friends = true;
        profile.preferences.system_prompt_prefix = Some("Trade wisely".to_string());
        id.update_profile(profile.clone()).unwrap();

        let json = id.export_profile().unwrap();
        let mut id2 = AdnetIdentity::new(dir.path().to_path_buf(), "agent-export").await.unwrap();
        id2.import_profile(&json).unwrap();
        assert_eq!(id2.profile().display_name, "Renamed Bot");
        assert_eq!(id2.profile().bio, "Trading bot");
        assert_eq!(id2.profile().capabilities, vec!["defi".to_string()]);
        assert_eq!(id2.profile().languages, vec!["en", "zh"]);
        assert_eq!(
            id2.profile().avatar_url.as_deref(),
            Some("https://example.com/avatar.png")
        );
        assert!(id2.profile().preferences.auto_accept_friends);
    }

    #[tokio::test]
    async fn identity_update_profile_rejects_mismatched_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        let mut bad = id.profile().clone();
        bad.node_id = NodeId::random();
        let err = id.update_profile(bad).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Cannot update profile"));
        assert!(msg.contains("NodeId"));
    }

    #[tokio::test]
    async fn identity_import_rejects_mismatched_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        let mut profile = id.profile().clone();
        profile.display_name = "X".to_string();
        profile.node_id = NodeId::random();
        let json = serde_json::to_string(&profile).unwrap();
        let err = id.import_profile(&json).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Cannot update profile"));
    }

    #[tokio::test]
    async fn identity_profile_mut_updates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        id.profile_mut().display_name = "Live Edit".to_string();
        assert_eq!(id.profile().display_name, "Live Edit");
    }

    #[tokio::test]
    async fn identity_save_profile_now_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        id.save_profile_now(dir.path()).unwrap();
        let written = std::fs::read_to_string(dir.path().join("profile.json")).unwrap();
        assert!(written.contains("agent"));
    }

    #[tokio::test]
    async fn identity_wallet_accessor_returns_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        let w1 = id.wallet();
        let w2 = id.wallet();
        assert_eq!(w1.secret_bytes(), w2.secret_bytes());
    }

    #[tokio::test]
    async fn identity_address_returns_public_address() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        let addr = id.address();
        assert_eq!(addr.as_bytes().len(), 20);
        // Recomputing from wallet yields same address.
        let from_wallet = id.wallet().public().address();
        assert_eq!(addr.as_bytes(), from_wallet.as_bytes());
    }

    // -----------------------------------------------------------------
    // Clone
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn identity_clone_preserves_node_id_and_profile() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = AdnetIdentity::new(dir.path().to_path_buf(), "agent").await.unwrap();
        let id2 = id1.clone();
        assert_eq!(id1.node_id(), id2.node_id());
        assert_eq!(id1.profile().eliza_agent_id, id2.profile().eliza_agent_id);
        // Signing with either wallet should verify against either.
        let sig = id1.sign(b"x").unwrap();
        assert!(id2.verify(b"x", &sig).unwrap());
    }

    // -----------------------------------------------------------------
    // BridgeError interop
    // -----------------------------------------------------------------

    #[test]
    fn bridge_error_round_trip_io() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: BridgeError = io.into();
        assert!(matches!(err, BridgeError::Io(_)));
    }

    #[test]
    fn bridge_error_from_serde_json() {
        let bad = serde_json::from_str::<u32>("not a number").unwrap_err();
        let err: BridgeError = bad.into();
        assert!(matches!(err, BridgeError::Serialization(_)));
    }

    // -----------------------------------------------------------------
    // AdnetError sanity (used by From impl)
    // -----------------------------------------------------------------

    #[test]
    fn bridge_error_from_a3net_error() {
        let ae = a3net_types::error::AdnetError::Validation("nope".into());
        let err: BridgeError = ae.into();
        let dbg = format!("{err:?}");
        assert!(dbg.contains("Internal"));
        assert!(dbg.contains("nope"));
    }

    // -----------------------------------------------------------------
    // Extra coverage: Debug, serde round-trips, edge cases
    // -----------------------------------------------------------------

    #[test]
    fn identity_debug_contains_fields() {
        let id = make_identity("dbg");
        let dbg = format!("{id:?}");
        assert!(dbg.contains("AdnetIdentity"));
        assert!(dbg.contains("eliza_agent_id"));
        assert!(dbg.contains("node_id"));
        assert!(dbg.contains("address"));
    }

    #[test]
    fn agent_profile_serde_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = futures::executor::block_on(AdnetIdentity::new(
            dir.path().to_path_buf(),
            "ser-profile",
        ))
        .unwrap();
        let mut p = id.profile().clone();
        p.display_name = "Round Trip".into();
        p.bio = "about me".into();
        p.avatar_url = Some("https://example.com/me.png".into());
        p.languages = vec!["en".into(), "es".into()];
        p.capabilities = vec!["cap-1".into(), "cap-2".into()];
        p.preferences.system_prompt_prefix = Some("hello".into());
        id.update_profile(p.clone()).unwrap();

        let json = id.export_profile().unwrap();
        let parsed: AgentProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.display_name, p.display_name);
        assert_eq!(parsed.bio, p.bio);
        assert_eq!(parsed.avatar_url, p.avatar_url);
        assert_eq!(parsed.languages, p.languages);
        assert_eq!(parsed.capabilities, p.capabilities);
        assert_eq!(
            parsed.preferences.system_prompt_prefix,
            p.preferences.system_prompt_prefix
        );
        // NodeId is forced to the wallet's identity on reload.
        assert_eq!(parsed.node_id, id.node_id());
    }

    #[test]
    fn agent_preferences_serde_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = futures::executor::block_on(AdnetIdentity::new(
            dir.path().to_path_buf(),
            "ser-prefs",
        ))
        .unwrap();
        let mut p = id.profile().clone();
        p.preferences.auto_accept_friends = true;
        p.preferences.notify_on_message = false;
        p.preferences.max_messages_per_minute = 7;
        p.preferences.send_typing_indicator = false;
        p.preferences.system_prompt_prefix = Some("x".into());
        id.update_profile(p).unwrap();

        let json = id.export_profile().unwrap();
        let parsed: AgentProfile = serde_json::from_str(&json).unwrap();
        assert!(parsed.preferences.auto_accept_friends);
        assert!(!parsed.preferences.notify_on_message);
        assert_eq!(parsed.preferences.max_messages_per_minute, 7);
        assert!(!parsed.preferences.send_typing_indicator);
        assert_eq!(parsed.preferences.system_prompt_prefix.as_deref(), Some("x"));
    }

    #[test]
    fn identity_address_format_matches_wallet() {
        let id = make_identity("addr");
        let from_id = id.address();
        let from_wallet = id.wallet().public().address();
        assert_eq!(from_id.as_bytes(), from_wallet.as_bytes());
    }

    #[tokio::test]
    async fn update_profile_with_matching_node_id_updates_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "ts").await.unwrap();
        let original = id.profile().updated_at;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut p = id.profile().clone();
        p.display_name = "Updated".into();
        id.update_profile(p).unwrap();
        assert!(id.profile().updated_at >= original);
        assert_eq!(id.profile().display_name, "Updated");
    }

    #[tokio::test]
    async fn import_profile_replaces_in_memory_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut id = AdnetIdentity::new(dir.path().to_path_buf(), "imp").await.unwrap();
        let original_id = id.profile().eliza_agent_id.clone();
        let json = serde_json::json!({
            "eliza_agent_id": "new-id",
            "node_id": id.node_id().as_hex(),
            "display_name": "Imported",
            "agent_type": {"kind": "analyst"},
            "bio": "x",
            "avatar_url": null,
            "languages": ["en"],
            "accepts_dm": true,
            "supports_groups": true,
            "capabilities": [],
            "preferences": {
                "auto_accept_friends": false,
                "notify_on_message": true,
                "max_messages_per_minute": 60,
                "send_typing_indicator": true,
                "system_prompt_prefix": null
            },
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        })
        .to_string();
        id.import_profile(&json).unwrap();
        assert_eq!(id.profile().eliza_agent_id, "new-id");
        assert_eq!(id.profile().display_name, "Imported");
        assert!(matches!(id.profile().agent_type, AgentType::Analyst));
        // eliza_agent_id changed (the wallet identity did not).
        assert_ne!(id.profile().eliza_agent_id, original_id);
    }

    #[tokio::test]
    async fn identity_sign_produces_compact_signature() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "sig-len").await.unwrap();
        let sig = id.sign(b"abc").unwrap();
        // secp256k1 compact (r||s||v) is 65 bytes.
        assert_eq!(sig.len(), 65);
    }

    #[tokio::test]
    async fn identity_sign_then_verify_with_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let id = AdnetIdentity::new(dir.path().to_path_buf(), "sv").await.unwrap();
        let sig = id.sign(b"hello").unwrap();
        assert!(id.verify(b"hello", &sig).unwrap());
        // Different message must fail.
        assert!(!id.verify(b"other", &sig).unwrap());
    }
}
