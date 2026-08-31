//! Node executor trait and registry.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;

/// Executes a single node given its data input values.
///
/// Implementations return the values produced on the node's data output pins,
/// keyed by pin id. The shared [`ExecutionContext`] provides access to the
/// registry, LLM clients, audit logging and the transaction log.
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    /// The node kind this executor handles (e.g. `Start`, `Add`, `Branch`).
    fn kind(&self) -> &str;

    /// Executes the node and returns its data output values.
    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>>;
}

/// A registry of node executors keyed by node kind.
#[derive(Default)]
pub struct NodeRegistry {
    executors: HashMap<String, Box<dyn NodeExecutor>>,
}

impl NodeRegistry {
    /// Creates a registry pre-populated with the built-in executors.
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        registry.register(Box::new(crate::execution::nodes::StartExecutor));
        registry.register(Box::new(crate::execution::nodes::EndExecutor));
        registry.register(Box::new(crate::execution::nodes::AddExecutor));
        registry.register(Box::new(crate::execution::nodes::SubtractExecutor));
        registry.register(Box::new(crate::execution::nodes::MultiplyExecutor));
        registry.register(Box::new(crate::execution::nodes::DivideExecutor));
        registry.register(Box::new(crate::execution::nodes::BranchExecutor));
        registry.register(Box::new(crate::execution::nodes::CallLlmExecutor));
        registry.register(Box::new(crate::execution::nodes::ToolExecutor));
        registry.register(Box::new(crate::execution::nodes::ValidatorExecutor));
        registry.register(Box::new(crate::execution::nodes::JudgeExecutor));
        registry.register(Box::new(crate::execution::nodes::abstract_node::AbstractExecutor));
        registry.register(Box::new(crate::execution::nodes::CallFunctionExecutor));
        registry.register(Box::new(crate::execution::nodes::FunctionEntryExecutor));
        registry.register(Box::new(crate::execution::nodes::FunctionExitExecutor));
        registry
    }

    /// Registers an executor, replacing any existing one with the same kind.
    pub fn register(&mut self, executor: Box<dyn NodeExecutor>) {
        self.executors.insert(executor.kind().to_string(), executor);
    }

    /// Returns the executor for the given node kind, if registered.
    pub fn get(&self, kind: &str) -> Option<&dyn NodeExecutor> {
        self.executors.get(kind).map(|e| e.as_ref())
    }

    /// Returns all registered node kinds.
    pub fn kinds(&self) -> Vec<String> {
        self.executors.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_include_abstract_kind() {
        let registry = NodeRegistry::with_builtins();
        assert!(registry.get("Abstract").is_some());
        assert!(registry.kinds().contains(&"Abstract".to_string()));
    }
}
