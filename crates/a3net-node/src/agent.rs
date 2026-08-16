//! AI-agent integration seam for [`crate::Node`].
//!
//! Feature-gated behind `agent` (= `dep:a3net-agent`). A3Net
//! does **not** ship an LLM client. This module exposes the
//! `Node`-side slots:
//!
//! - [`NodeAgentBridge`] — owns the live
//!   [`a3net_agent::ToolRegistry`] + optional [`a3net_agent::Agent`]
//!   handle. Built by [`crate::NodeBuilder::with_agent_bridge`].
//! - [`crate::Node::register_agent`] /
//!   [`crate::Node::register_tool`] — attach your host-side
//!   `ChatModel` / `Tool` implementations to the running node.
//! - [`crate::Node::agent_endpoint`] — returns an
//!   [`AgentEndpoint`] for the IPC layer.
//!
//! ## Example (host code, not A3Net itself)
//!
//! ```ignore
//! use a3net_agent::{ChatModel, Agent};
//! use a3net_node::Node;
//!
//! // Build a node and accept the default empty bridge.
//! let node = Node::builder(cfg).build_with_bus().await?;
//!
//! // Host plugs in its own LLM provider:
//! node.register_agent(Arc::new(my_openai_adapter), my_registry);
//!
//! // The CLI / IPC layer can now talk to `node.agent_endpoint()`.
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use a3net_agent as agent;
use parking_lot::Mutex;

// ---------------------------------------------------------------------------
// ACL types
// ---------------------------------------------------------------------------

/// Who is allowed to ask this node's agent a question over the P2P wire.
///
/// The default is [`DenyAll`] — nodes are **private by default** and the
/// operator must explicitly grant access per peer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "agent-v1", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "agent-v1", serde(rename_all = "snake_case"))]
pub enum AgentAclMode {
    /// No remote peer may invoke the agent.
    #[default]
    DenyAll,
    /// Every connected peer may invoke the agent (convenient for LAN / dev).
    AllowAll,
    /// Only the listed peer NodeIds may invoke the agent.
    AllowList(HashSet<String>),
}

impl AgentAclMode {
    /// Returns `true` if the given peer NodeId is permitted.
    pub fn permits(&self, peer_node_id: &str) -> bool {
        match self {
            AgentAclMode::DenyAll => false,
            AgentAclMode::AllowAll => true,
            AgentAclMode::AllowList(ids) => ids.contains(peer_node_id),
        }
    }

    /// Grant a peer access.
    pub fn grant(&mut self, peer_node_id: String) {
        match self {
            AgentAclMode::DenyAll => {
                *self = AgentAclMode::AllowList(std::iter::once(peer_node_id).collect());
            }
            AgentAclMode::AllowAll => {}
            AgentAclMode::AllowList(ids) => {
                ids.insert(peer_node_id);
            }
        }
    }

    /// Revoke a peer's access (no-op for [`AllowAll`]).
    pub fn revoke(&mut self, peer_node_id: &str) {
        if let AgentAclMode::AllowList(ids) = self {
            ids.remove(peer_node_id);
        }
    }
}

#[cfg(test)]
mod acl_tests {
    use super::*;

    #[test]
    fn deny_all_denies_everyone() {
        let mode = AgentAclMode::DenyAll;
        assert!(!mode.permits("alice"));
        assert!(!mode.permits("bob"));
    }

    #[test]
    fn allow_all_allows_everyone() {
        let mode = AgentAclMode::AllowAll;
        assert!(mode.permits("alice"));
        assert!(mode.permits("bob"));
    }

    #[test]
    fn allow_list_respects_allowlist() {
        let mut mode = AgentAclMode::DenyAll;
        mode.grant("alice".into());
        assert!(mode.permits("alice"));
        assert!(!mode.permits("bob"));
    }

    #[test]
    fn grant_on_allow_all_stays_allow_all() {
        let mut mode = AgentAclMode::AllowAll;
        mode.grant("alice".into());
        assert!(matches!(mode, AgentAclMode::AllowAll));
        assert!(mode.permits("alice"));
    }

    #[test]
    fn revoke_from_allow_list_works() {
        let mut mode = AgentAclMode::DenyAll;
        mode.grant("alice".into());
        mode.grant("bob".into());
        assert!(mode.permits("alice"));
        assert!(mode.permits("bob"));
        mode.revoke("alice");
        assert!(!mode.permits("alice"));
        assert!(mode.permits("bob"));
    }

    #[test]
    fn serde_roundtrip_deny_all() {
        #[cfg(feature = "agent-v1")]
        {
            let mode = AgentAclMode::DenyAll;
            let json = serde_json::to_string(&mode).unwrap();
            let back: AgentAclMode = serde_json::from_str(&json).unwrap();
            assert!(matches!(back, AgentAclMode::DenyAll));
        }
    }

