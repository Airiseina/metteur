//! Integration tests for the `DraftBlueprint` tool.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::{Registry, Tool, tools::blueprint::DraftBlueprint};
use metteur_shared::Value;

/// Builds a context rooted at a throwaway workspace.
fn context(with_db: bool) -> (ExecutionContext, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("metteur-draft-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut ctx = ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        root.clone(),
    );
    if with_db {
        let db = metteur_daemon::storage::persistence::Db::open(&root.join("db")).unwrap();
        ctx.workspace_db = Some(db);
    }
    (ctx, root)
}

fn args(draft: serde_json::Value, save: bool) -> Vec<Value> {
    vec![Value::Json(serde_json::json!({ "draft": draft, "save": save }))]
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => panic!("expected a string result, got {other:?}"),
    }
}

#[tokio::test]
async fn returns_a_reviewable_plan_without_saving() {
    let (mut ctx, root) = context(false);
    let result = DraftBlueprint
        .call(
            &args(
                serde_json::json!({
                    "name": "Review me",
                    "nodes": {
                        "start": { "kind": "Start" },
                        "read":  { "kind": "ReadFile", "path": "src/main.rs" },
                        "edit":  { "kind": "EditFile", "path": "src/main.rs",
                                   "edits": [{ "old_string": "a", "new_string": "b" }] }
                    },
                    "flow": ["start -> read -> edit"]
                }),
                false,
            ),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("Review me"), "{text}");
    assert!(text.contains("3 node(s)"), "{text}");
    assert!(text.contains("Not saved"), "{text}");
    assert!(text.contains("ReadFile"), "{text}");
    // The entry node is called out so the plan reads unambiguously.
    assert!(text.contains("(entry)"), "{text}");
    // Nothing was persisted: the caller only asked for review.
    let _ = root;
}

#[tokio::test]
async fn saves_the_compiled_blueprint_when_asked() {
    let (mut ctx, _root) = context(true);
    let result = DraftBlueprint
        .call(
            &args(
                serde_json::json!({
                    "name": "Keep me",
                    "nodes": {
                        "start": { "kind": "Start" },
                        "r":     { "kind": "ReadFile", "path": "a.txt" }
                    },
                    "flow": ["start -> r"]
                }),
                true,
            ),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("Saved"), "{text}");

    // The stored copy round-trips and matches what was reported.
    let db = ctx.workspace_db.clone().expect("the context holds the database");
    let blueprint = metteur_shared::dsl::compile_draft(
        r#"{
          "name": "Keep me",
          "nodes": {
            "start": { "kind": "Start" },
            "r":     { "kind": "ReadFile", "path": "a.txt" }
          },
          "flow": ["start -> r"]
        }"#,
    )
    .unwrap();
    let stored = db
        .get(metteur_daemon::storage::persistence::cf::BLUEPRINTS, blueprint.id.as_bytes())
        .unwrap()
        .expect("the blueprint must be persisted under its stable id");
    let decoded: metteur_shared::Blueprint = serde_json::from_slice(&stored).unwrap();
    assert_eq!(decoded.name, "Keep me");
    assert_eq!(decoded.nodes.len(), 2);
}

#[tokio::test]
async fn saving_without_a_workspace_database_is_an_error() {
    let (mut ctx, _) = context(false);
    let error = DraftBlueprint
        .call(
            &args(
                serde_json::json!({
                    "nodes": { "s": { "kind": "Start" } }
                }),
                true,
            ),
            &mut ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("needs a workspace database"), "{error}");
}

