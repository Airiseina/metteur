//! Tool trait for LLM-accessible capabilities.

use std::time::Duration;

use async_trait::async_trait;
use metteur_shared::{ToolResultLifetime, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;

/// Result byte cap applied to tools that do not declare their own.
pub const DEFAULT_TOOL_RESULT_BYTES: usize = 16 * 1024;

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

    /// Whether the tool has no side effects.
    ///
    /// Read-only calls in one model turn may run concurrently; mutating tools
    /// never do. Defaults to `false` so an unannotated tool is never
    /// parallelized by mistake.
    fn read_only(&self) -> bool {
        false
    }

    /// Retention hint for the context manager.
    ///
    /// Reads stay useful across turns; mutations are consumed on the spot.
    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::OneShot
    }

    /// Byte cap applied to this tool's textual result by the ReAct loop.
    ///
    /// `0` selects the runtime default (`[llm].max_tool_result_bytes`), which
    /// lets an operator retune every unreserved tool at once.
    fn max_result_bytes(&self) -> usize {
        0
    }

    /// Per-call timeout; `None` disables the extra guard.
    fn timeout(&self) -> Option<Duration> {
        None
    }
}

/// Returns whether `name` is a valid registered tool name: PascalCase
/// (leading uppercase, ASCII alphanumeric).
///
/// The same rule applies to built-in, MCP-derived, addon and DSL tools so the
/// registry is a single enforcement point instead of per-source checks.
pub fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|first| first.is_ascii_uppercase())
        && name.chars().all(|ch| ch.is_ascii_alphanumeric())
}
