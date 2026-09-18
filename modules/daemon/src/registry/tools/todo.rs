//! The `TodoWrite` and `TodoRead` tools.
//!
//! A todo list is the model's own plan for a multi-step task. Writes replace
//! the whole list, which keeps the model's view and the stored list in sync by
//! construction: there is no merge to disagree about.

use async_trait::async_trait;
use metteur_shared::llm::{TodoItem, TodoStatus, render_todos};
use metteur_shared::{ToolResultLifetime, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::tool::Tool;

use super::Args;

/// Replaces the agent's task list.
pub struct TodoWrite;

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Records your task list for the current work. Send the complete list \
         every time: it replaces the previous one. Mark exactly one item as \
         in_progress while you work on it, and mark it completed immediately \
         after finishing. Use it for multi-step work; skip it for a single \
         trivial action."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete task list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Imperative description of the task." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Task state."
                            },
                            "active_form": {
                                "type": "string",
                                "description": "Present-continuous phrasing shown while in progress."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    fn max_result_bytes(&self) -> usize {
        4 * 1024
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let raw = a
            .get("todos", 0)
            .ok_or_else(|| DaemonError::Execution("TodoWrite requires a todos list".to_string()))?;
        let todos = parse_todos(&raw)?;
        // More than one active item hides what is actually being worked on;
        // reject it so the model picks a single focus.
        let active = metteur_shared::llm::todo::in_progress_count(&todos);
        if active > 1 {
            return Err(DaemonError::Execution(format!(
                "at most one todo may be in_progress, found {active}; mark the others pending or completed"
            )));
        }

        ctx.todos = todos;
        if let Some(tx) = &ctx.events {
            let _ = tx.send(crate::execution::ExecutionEvent::Todos {
                node_id: ctx.current_node,
                todos: ctx.todos.clone(),
            });
        }
        ctx.audit(
            "todo.write",
            serde_json::json!({
                "run_id": ctx.run_id.to_string(),
                "count": ctx.todos.len(),
            }),
        );
        Ok(Value::String(render_todos(&ctx.todos)))
    }
}

/// Reads back the agent's task list.
pub struct TodoRead;

#[async_trait]
impl Tool for TodoRead {
    fn name(&self) -> &str {
        "TodoRead"
    }

    fn description(&self) -> &str {
        "Returns the current task list. Usually unnecessary: the list is \
         restated whenever you write it."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    fn max_result_bytes(&self) -> usize {
        4 * 1024
    }

    async fn call(&self, _args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        Ok(Value::String(render_todos(&ctx.todos)))
    }
}

/// Parses the `todos` argument into shared items.
fn parse_todos(raw: &Value) -> DaemonResult<Vec<TodoItem>> {
    let items: Vec<serde_json::Value> = match raw {
        Value::Json(serde_json::Value::Array(items)) => items.clone(),
        Value::Json(serde_json::Value::String(text)) => serde_json::from_str(text).map_err(|err| {
            DaemonError::Execution(format!("TodoWrite todos must be a JSON array: {err}"))
        })?,
        Value::List(values) => values.iter().map(value_to_json).collect(),
        other => {
            let encoded = crate::execution::nodes::value_to_string(other);
            serde_json::from_str(&encoded).map_err(|err| {
                DaemonError::Execution(format!("TodoWrite todos must be a JSON array: {err}"))
            })?
        }
    };

    let mut todos = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let content = item
            .get("content")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                DaemonError::Execution(format!("todo #{} is missing a non-empty content", index + 1))
            })?;
        let raw_status = item
            .get("status")
            .and_then(|value| value.as_str())
            .ok_or_else(|| DaemonError::Execution(format!("todo #{index} is missing a status")))?;
        let status = TodoStatus::parse(raw_status).ok_or_else(|| {
            DaemonError::Execution(format!(
                "todo #{index} has unknown status '{raw_status}'; use pending, in_progress or completed"
            ))
        })?;
        let active_form = item
            .get("active_form")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        todos.push(TodoItem {
            content: content.to_string(),
            status,
            active_form,
        });
    }
    Ok(todos)
}

/// Converts a shared value into JSON for uniform parsing.
fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Json(json) => json.clone(),
        other => serde_json::json!(other.as_str().unwrap_or_default()),
    }
}
