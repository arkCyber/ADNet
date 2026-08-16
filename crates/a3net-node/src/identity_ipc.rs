//! Identity + contacts IPC service.
//!
//! Exposes the local node's [`NodeIdentityStore`] and
//! [`ContactsManager`] over a JSON-RPC 2.0 Unix socket so the
//! `a3net` CLI (and any other external consumer) can read and
//! mutate the local state without linking against the full
//! `a3net-node` build graph.
//!
//! ## Methods
//!
//! All methods take a JSON object as `params` and return a JSON
//! object as `result`. Errors are surfaced as JSON-RPC errors
//! with a stable `message` field; see [`err_string`] for the
//! mapping.
//!
//! ### Identity
//!
//! - `identity.get` — returns the current [`NodeIdentity`] as
//!   JSON. No parameters.
//! - `identity.set_email` — `{ "email": "..." }`. Re-validates
//!   email format on the server side.
//! - `identity.set_nickname` — `{ "nickname": "..." }`.
//! - `identity.set_description` — `{ "description": "..." }`.
//! - `identity.set_avatar` — `{ "kind": "url" | "data", ... }`.
//! - `identity.set_wallet` — `{ "wallet": "0x..." }`.
//! - `identity.set_dns_node_id` — `{ "dns_node_id": "..." }`.
//!
//! ### Contacts
//!
//! - `contacts.list` — returns every [`ContactEntry`] as JSON.
//! - `contacts.get` — `{ "node_id": "..." }`. Returns the entry
//!   or `null` if absent.
//! - `contacts.add_manual` — `{ "node_id": "...", "nickname": "..." }`.
//! - `contacts.remove` — `{ "node_id": "..." }`. Returns the
//!   removed entry or `null`.
//! - `contacts.rename` — `{ "node_id": "...", "nickname": "..." }`.
//! - `contacts.set_blocked` — `{ "node_id": "...", "blocked": true|false }`.
//! - `contacts.bump_reputation` — `{ "node_id": "...", "delta": 50 }`.
//! - `contacts.set_reputation` — `{ "node_id": "...", "reputation": 500 }`.
//! - `contacts.get_reputation` — `{ "node_id": "..." }`.
//! - `contacts.reputation_summary` — returns [`ReputationSummary`]
//!   as JSON.
//!
//! ### Profile
//!
//! - `profile.html` — returns the rendered profile page as a
//!   `text/html` string (consumers can write to `/var/www/`).
//! - `profile.card_json` — returns the [`NodeIdentityCard`] as
//!   JSON.
//!
//! ## Validation
//!
//! Every input field is re-validated against the same invariants
//! the in-process setters enforce. A request that fails
//! validation produces a JSON-RPC error with the validation
//! message; the local state is never partially updated.

use std::path::PathBuf;
use std::sync::Arc;

use a3net_types::{ContactEntry, ContactsListError, NodeId, NodeIdentityError};

use crate::contacts_manager::ReputationSummary;
use a3net_types::Validate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use a3net_ipc::server::{JsonRpcServer, JsonRpcServerHandle, RpcHandler};

use crate::contacts_manager::ContactsManager;
use crate::node::Node;
use crate::node_identity_store::NodeIdentityStore;

/// Default socket path for the identity service. Mirrors the
/// pattern used by `BlobsIpcConfig::default()`.
pub fn default_socket_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push("a3net_identity.sock");
    p
}

/// Configuration for the identity IPC service.
#[derive(Debug, Clone)]
pub struct IdentityIpcConfig {
    pub socket_path: PathBuf,
}

impl Default for IdentityIpcConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
        }
    }
}

/// Identity + contacts IPC service.
///
/// Holds cheap-clonable references to the local node's
/// [`NodeIdentityStore`] and [`ContactsManager`], plus a
/// [`Node`] for the profile-html endpoint. The whole struct is
/// `Arc`-wrapped before being passed to [`JsonRpcServer`].
pub struct IdentityIpcService {
    cfg: IdentityIpcConfig,
    node: Arc<Node>,
}

impl IdentityIpcService {
    pub fn new(cfg: IdentityIpcConfig, node: Arc<Node>) -> Self {
        Self { cfg, node }
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.cfg.socket_path
    }

    /// Start the JSON-RPC server.
    pub async fn serve(self: Arc<Self>) -> Result<JsonRpcServerHandle, String> {
        JsonRpcServer::start(self.cfg.socket_path.clone(), self).await
    }

    fn identity(&self) -> &NodeIdentityStore {
        self.node.identity()
    }

    fn contacts(&self) -> &ContactsManager {
        self.node.contacts()
    }
}