    #[test]
    fn serde_roundtrip_allow_list() {
        #[cfg(feature = "agent-v1")]
        {
            let mut mode = AgentAclMode::DenyAll;
            mode.grant("alice".into());
            let json = serde_json::to_string(&mode).unwrap();
            let back: AgentAclMode = serde_json::from_str(&json).unwrap();
            assert!(back.permits("alice"));
        }
    }
}

// ---------------------------------------------------------------------------
// Bridge state
// ---------------------------------------------------------------------------

/// The bridge that holds an `AgentHandle` + tool registry
/// registered with the node. `Node::agent_endpoint()` returns
/// the inner state through [`AgentEndpoint`] so callers do
/// not need direct access to the bridge.
#[derive(Clone, Default)]
pub struct NodeAgentBridge {
    inner: Arc<Mutex<BridgeState>>,
}

#[derive(Default)]
struct BridgeState {
    model: Option<Arc<dyn agent::ChatModel>>,
    registry: agent::ToolRegistry,
    handle: Option<agent::AgentHandle>,
    /// P2P ACL for the `agent.v1` protocol.  Controls which remote peers
    /// may invoke this node's registered model.
    acl: AgentAclMode,
}

impl std::fmt::Debug for NodeAgentBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.inner.lock();
        f.debug_struct("NodeAgentBridge")
            .field("model_id", &g.model.as_ref().map(|m| m.model_id().to_string()))
            .field("tools", &g.registry.list())
            .field("has_handle", &g.handle.is_some())
            .field("acl", &g.acl)
            .finish()
    }
}

impl NodeAgentBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a [`agent::ChatModel`] implementation. If a
    /// model was previously registered, it is replaced. The
    /// bridge rebuilds its [`AgentHandle`] on the next
    /// [`Self::handle`] call so the new model is used in
    /// subsequent runs.
    pub fn register_model(&self, model: Arc<dyn agent::ChatModel>) {
        let mut g = self.inner.lock();
        g.model = Some(model);
        g.handle = None; // invalidate
    }

    /// Register a [`agent::Tool`]. Returns true if the slot
    /// was newly inserted, false if a tool with the same name
    /// already existed.
    pub fn register_tool<T: agent::Tool + 'static>(&self, tool: T) -> bool {
        let mut g = self.inner.lock();
        let replaced = g.registry.register(tool).is_some();
        g.handle = None;
        !replaced
    }

    /// Snapshot of the currently registered tool names.
    pub fn tools(&self) -> Vec<String> {
        self.inner.lock().registry.list()
    }

    /// Model id, if any.
    pub fn model_id(&self) -> Option<String> {
        self.inner
            .lock()
            .model
            .as_ref()
            .map(|m| m.model_id().to_string())
    }

    /// Lazily build (or rebuild) the [`AgentHandle`] and
    /// return a clone so the caller can drive the loop.
    pub fn handle(&self) -> Option<agent::AgentHandle> {
        let mut g = self.inner.lock();
        if g.handle.is_none() {
            let model = g.model.clone()?;
            let handle = agent::Agent::new(model, g.registry.clone()).handle();
            g.handle = Some(handle);
        }
        g.handle.clone()
    }

    /// Direct access to the tool registry. Used by
    /// `Node::register_tool` and integration tests.
    pub fn registry(&self) -> agent::ToolRegistryHandle {
        self.inner.lock().registry.handle()
    }

    /// Get a snapshot of the P2P ACL mode.
    pub fn acl(&self) -> AgentAclMode {
        self.inner.lock().acl.clone()
    }

    /// Set the P2P ACL mode.
    pub fn set_acl(&self, acl: AgentAclMode) {
        self.inner.lock().acl = acl;
    }

    /// Grant a remote peer the right to invoke the agent over P2P.
    pub fn grant_peer(&self, peer_node_id: String) {
        self.inner.lock().acl.grant(peer_node_id);
    }

    /// Revoke a remote peer's right to invoke the agent over P2P.
    pub fn revoke_peer(&self, peer_node_id: &str) {
        self.inner.lock().acl.revoke(peer_node_id);
    }

    /// Check if a remote peer is permitted to invoke the agent over P2P.
    /// Returns `true` if the ACL permits the peer.
    pub fn peer_permitted(&self, peer_node_id: &str) -> bool {
        self.inner.lock().acl.permits(peer_node_id)
    }

    /// Handle an inbound `agent.v1` chat request from a remote peer.
    ///
    /// This is the server-side of the P2P protocol. It checks the ACL,
    /// dispatches to the registered `ChatModel`, and returns the response
    /// envelope (or an error).
    ///
    /// `local_node_id` is used to populate the `from` field of the response.
    /// `peer_node_id` is the authenticated NodeId of the calling peer (from
    /// the transport layer).
    #[cfg(feature = "agent-v1")]
    pub async fn handle_v1_chat(
        &self,
        local_node_id: &str,
        peer_node_id: &str,
        body: agent::ChatRequest,
    ) -> a3net_transport::agent_v1::AgentV1ChatResponse {
        use a3net_transport::agent_v1::AgentV1ChatResponse;
        use agent::ChatMessage;

        // 1. ACL check
        if !self.peer_permitted(peer_node_id) {
            return AgentV1ChatResponse::error(
                uuid::Uuid::new_v4().to_string(),
                local_node_id.to_string(),
                "peer not permitted by agent ACL",
            );
        }

        // 2. Model check
        let Some(model) = self.inner.lock().model.clone() else {
            return AgentV1ChatResponse::error(
                uuid::Uuid::new_v4().to_string(),
                local_node_id.to_string(),
                "no agent model registered",
            );
        };

        // 3. Execute
        let result = model.complete(body).await;

        // 4. Package response
        match result {
            Ok(resp) => AgentV1ChatResponse::ok(
                uuid::Uuid::new_v4().to_string(),
                local_node_id.to_string(),
                resp,
            ),
            Err(e) => AgentV1ChatResponse::error(
                uuid::Uuid::new_v4().to_string(),
                local_node_id.to_string(),
                e.to_string(),
            ),
        }
    }
}

