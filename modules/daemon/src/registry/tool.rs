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

/// Argument names that carry the subject of a call, most specific first.
///
/// A tool's own name is not consulted: tools with the same shape (a path) share
/// a summary, which keeps the activity line consistent across the tool set.
const SUMMARY_KEYS: [&str; 14] = [
    "path",
    "file_path",
    "file",
    "files",
    "command",
    "pattern",
    "query",
    "glob",
    "url",
    "job_id",
    "id",
    "task",
    "description",
    "title",
];

/// Longest summary rendered to a client; longer arguments are cut.
const SUMMARY_MAX_CHARS: usize = 120;

/// Renders a one-line summary of a tool call's arguments.
///
/// Used for the live activity line of a streaming client ("Read src/api.ts")
/// and as the arguments preview of a running call. Only [`SUMMARY_KEYS`] is
/// consulted, so a large payload (a file body, a diff) never reaches the UI;
/// when no known key is present the first string value is used. Line breaks are
/// collapsed and the result is truncated, because this text is rendered in a
/// single line.
///
/// The tool name is not an input: tools that take the same kind of subject
/// render the same way, and a client already knows which tool it asked about.
pub fn summarize_call(arguments: &serde_json::Value) -> String {
    let raw = match arguments {
        serde_json::Value::Object(map) => SUMMARY_KEYS
            .iter()
            .find_map(|key| map.get(*key).and_then(render_value))
            .or_else(|| map.values().find_map(render_value)),
        // Older clients send positional arguments as an array.
        serde_json::Value::Array(items) => items.iter().find_map(render_value),
        other => render_value(other),
    };
    let Some(raw) = raw else {
        return String::new();
    };
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SUMMARY_MAX_CHARS {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(SUMMARY_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Renders one argument value as a single-line string.
fn render_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        serde_json::Value::Array(items) => items.iter().find_map(render_value),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_path_argument() {
        let args = serde_json::json!({ "path": "src/api.ts", "offset": 1, "limit": 40 });
        assert_eq!(summarize_call(&args), "src/api.ts");
    }

    #[test]
    fn prefers_the_subject_over_other_arguments() {
        let args = serde_json::json!({
            "path": "src/api.ts",
            "old_string": "a very long body\nwith newlines",
            "new_string": "another body",
        });
        assert_eq!(summarize_call(&args), "src/api.ts");
    }

    #[test]
    fn collapses_whitespace_and_truncates() {
        let args = serde_json::json!({ "command": "cargo  test\n  --workspace" });
        assert_eq!(summarize_call(&args), "cargo test --workspace");

        let long = "x".repeat(500);
        let args = serde_json::json!({ "command": long });
        let summary = summarize_call(&args);
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn falls_back_to_positional_arguments() {
        assert_eq!(summarize_call(&serde_json::json!(["a.txt", 10])), "a.txt");
        assert_eq!(summarize_call(&serde_json::json!({})), "");
    }
}
