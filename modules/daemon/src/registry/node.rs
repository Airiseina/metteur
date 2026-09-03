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
        use crate::execution::nodes as n;
        let mut registry = Self::default();
        registry.register(Box::new(n::StartExecutor));
        registry.register(Box::new(n::EndExecutor));
        registry.register(Box::new(n::AddExecutor));
        registry.register(Box::new(n::SubtractExecutor));
        registry.register(Box::new(n::MultiplyExecutor));
        registry.register(Box::new(n::DivideExecutor));
        registry.register(Box::new(n::BranchExecutor));
        registry.register(Box::new(n::CallLlmExecutor));
        registry.register(Box::new(n::ToolExecutor));
        registry.register(Box::new(n::ValidatorExecutor));
        registry.register(Box::new(n::JudgeExecutor));
        registry.register(Box::new(n::AbstractExecutor));
        registry.register(Box::new(n::CallFunctionExecutor));
        registry.register(Box::new(n::FunctionEntryExecutor));
        registry.register(Box::new(n::FunctionExitExecutor));
        // Extended math.
        registry.register(Box::new(n::ModuloExecutor));
        registry.register(Box::new(n::PowerExecutor));
        registry.register(Box::new(n::MinExecutor));
        registry.register(Box::new(n::MaxExecutor));
        registry.register(Box::new(n::AbsExecutor));
        registry.register(Box::new(n::RoundExecutor));
        // Comparison and logic.
        registry.register(Box::new(n::EqualExecutor));
        registry.register(Box::new(n::NotEqualExecutor));
        registry.register(Box::new(n::GreaterExecutor));
        registry.register(Box::new(n::LessExecutor));
        registry.register(Box::new(n::GreaterEqualExecutor));
        registry.register(Box::new(n::LessEqualExecutor));
        registry.register(Box::new(n::AndExecutor));
        registry.register(Box::new(n::OrExecutor));
        registry.register(Box::new(n::XorExecutor));
        registry.register(Box::new(n::NotExecutor));
        // Strings and conversion.
        registry.register(Box::new(n::ConcatExecutor));
        registry.register(Box::new(n::LengthExecutor));
        registry.register(Box::new(n::UpperExecutor));
        registry.register(Box::new(n::LowerExecutor));
        registry.register(Box::new(n::TrimExecutor));
        registry.register(Box::new(n::ContainsExecutor));
        registry.register(Box::new(n::ReplaceExecutor));
        registry.register(Box::new(n::SubstringExecutor));
        registry.register(Box::new(n::ToStringExecutor));
        registry.register(Box::new(n::ToIntExecutor));
        registry.register(Box::new(n::ToFloatExecutor));
        registry.register(Box::new(n::ToBoolExecutor));
        registry.register(Box::new(n::ToJsonExecutor));
        registry.register(Box::new(n::ParseJsonExecutor));
        // Collections.
        registry.register(Box::new(n::ListCreateExecutor));
        registry.register(Box::new(n::ListAppendExecutor));
        registry.register(Box::new(n::ListGetExecutor));
        registry.register(Box::new(n::ListLengthExecutor));
        registry.register(Box::new(n::ListContainsExecutor));
        registry.register(Box::new(n::JsonGetExecutor));
        registry.register(Box::new(n::JsonSetExecutor));
        // Context manager nodes.
        registry.register(Box::new(n::ContextCreateExecutor));
        registry.register(Box::new(n::ContextCloneExecutor));
        registry.register(Box::new(n::ContextMergeExecutor));
        registry.register(Box::new(n::ContextFilterExecutor));
        registry.register(Box::new(n::ContextTrimExecutor));
        registry.register(Box::new(n::ContextToTextExecutor));
        // Flow support.
        registry.register(Box::new(n::DelayExecutor));
        registry.register(Box::new(n::RequestApprovalExecutor));
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
