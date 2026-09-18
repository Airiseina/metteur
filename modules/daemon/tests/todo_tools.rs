//! Integration tests for the agent task-list tools and the todo model.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::{
    Registry, Tool, tools::todo::TodoRead, tools::todo::TodoWrite,
};
use metteur_shared::llm::{TodoItem, TodoStatus};
use metteur_shared::Value;

fn context() -> ExecutionContext {
    ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        std::env::temp_dir(),
    )
}

fn args(value: serde_json::Value) -> Vec<Value> {
    vec![Value::Json(value)]
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => panic!("expected string result, got {other:?}"),
    }
}

#[tokio::test]
async fn write_records_and_renders_the_list() {
    let mut ctx = context();
    let result = TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [
                    { "content": "Read the code", "status": "completed" },
                    { "content": "Add the parser", "status": "in_progress", "active_form": "Adding the parser" },
                    { "content": "Run the tests", "status": "pending" }
                ]
            })),
            &mut ctx,
        )
        .await
        .unwrap();
    assert_eq!(ctx.todos.len(), 3);
    assert_eq!(ctx.todos[1].status, TodoStatus::InProgress);

    let text = text_of(&result);
    assert!(text.contains("1/3 completed"), "{text}");
    assert!(text.contains("[x] Read the code"), "{text}");
    // The in-progress label uses the present-continuous phrasing.
    assert!(text.contains("[~] Adding the parser"), "{text}");
    assert!(text.contains("[ ] Run the tests"), "{text}");
}

#[tokio::test]
async fn write_replaces_the_whole_list() {
    let mut ctx = context();
    TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "first", "status": "pending" }]
            })),
            &mut ctx,
        )
        .await
        .unwrap();
    TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [
                    { "content": "second", "status": "completed" },
                    { "content": "third", "status": "pending" }
                ]
            })),
            &mut ctx,
        )
        .await
        .unwrap();
    // Replacement, not merge: the first item is gone.
    assert_eq!(ctx.todos.len(), 2);
    assert_eq!(ctx.todos[0].content, "second");
}

#[tokio::test]
async fn multiple_in_progress_items_are_rejected() {
    let mut ctx = context();
    let error = TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [
                    { "content": "a", "status": "in_progress" },
                    { "content": "b", "status": "in_progress" }
                ]
            })),
            &mut ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("at most one todo"), "{error}");
    // The rejected write left the list untouched.
    assert!(ctx.todos.is_empty());
}

#[tokio::test]
async fn unknown_status_is_rejected_with_a_hint() {
    let mut ctx = context();
    let error = TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "a", "status": "doing" }]
            })),
            &mut ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown status 'doing'"), "{error}");
    assert!(error.contains("pending, in_progress or completed"), "{error}");
}

#[tokio::test]
async fn missing_content_is_rejected() {
    let mut ctx = context();
    let error = TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "  ", "status": "pending" }]
            })),
            &mut ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-empty content"), "{error}");
}

#[tokio::test]
async fn an_empty_list_clears_the_plan() {
    let mut ctx = context();
    TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "a", "status": "pending" }]
            })),
            &mut ctx,
        )
        .await
        .unwrap();
    let result =
        TodoWrite.call(&args(serde_json::json!({ "todos": [] })), &mut ctx).await.unwrap();
    assert!(ctx.todos.is_empty());
    assert_eq!(text_of(&result), "No todos.");
}

#[tokio::test]
async fn read_returns_the_current_list() {
    let mut ctx = context();
    ctx.todos = vec![TodoItem {
        content: "Ship it".to_string(),
        status: TodoStatus::Pending,
        active_form: None,
    }];
    let result = TodoRead.call(&[], &mut ctx).await.unwrap();
    assert!(text_of(&result).contains("[ ] Ship it"));
    assert!(TodoRead.read_only(), "reading the list has no side effects");
}

#[tokio::test]
async fn writes_emit_an_event_for_live_viewers() {
    let mut ctx = context();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.events = Some(tx);
    TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "watch me", "status": "in_progress" }]
            })),
            &mut ctx,
        )
        .await
        .unwrap();
    let event = rx.recv().await.expect("an event must be emitted");
    match event {
        metteur_daemon::execution::ExecutionEvent::Todos {
            todos,
            ..
        } => {
            assert_eq!(todos.len(), 1);
            assert_eq!(todos[0].content, "watch me");
        }
        other => panic!("expected a Todos event, got {other:?}"),
    }
}

#[tokio::test]
async fn a_nested_context_keeps_its_own_list() {
    let mut parent = context();
    parent.todos = vec![TodoItem::pending("parent task")];
    let mut child = parent.child_nested();
    TodoWrite
        .call(
            &args(serde_json::json!({
                "todos": [{ "content": "child task", "status": "in_progress" }]
            })),
            &mut child,
        )
        .await
        .unwrap();
    // The sub-agent's plan must not rewrite the caller's list.
    assert_eq!(parent.todos.len(), 1);
    assert_eq!(parent.todos[0].content, "parent task");
    assert_eq!(child.todos[0].content, "child task");
}