#[async_trait::async_trait]
impl RpcHandler for IdentityIpcService {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            // Identity
            "identity.get" => self.identity_get().await,
            "identity.set_email" => self.identity_set_email(params).await,
            "identity.set_nickname" => self.identity_set_nickname(params).await,
            "identity.set_description" => self.identity_set_description(params).await,
            "identity.set_avatar" => self.identity_set_avatar(params).await,
            "identity.set_wallet" => self.identity_set_wallet(params).await,
            "identity.set_dns_node_id" => self.identity_set_dns_node_id(params).await,
            // Contacts
            "contacts.list" => self.contacts_list().await,
            "contacts.get" => self.contacts_get(params).await,
            "contacts.add_manual" => self.contacts_add_manual(params).await,
            "contacts.remove" => self.contacts_remove(params).await,
            "contacts.rename" => self.contacts_rename(params).await,
            "contacts.set_blocked" => self.contacts_set_blocked(params).await,
            "contacts.bump_reputation" => self.contacts_bump_reputation(params).await,
            "contacts.set_reputation" => self.contacts_set_reputation(params).await,
            "contacts.get_reputation" => self.contacts_get_reputation(params).await,
            "contacts.reputation_summary" => self.contacts_reputation_summary().await,
            // Profile
            "profile.html" => self.profile_html().await,
            "profile.card_json" => self.profile_card_json().await,
            _ => Err(format!("unknown method: {method}")),
        }
    }
}

// ── Identity handlers ──────────────────────────────────────────────

impl IdentityIpcService {
    async fn identity_get(&self) -> Result<Value, String> {
        let id = self.identity().snapshot();
        serde_json::to_value(&id).map_err(err_string)
    }

