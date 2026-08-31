//! Tool trait for LLM-accessible capabilities.

use async_trait::async_trait;
use metteur_shared::Value;

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;

/// A tool that an LLM or blueprint node can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool name (PascalCase imperative verb, e.g. `SearchFile`).
    fn name(&self) -> &str;

    /// A human-readable description of the tool.
    fn description(&self) -> &str;

    /// The JSON Schema describing the tool's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Invokes the tool with the given arguments.
    ///
    /// The shared [`ExecutionContext`] provides access to the transaction log
    /// so that file-mutating tools can record changes for rollback.
    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value>;
}
