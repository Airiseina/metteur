//! The `ReleaseContext` tool: the model's handle on its own context size.

use async_trait::async_trait;
use metteur_shared::Value;
use metteur_shared::llm::ReleaseQuery;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::{ContextOp, ExecutionContext};
use crate::registry::tool::Tool;

use super::Args;

/// Releases tool results the model no longer needs.
///
/// The tool itself cannot touch the conversation: the loop that owns the turn
/// applies the queued operation before the next model call. The result text
/// therefore only confirms the request; the loop reports what was actually
/// released.
pub struct ReleaseContext;

#[async_trait]
impl Tool for ReleaseContext {
    fn name(&self) -> &str {
        "ReleaseContext"
    }

    fn description(&self) -> &str {
        "Releases tool results from your context to free room for further work. \
         Select by exact paths, glob patterns, tool names, or release everything \
         at once; `keep_recent` protects the newest results. Released results are \
         replaced by a short marker — re-read a file if you need it again."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact workspace-relative paths whose results should be released."
                },
                "patterns": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns over workspace-relative paths, e.g. \"src/**/*.rs\"."
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Release results produced by these tools, e.g. [\"ReadFile\", \"Grep\"]."
                },
                "all": {
                    "type": "boolean",
                    "description": "Release every releasable result (subject to keep_recent)."
                },
                "keep_recent": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Never release the newest N results (default 0)."
                }
            }
        })
    }

    fn lifetime(&self) -> metteur_shared::ToolResultLifetime {
        metteur_shared::ToolResultLifetime::OneShot
    }

    fn max_result_bytes(&self) -> usize {
        2048
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let query = ReleaseQuery {
            paths: string_list(&a, "paths", 1),
            patterns: string_list(&a, "patterns", 2),
            tools: string_list(&a, "tools", 3),
            all: a.bool("all", 4).unwrap_or(false),
            keep_recent: a.int("keep_recent", 5).unwrap_or(0).max(0) as usize,
        };
        if !query.selects_anything() {
            return Err(DaemonError::Execution(
                "ReleaseContext needs at least one of: paths, patterns, tools, all".to_string(),
            ));
        }
        if !ctx.in_react_loop {
            // A blueprint pin value has no single conversation to mutate, so
            // queueing here would silently do nothing.
            return Err(DaemonError::Execution(
                "ReleaseContext only works inside a conversation; in a blueprint use the \
                 ContextRelease node instead"
                    .to_string(),
            ));
        }
        let summary = describe(&query);
        ctx.context_ops.push(ContextOp::Release(query));
        ctx.audit("context.release_request", serde_json::json!({ "selector": summary }));
        Ok(Value::String(format!(
            "Release queued for {summary}. Matching results will be removed before your next turn; \
             the outcome is reported then."
        )))
    }
}

/// Reads a string-list argument, tolerating a single string.
fn string_list(a: &Args<'_>, name: &str, index: usize) -> Vec<String> {
    match a.get(name, index) {
        Some(Value::List(items)) => {
            items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()
        }
        Some(Value::String(single)) if !single.is_empty() => vec![single],
        _ => Vec::new(),
    }
}

/// Renders a short human-readable selector description for the tool result.
fn describe(query: &ReleaseQuery) -> String {
    if query.all {
        return match query.keep_recent {
            0 => "all tool results".to_string(),
            keep => format!("all tool results except the newest {keep}"),
        };
    }
    let mut parts: Vec<String> = Vec::new();
    if !query.paths.is_empty() {
        parts.push(format!("paths=[{}]", query.paths.join(", ")));
    }
    if !query.patterns.is_empty() {
        parts.push(format!("patterns=[{}]", query.patterns.join(", ")));
    }
    if !query.tools.is_empty() {
        parts.push(format!("tools=[{}]", query.tools.join(", ")));
    }
    if query.keep_recent > 0 {
        parts.push(format!("keep_recent={}", query.keep_recent));
    }
    parts.join(", ")
}
