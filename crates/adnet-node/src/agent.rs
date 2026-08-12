//! AI-agent integration seam for [`crate::Node`].
//!
//! Feature-gated behind `agent` (= `dep:adnet-agent`). ADNet
//! does **not** ship an LLM client. This module exposes the
//! `Node`-side slots:
//!
//! - [`NodeAgentBridge`] — owns the live
//!   [`adnet_agent::ToolRegistry`] + optional [`adnet_agent::Agent`]
//!   handle. Built by [`crate::NodeBuilder::with_agent_bridge`].
//! - [`crate::Node::register_agent`] /
//!   [`crate::Node::register_tool`] — attach your host-side
//!   `ChatModel` / `Tool` implementations to the running node.
//! - [`crate::Node::agent_endpoint`] — returns an
//!   [`AgentEndpoint`] for the IPC layer.
//!
//! ## Example (host code, not ADNet itself)
//!
//! ```ignore
//! use adnet_agent::{ChatModel, Agent};
//! use adnet_node::Node;
//!
//! // Build a node and accept the default empty bridge.
//! let node = Node::builder(cfg).build_with_bus().await?;
//!
//! // Host plugs in its own LLM provider:
//! node.register_agent(Arc::new(my_openai_adapter), my_registry);
//!
//! // The CLI / IPC layer can now talk to `node.agent_endpoint()`.
//! ```

use std::sync::Arc;

use adnet_agent as agent;
use parking_lot::Mutex;

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
}

impl std::fmt::Debug for NodeAgentBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.inner.lock();
        f.debug_struct("NodeAgentBridge")
            .field("model_id", &g.model.as_ref().map(|m| m.model_id().to_string()))
            .field("tools", &g.registry.list())
            .field("has_handle", &g.handle.is_some())
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
    use adnet_agent::{mock::MockChatModel, mock::MockTool};

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