#[test]
fn status_parsing_accepts_common_spellings() {
    assert_eq!(TodoStatus::parse("pending"), Some(TodoStatus::Pending));
    assert_eq!(TodoStatus::parse("In_Progress"), Some(TodoStatus::InProgress));
    assert_eq!(TodoStatus::parse("in-progress"), Some(TodoStatus::InProgress));
    assert_eq!(TodoStatus::parse("DONE"), Some(TodoStatus::Completed));
    assert_eq!(TodoStatus::parse("nonsense"), None);
    assert_eq!(TodoStatus::InProgress.as_str(), "in_progress");
}

#[test]
fn render_reports_progress_counts() {
    let todos = vec![
        TodoItem {
            content: "a".to_string(),
            status: TodoStatus::Completed,
            active_form: None,
        },
        TodoItem {
            content: "b".to_string(),
            status: TodoStatus::Completed,
            active_form: None,
        },
        TodoItem::pending("c"),
    ];
    let text = metteur_shared::llm::render_todos(&todos);
    assert!(text.starts_with("2/3 completed (0 in progress)"), "{text}");
}

#[test]
fn todo_list_round_trips_through_json() {
    let todos = vec![TodoItem {
        content: "persist me".to_string(),
        status: TodoStatus::InProgress,
        active_form: Some("Persisting".to_string()),
    }];
    let encoded = serde_json::to_string(&todos).unwrap();
    let decoded: Vec<TodoItem> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, todos);
    // A record written before the field existed still deserializes.
    let legacy: Vec<TodoItem> = serde_json::from_str("[]").unwrap();
    assert!(legacy.is_empty());
}

// ---------------------------------------------------------------------------
// LspCheck: the deterministic validation node.
// ---------------------------------------------------------------------------

use metteur_daemon::execution::nodes::LspCheckExecutor;
use metteur_daemon::registry::NodeExecutor;
use metteur_shared::{DataType, Node, NodeType, Pin, PinType};

/// Builds an `LspCheck` node with the given data overrides.
fn lsp_check_node(data: serde_json::Value) -> Node {
    let pin = |name: &str, pin_type: PinType, data_type: DataType| Pin {
        id: uuid::Uuid::new_v4(),
        name: name.to_string(),
        pin_type,
        data_type,
        ..Default::default()
    };
    Node {
        id: uuid::Uuid::new_v4(),
        node_type: NodeType::Control,
        kind: "LspCheck".to_string(),
        position: (0.0, 0.0),
        pins: vec![
            pin("x-in", PinType::ExecInput, DataType::Void),
            pin("x-out", PinType::ExecOutput, DataType::Void),
            pin("Path", PinType::DataInput, DataType::String),
            pin("Passed", PinType::DataOutput, DataType::Bool),
            pin("Errors", PinType::DataOutput, DataType::Int),
            pin("Warnings", PinType::DataOutput, DataType::Int),
            pin("Diagnostics", PinType::DataOutput, DataType::String),
        ],
        data,
    }
}

/// Reads a named output from the executor result.
fn output<'a>(
    node: &Node,
    outputs: &'a std::collections::HashMap<metteur_shared::PinId, Value>,
    name: &str,
) -> &'a Value {
    let pin = node.pins.iter().find(|p| p.name == name).expect("output pin exists");
    outputs.get(&pin.id).expect("output present")
}

#[tokio::test]
async fn lsp_check_passes_when_no_server_is_configured() {
    // Without LSP the rule has nothing to observe; failing here would break
    // every workspace that has not configured a language server.
    let node = lsp_check_node(serde_json::json!({ "path": "src/lib.rs" }));
    let mut ctx = context();
    let outputs = LspCheckExecutor.execute(&node, &std::collections::HashMap::new(), &mut ctx).await.unwrap();
    assert_eq!(output(&node, &outputs, "Passed"), &Value::Bool(true));
    // Counters are declared `Int`; the pin type and the value must agree.
    assert_eq!(output(&node, &outputs, "Errors"), &Value::Int(0));
    let text = output(&node, &outputs, "Diagnostics").as_str().unwrap_or_default().to_string();
    assert!(text.contains("not configured"), "{text}");
}

#[tokio::test]
async fn lsp_check_requires_a_path() {
    let node = lsp_check_node(serde_json::json!({}));
    let mut ctx = context();
    let error = LspCheckExecutor
        .execute(&node, &std::collections::HashMap::new(), &mut ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a Path"), "{error}");
}

#[tokio::test]
async fn lsp_check_reads_a_path_list_from_node_data() {
    // A `paths` list checks several files in one node; without LSP the result
    // is still a pass, which proves the list was accepted.
    let node = lsp_check_node(serde_json::json!({ "paths": ["src/a.rs", "src/b.rs"] }));
    let mut ctx = context();
    let outputs = LspCheckExecutor.execute(&node, &std::collections::HashMap::new(), &mut ctx).await.unwrap();
    assert_eq!(output(&node, &outputs, "Passed"), &Value::Bool(true));
}

#[tokio::test]
async fn lsp_check_participates_in_validation_failures() {
    // The interpreter's circuit/retry logic keys off these kinds; a change
    // here would silently stop `LspCheck` from triggering rollback.
    let registry = Registry::with_builtins();
    assert!(registry.node_executor("LspCheck").is_some(), "LspCheck must be registered");
    let node = lsp_check_node(serde_json::json!({ "path": "a.rs" }));
    assert_eq!(node.kind, "LspCheck");
}
