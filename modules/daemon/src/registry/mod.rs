//! Resource registry for tools, functions and node executors.

pub mod library;
pub mod node;
pub mod tool;
pub mod tools;

use std::collections::HashMap;
use std::sync::Arc;

use metteur_shared::model::function::{FunctionEntry, FunctionSource};
use parking_lot::RwLock;

use crate::error::DaemonResult;

pub use node::{NodeExecutor, NodeRegistry};
pub use tool::Tool;

/// The central registry of daemon resources.
///
/// Tools and functions live behind internal locks so hosts such as the MCP
/// manager, the addon system and workspace open/close can register or retire
/// them at runtime.
#[derive(Default)]
pub struct Registry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    nodes: NodeRegistry,
    functions: RwLock<HashMap<String, FunctionEntry>>,
}

impl Registry {
    /// Creates a registry pre-populated with built-in resources.
    pub fn with_builtins() -> Self {
        let registry = Self {
            tools: RwLock::new(HashMap::new()),
            nodes: NodeRegistry::with_builtins(),
            functions: RwLock::new(HashMap::new()),
        };
        for tool in [
            Arc::new(tools::fs_tools::ReadFile) as Arc<dyn Tool>,
            Arc::new(tools::fs_tools::WriteFile),
            Arc::new(tools::fs_tools::ListDirectory),
            Arc::new(tools::fs_tools::SearchFile),
            Arc::new(tools::command::ExecuteCommand),
            Arc::new(tools::deps::GetDependencies),
            Arc::new(tools::subagent::SpawnSubAgent),
            Arc::new(tools::lsp_tools::CheckDiagnostics),
            Arc::new(tools::lsp_tools::GetHover),
            Arc::new(tools::lsp_tools::FindDefinition),
            Arc::new(tools::replan::ReplanBlueprint),
        ] {
            registry.register_tool(tool);
        }
        registry.register_function(library::builtin_chain_of_thought());
        registry
    }

    /// Registers a tool, replacing any existing one with the same name.
    pub fn register_tool(&self, tool: Arc<dyn Tool>) {
        self.tools.write().insert(tool.name().to_string(), tool);
    }

    /// Removes a tool by name, returning it when it was registered.
    pub fn unregister_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.write().remove(name)
    }

    /// Returns a tool by name, if registered.
    pub fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().get(name).cloned()
    }

    /// Returns all registered tools.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.read().values().cloned().collect()
    }

    /// Returns all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.read().keys().cloned().collect()
    }

    /// Registers a function, replacing any existing one with the same name.
    pub fn register_function(&self, entry: FunctionEntry) {
        self.functions.write().insert(entry.name.clone(), entry);
    }

    /// Removes a function by name, returning it when registered.
    pub fn unregister_function(&self, name: &str) -> Option<FunctionEntry> {
        self.functions.write().remove(name)
    }

    /// Returns a function by name.
    pub fn function(&self, name: &str) -> Option<FunctionEntry> {
        self.functions.read().get(name).cloned()
    }

    /// Returns a function by its stable id.
    pub fn function_by_id(&self, id: uuid::Uuid) -> Option<FunctionEntry> {
        self.functions.read().values().find(|f| f.id == id).cloned()
    }

    /// Returns all registered functions, sorted by name.
    pub fn functions(&self) -> Vec<FunctionEntry> {
        let mut out: Vec<FunctionEntry> = self.functions.read().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Registers all functions stored in `db`, tagged with `source`.
    pub fn load_functions(&self, db: &crate::persistence::Db, source: FunctionSource) -> DaemonResult<()> {
        for mut entry in crate::registry::library::load_all(db)? {
            entry.source = source;
            self.register_function(entry);
        }
        Ok(())
    }

    /// Returns the node executor for the given kind, if registered.
    pub fn node_executor(&self, kind: &str) -> Option<&dyn NodeExecutor> {
        self.nodes.get(kind)
    }

    /// Returns all registered node kinds.
    pub fn node_kinds(&self) -> Vec<String> {
        self.nodes.kinds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DaemonResult;
    use crate::execution::context::ExecutionContext;
    use async_trait::async_trait;
    use metteur_shared::Value;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "Dummy"
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn dynamic_register_and_unregister() {
        let registry = Registry::with_builtins();
        assert!(registry.tool("Dummy").is_none());
        registry.register_tool(Arc::new(DummyTool));
        assert!(registry.tool("Dummy").is_some());
        assert!(registry.unregister_tool("Dummy").is_some());
        assert!(registry.tool("Dummy").is_none());
        assert!(registry.unregister_tool("Dummy").is_none());
    }

    #[test]
    fn builtin_tools_are_registered() {
        let registry = Registry::with_builtins();
        for name in ["ReadFile", "WriteFile", "ExecuteCommand", "SpawnSubAgent"] {
            assert!(registry.tool(name).is_some(), "{name} should be registered");
        }
    }
}