#[tokio::test]
async fn compilation_errors_reach_the_model_verbatim() {
    let (mut ctx, _) = context(false);
    let error = DraftBlueprint
        .call(
            &args(
                serde_json::json!({
                    "nodes": {
                        "start": { "kind": "Start" },
                        "bad":   { "kind": "NoSuchKind" }
                    }
                }),
                false,
            ),
            &mut ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    // The message must name the offending node so a retry can be targeted.
    assert!(error.contains("NoSuchKind"), "{error}");
    assert!(error.contains("node 'bad'"), "{error}");
}

#[tokio::test]
async fn a_json_string_draft_is_accepted() {
    // Some callers stringify nested arguments; both shapes must work.
    let (mut ctx, _) = context(false);
    let draft = r#"{"nodes": {"s": {"kind": "Start"}}, "flow": []}"#;
    let result = DraftBlueprint
        .call(&[Value::Json(serde_json::json!({ "draft": draft }))], &mut ctx)
        .await
        .unwrap();
    assert!(text_of(&result).contains("1 node(s)"));
}

#[tokio::test]
async fn malformed_json_is_reported() {
    let (mut ctx, _) = context(false);
    let error = DraftBlueprint
        .call(&[Value::Json(serde_json::json!({ "draft": "{ not json }" }))], &mut ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not valid JSON"), "{error}");
}

#[tokio::test]
async fn missing_draft_is_an_error() {
    let (mut ctx, _) = context(false);
    let error = DraftBlueprint
        .call(&[Value::Json(serde_json::json!({}))], &mut ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a draft"), "{error}");
}

#[tokio::test]
async fn the_report_lists_data_wiring_and_unreached_nodes() {
    let (mut ctx, _) = context(false);
    let result = DraftBlueprint
        .call(
            &args(
                serde_json::json!({
                    "nodes": {
                        "start": { "kind": "Start", "A": 2 },
                        "sum":   { "kind": "Add", "A": "$start.A", "B": 3 },
                        "loose": { "kind": "ReadFile", "path": "unused.txt" }
                    },
                    "flow": ["start -> sum"]
                }),
                false,
            ),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("Data wiring:"), "{text}");
    assert!(text.contains("Start.A -> Add.A"), "{text}");
    // A node the flow never reaches is called out: usually a mistake.
    assert!(text.contains("Not reached by the flow"), "{text}");
    // Tool steps are labelled with the argument that distinguishes them, so a
    // plan with several calls of one tool stays readable.
    assert!(text.contains("Tool: ReadFile unused.txt"), "{text}");
}

#[tokio::test]
async fn the_tool_is_registered_and_persistent() {
    let registry = Registry::with_builtins();
    let tool = registry.tool("DraftBlueprint").expect("registered");
    // The result is useful across turns (the model may revise and re-save).
    assert_eq!(tool.lifetime(), metteur_shared::ToolResultLifetime::Persistent);
    assert!(!tool.read_only(), "it can write a blueprint");
}

#[tokio::test]
async fn re_drafting_updates_rather_than_duplicates() {
    let (mut ctx, root) = context(true);
    let first = serde_json::json!({
        "name": "Evolving",
        "nodes": {
            "start": { "kind": "Start" },
            "r":     { "kind": "ReadFile", "path": "v1.txt" }
        },
        "flow": ["start -> r"]
    });
    DraftBlueprint.call(&args(first, true), &mut ctx).await.unwrap();

    // Same draft shape, one argument corrected: the id is derived from the
    // name, so the corrected plan overwrites the first instead of adding a
    // near-duplicate the user has to clean up.
    let second = serde_json::json!({
        "name": "Evolving",
        "nodes": {
            "start": { "kind": "Start" },
            "r":     { "kind": "ReadFile", "path": "v2.txt" }
        },
        "flow": ["start -> r"]
    });
    DraftBlueprint.call(&args(second, true), &mut ctx).await.unwrap();

    let _ = root;
    let db = ctx.workspace_db.clone().expect("the context holds the database");
    let id = metteur_shared::dsl::compile_draft(
        r#"{"name": "Evolving", "nodes": {"start": {"kind": "Start"}, "r": {"kind": "ReadFile", "path": "v2.txt"}}, "flow": ["start -> r"]}"#,
    )
    .unwrap()
    .id;
    let stored = db
        .get(metteur_daemon::storage::persistence::cf::BLUEPRINTS, id.as_bytes())
        .unwrap()
        .expect("stored");
    let decoded: metteur_shared::Blueprint = serde_json::from_slice(&stored).unwrap();
    let read = decoded.nodes.iter().find(|n| n.kind == "Tool").unwrap();
    assert_eq!(read.data.get("path"), Some(&serde_json::json!("v2.txt")));
}