/// Public-facing agent endpoint — what the IPC / CLI layer
/// receives. Cheap to clone.
#[derive(Clone)]
pub struct AgentEndpoint {
    bridge: NodeAgentBridge,
}

impl std::fmt::Debug for AgentEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEndpoint").field("bridge", &self.bridge).finish()
    }
}

impl AgentEndpoint {
    pub(crate) fn new(bridge: NodeAgentBridge) -> Self {
        Self { bridge }
    }

    /// Whether a model has been registered.
    pub fn has_model(&self) -> bool {
        self.bridge.model_id().is_some()
    }

    /// Model id, if any.
    pub fn model_id(&self) -> Option<String> {
        self.bridge.model_id()
    }

    /// List tool names currently registered.
    pub fn tools(&self) -> Vec<String> {
        self.bridge.tools()
    }

    /// Get an [`agent::AgentHandle`], building one on demand.
    pub fn handle(&self) -> Option<agent::AgentHandle> {
        self.bridge.handle()
    }

    /// Direct access to the underlying bridge (for advanced
    /// uses — most callers stay on [`Self::handle`] /
    /// [`Self::tools`]).
    pub fn bridge(&self) -> NodeAgentBridge {
        self.bridge.clone()
    }

    /// Borrow the tool registry.
    pub fn registry(&self) -> agent::ToolRegistryHandle {
        self.bridge.registry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a3net_agent::{mock::MockChatModel, mock::MockTool};

    #[test]
    fn bridge_starts_empty() {
        let bridge = NodeAgentBridge::new();
        assert_eq!(bridge.tools(), Vec::<String>::new());
        assert_eq!(bridge.model_id(), None);
    }

    #[test]
    fn register_tool_then_list() {
        let bridge = NodeAgentBridge::new();
        bridge.register_tool(MockTool::echo("echo"));
        bridge.register_tool(MockTool::echo("reverse"));
        assert_eq!(bridge.tools(), vec!["echo".to_string(), "reverse".to_string()]);
    }

    #[test]
    fn register_model_then_handle() {
        let bridge = NodeAgentBridge::new();
        bridge.register_tool(MockTool::echo("echo"));
        bridge.register_model(Arc::new(MockChatModel::text("ok")));
        let handle = bridge.handle().expect("handle built");
        assert_eq!(handle.model_id(), "mock-text/1");
        assert_eq!(handle.tools(), vec!["echo".to_string()]);
    }

    #[test]
    fn endpoint_clone_preserves_state() {
        let bridge = NodeAgentBridge::new();
        bridge.register_tool(MockTool::echo("a"));
        bridge.register_model(Arc::new(MockChatModel::text("hi")));
        let ep1 = AgentEndpoint::new(bridge.clone());
        let ep2 = AgentEndpoint::new(bridge);
        assert_eq!(ep1.tools(), vec!["a".to_string()]);
        assert_eq!(ep2.tools(), vec!["a".to_string()]);
        assert_eq!(ep1.model_id().as_deref(), Some("mock-text/1"));
    }
}
