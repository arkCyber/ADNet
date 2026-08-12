//! Thread-safe tool registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::{AgentError, AgentResult};
use crate::tool::{BoxedTool, Tool, ToolContext, ToolDescriptor, ToolResult};

/// Concurrent registry of named tools. `AdNetNode` exposes one
/// of these as the source of truth for what the AI agent can
/// call.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    inner: Arc<RwLock<BTreeMap<String, BoxedTool>>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.inner.read().keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a tool. Returns the previous instance
    /// (if any) so callers can chain `.or_insert_with(...)` and
    /// assert conflict-free registration.
    pub fn register<T: Tool + 'static>(&self, tool: T) -> Option<BoxedTool> {
        self.register_boxed(Arc::new(tool))
    }

    pub fn register_boxed(&self, tool: BoxedTool) -> Option<BoxedTool> {
        self.inner.write().insert(tool.name().to_string(), tool)
    }

    pub fn unregister(&self, name: &str) -> Option<BoxedTool> {
        self.inner.write().remove(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.inner.read().contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    pub fn list(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }

    /// Snapshot of every tool's [`ToolDescriptor`]. Used to
    /// build the next [`crate::ChatRequest`].
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.inner
            .read()
            .values()
            .map(|t| t.descriptor())
            .collect()
    }

    /// Invoke a tool by name. Returns
    /// [`AgentError::UnknownTool`] when the name is not in the
    /// registry.
    pub async fn invoke(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> AgentResult<serde_json::Value> {
        let tool = {
            let r = self.inner.read();
            r.get(name).cloned()
        };
        let tool = tool.ok_or_else(|| AgentError::UnknownTool(name.to_string()))?;
        tool.invoke(args, ctx).await.map_err(|e| AgentError::Tool(e.to_string()))
    }

    /// Resolve `(name → &dyn Tool)` so callers can introspect
    /// the schema.
    pub fn get(&self, name: &str) -> Option<BoxedTool> {
        self.inner.read().get(name).cloned()
    }

    pub fn handle(&self) -> ToolRegistryHandle {
        ToolRegistryHandle {
            inner: self.inner.clone(),
        }
    }
}

/// Cheap, cloneable, sendable handle that exposes the registry's
/// primitives without owning the registry.
#[derive(Clone, Default)]
pub struct ToolRegistryHandle {
    inner: Arc<RwLock<BTreeMap<String, BoxedTool>>>,
}

impl ToolRegistryHandle {
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
    pub fn list(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.inner
            .read()
            .values()
            .map(|t| t.descriptor())
            .collect()
    }
    pub async fn invoke(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> AgentResult<serde_json::Value> {
        let tool = self.inner.read().get(name).cloned();
        let tool = tool.ok_or_else(|| AgentError::UnknownTool(name.to_string()))?;
        tool.invoke(args, ctx).await.map_err(|e| AgentError::Tool(e.to_string()))
    }
}

impl std::fmt::Debug for ToolRegistryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistryHandle")
            .field("tools", &self.inner.read().keys().collect::<Vec<_>>())
            .finish()
    }
}

// Reference the typed result aliases so they don't get
// flagged as unused when only `ToolResult` is re-used by callers.
#[allow(dead_code)]
fn _aliases() -> ToolResult {
    Ok(serde_json::json!({}))
}
