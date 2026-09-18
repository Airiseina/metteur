//! Tool definitions, calls and results.

use serde::{Deserialize, Serialize};

/// A tool definition exposed to the model for function calling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// The tool name (PascalCase imperative verb).
    pub name: String,
    /// A human-readable description of the tool.
    pub description: String,
    /// The JSON Schema describing the tool's arguments.
    pub parameters: serde_json::Value,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The unique id of the call, used to correlate the result.
    pub id: String,
    /// The name of the tool to invoke.
    pub name: String,
    /// The arguments for the call.
    pub arguments: serde_json::Value,
}

/// How long a tool result should be retained in the context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultLifetime {
    /// The result is only useful for the immediate next step.
    #[default]
    OneShot,
    /// The result remains relevant across multiple steps.
    Persistent,
}

/// The result of a tool call, mixed into the context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The id of the tool call this result corresponds to.
    pub tool_call_id: String,
    /// The name of the tool that produced the result.
    ///
    /// Recorded so results can be released or audited by tool; empty for
    /// records persisted before the field existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// The textual result content.
    pub content: String,
    /// Creation time in milliseconds since the Unix epoch.
    pub timestamp: u64,
    /// How long the result should be retained.
    pub lifetime: ToolResultLifetime,
    /// Workspace-relative paths this result reads, used to expire it when the
    /// file changes afterwards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<std::path::PathBuf>,
}