    async fn identity_set_email(&self, params: Value) -> Result<Value, String> {
        let p: SetEmailParams = serde_json::from_value(params).map_err(err_string)?;
        match self.identity().set_email(p.email) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn identity_set_nickname(&self, params: Value) -> Result<Value, String> {
        let p: SetNicknameParams = serde_json::from_value(params).map_err(err_string)?;
        match self.identity().set_nickname(p.nickname) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn identity_set_description(&self, params: Value) -> Result<Value, String> {
        let p: SetDescriptionParams = serde_json::from_value(params).map_err(err_string)?;
        match self.identity().set_description(p.description) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn identity_set_avatar(&self, params: Value) -> Result<Value, String> {
        let avatar: a3net_types::Avatar =
            serde_json::from_value(params).map_err(err_string)?;
        match self.identity().set_avatar(avatar) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn identity_set_wallet(&self, params: Value) -> Result<Value, String> {
        let p: SetWalletParams = serde_json::from_value(params).map_err(err_string)?;
        let wallet = a3net_types::WalletAddress::from_hex(&p.wallet)
            .map_err(|e| format!("invalid wallet hex: {e}"))?;
        match self.identity().set_wallet_address(wallet) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn identity_set_dns_node_id(&self, params: Value) -> Result<Value, String> {
        let p: SetDnsParams = serde_json::from_value(params).map_err(err_string)?;
        let dns = a3net_types::DnsNodeId::parse(&p.dns_node_id)
            .map_err(|e| format!("invalid dns_node_id: {e}"))?;
        match self.identity().set_dns_node_id(dns) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }
}

// ── Contacts handlers ──────────────────────────────────────────────

impl IdentityIpcService {
    async fn contacts_list(&self) -> Result<Value, String> {
        let entries = self.contacts().snapshot();
        serde_json::to_value(&entries).map_err(err_string)
    }

    async fn contacts_get(&self, params: Value) -> Result<Value, String> {
        let p: NodeIdParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().get(&id) {
            Some(entry) => serde_json::to_value(&entry).map_err(err_string),
            None => Ok(Value::Null),
        }
    }

    async fn contacts_add_manual(&self, params: Value) -> Result<Value, String> {
        let p: AddContactParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().upsert_manual(id, p.nickname) {
            Ok(entry) => serde_json::to_value(&entry).map_err(err_string),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_remove(&self, params: Value) -> Result<Value, String> {
        let p: NodeIdParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().remove(&id) {
            Ok(entry) => serde_json::to_value(&entry).map_err(err_string),
            Err(ContactsListError::NotFound(_)) => Ok(Value::Null),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_rename(&self, params: Value) -> Result<Value, String> {
        let p: RenameParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().rename(&id, p.nickname) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_set_blocked(&self, params: Value) -> Result<Value, String> {
        let p: SetBlockedParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().set_blocked(&id, p.blocked) {
            Ok(()) => Ok(json!({ "ok": true })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_bump_reputation(&self, params: Value) -> Result<Value, String> {
        let p: BumpReputationParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().bump_reputation(&id, p.delta) {
            Ok(new_value) => Ok(json!({ "nodeId": id, "reputation": new_value })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_set_reputation(&self, params: Value) -> Result<Value, String> {
        let p: SetReputationParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        match self.contacts().set_reputation(&id, p.reputation) {
            Ok(new_value) => Ok(json!({ "nodeId": id, "reputation": new_value })),
            Err(e) => Err(err_string(e)),
        }
    }

    async fn contacts_get_reputation(&self, params: Value) -> Result<Value, String> {
        let p: NodeIdParams = serde_json::from_value(params).map_err(err_string)?;
        let id = parse_node_id(&p.node_id)?;
        Ok(match self.contacts().get_reputation(&id) {
            Some(r) => json!({ "nodeId": id, "reputation": r }),
            None => Value::Null,
        })
    }

    async fn contacts_reputation_summary(&self) -> Result<Value, String> {
        let s: ReputationSummary = self.contacts().reputation_summary();
        serde_json::to_value(&s).map_err(err_string)
    }
}

// ── Profile handlers ───────────────────────────────────────────────

impl IdentityIpcService {
    async fn profile_html(&self) -> Result<Value, String> {
        let html = self.node.render_profile_html();
        Ok(json!({
            "contentType": "text/html; charset=utf-8",
            "body": html,
            "byteLength": html.len(),
        }))
    }

    async fn profile_card_json(&self) -> Result<Value, String> {
        let card = self.node.identity_card();
        serde_json::to_value(&card).map_err(err_string)
    }
}

// ── Param types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct SetEmailParams {
    email: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetNicknameParams {
    nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetDescriptionParams {
    description: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetWalletParams {
    wallet: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetDnsParams {
    dns_node_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NodeIdParams {
    node_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AddContactParams {
    node_id: String,
    nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RenameParams {
    node_id: String,
    nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SetBlockedParams {
    node_id: String,
    blocked: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct BumpReputationParams {
    node_id: String,
    delta: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct SetReputationParams {
    node_id: String,
    reputation: u32,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn parse_node_id(s: &str) -> Result<NodeId, String> {
    NodeId::from_hex(s).map_err(|e| format!("invalid node_id: {e}"))
}

fn err_string<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// We never use Validate in this module, but a deferred trait
// import keeps the API surface uniform with the rest of the IPC
// crate so future validators can drop in.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<IdentityIpcService>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeConfig;

    async fn build_node() -> (tempfile::TempDir, Arc<Node>) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::new(tmp.path(), NodeId::random());
        let node = Arc::new(Node::builder(cfg).build().await.unwrap());
        (tmp, node)
    }

    #[tokio::test]
    async fn round_trip_identity_email() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));

        // Set email via the IPC handler.
        let res = svc
            .handle(
                "identity.set_email",
                json!({ "email": "alice@example.com" }),
            )
            .await
            .unwrap();
        assert_eq!(res, json!({"ok": true}));

        // Read it back.
        let v = svc.handle("identity.get", json!({})).await.unwrap();
        assert_eq!(v["email"], "alice@example.com");
    }

    #[tokio::test]
    async fn identity_set_email_invalid() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let err = svc
            .handle("identity.set_email", json!({ "email": "not-an-email" }))
            .await
            .unwrap_err();
        assert!(err.contains("email"), "got: {err}");
    }

    #[tokio::test]
    async fn contacts_add_then_get() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let id = NodeId::random();
        let added = svc
            .handle(
                "contacts.add_manual",
                json!({ "node_id": id.as_hex(), "nickname": "alice" }),
            )
            .await
            .unwrap();
        assert_eq!(added["nickname"], "alice");

        let got = svc
            .handle(
                "contacts.get",
                json!({ "node_id": id.as_hex() }),
            )
            .await
            .unwrap();
        assert_eq!(got["nickname"], "alice");
    }

    #[tokio::test]
    async fn contacts_reputation_bump() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let id = NodeId::random();
        svc.handle(
            "contacts.add_manual",
            json!({ "node_id": id.as_hex(), "nickname": "bob" }),
        )
        .await
        .unwrap();
        let res = svc
            .handle(
                "contacts.bump_reputation",
                json!({ "node_id": id.as_hex(), "delta": 100 }),
            )
            .await
            .unwrap();
        assert_eq!(res["reputation"], 200); // 100 default + 100 bump
    }

    #[tokio::test]
    async fn contacts_set_reputation_out_of_range() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let id = NodeId::random();
        svc.handle(
            "contacts.add_manual",
            json!({ "node_id": id.as_hex(), "nickname": "x" }),
        )
        .await
        .unwrap();
        let err = svc
            .handle(
                "contacts.set_reputation",
                json!({ "node_id": id.as_hex(), "reputation": 5_000 }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("reputation"), "got: {err}");
    }

    #[tokio::test]
    async fn contacts_reputation_summary_default() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let res = svc
            .handle("contacts.reputation_summary", json!({}))
            .await
            .unwrap();
        assert_eq!(res["contacts"], 0);
    }

    #[tokio::test]
    async fn profile_html_returns_html_body() {
        let (_tmp, node) = build_node().await;
        node.identity().set_nickname("tester").unwrap();
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let res = svc.handle("profile.html", json!({})).await.unwrap();
        assert_eq!(res["contentType"], "text/html; charset=utf-8");
        assert!(res["body"].as_str().unwrap().contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn unknown_method_errors() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let err = svc.handle("does.not.exist", json!({})).await.unwrap_err();
        assert!(err.contains("unknown method"));
    }

    #[tokio::test]
    async fn invalid_node_id() {
        let (_tmp, node) = build_node().await;
        let svc = Arc::new(IdentityIpcService::new(IdentityIpcConfig::default(), node));
        let err = svc
            .handle(
                "contacts.add_manual",
                json!({ "node_id": "deadbeef", "nickname": "x" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("node_id"));
    }
}
