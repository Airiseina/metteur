//! Integration tests for the `EditFile` tool.
//!
//! These cover the properties that make anchored editing reliable: exact and
//! normalized matching, atomicity across blocks, ambiguity reporting, the
//! concurrency guard, and line-style preservation.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::{Registry, tools::edit::EditFile, Tool};
use metteur_shared::Value;

/// Creates a temporary workspace with one file and returns its root.
fn workspace(content: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-edit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.rs"), content).unwrap();
    root
}

fn context(root: &std::path::Path) -> ExecutionContext {
    ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        root.to_path_buf(),
    )
}

/// Builds the tool arguments for one or more edit blocks.
fn args(path: &str, edits: serde_json::Value) -> Vec<Value> {
    vec![Value::Json(serde_json::json!({
        "path": path,
        "edits": edits,
    }))]
}

fn read(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join("main.rs")).unwrap()
}

/// Runs EditFile and returns the resulting message or the error text.
async fn run(root: &std::path::Path, edits: serde_json::Value) -> Result<String, String> {
    let mut ctx = context(root);
    match EditFile.call(&args("main.rs", edits), &mut ctx).await {
        Ok(Value::String(text)) => Ok(text),
        Ok(other) => panic!("unexpected result type: {other:?}"),
        Err(err) => Err(err.to_string()),
    }
}

#[tokio::test]
async fn applies_a_single_exact_edit() {
    let root = workspace("fn main() {\n    println!(\"hi\");\n}\n");
    let result = run(
        &root,
        serde_json::json!([{ "old_string": "println!(\"hi\");", "new_string": "println!(\"bye\");" }]),
    )
    .await
    .unwrap();
    assert!(result.contains("exact"), "{result}");
    assert!(read(&root).contains("println!(\"bye\");"));
    assert!(!read(&root).contains("println!(\"hi\");"));
}

#[tokio::test]
async fn applies_all_blocks_atomically() {
    let root = workspace("alpha\nbeta\ngamma\n");
    let result = run(
        &root,
        serde_json::json!([
            { "old_string": "alpha", "new_string": "ALPHA" },
            { "old_string": "gamma", "new_string": "GAMMA" },
        ]),
    )
    .await
    .unwrap();
    assert_eq!(read(&root), "ALPHA\nbeta\nGAMMA\n");
    assert!(result.contains("2 edit(s)"), "{result}");
}

#[tokio::test]
async fn a_failing_block_leaves_the_file_untouched() {
    let root = workspace("alpha\nbeta\n");
    let original = read(&root);
    let error = run(
        &root,
        serde_json::json!([
            { "old_string": "alpha", "new_string": "ALPHA" },
            { "old_string": "does-not-exist", "new_string": "x" },
        ]),
    )
    .await
    .unwrap_err();
    assert!(error.contains("edit #2 failed"), "{error}");
    // Atomicity: the first block was not written either.
    assert_eq!(read(&root), original);
}

#[tokio::test]
async fn ambiguous_anchor_lists_the_candidate_lines() {
    let root = workspace("let x = 1;\nlet y = 2;\nlet x = 1;\n");
    let error = run(
        &root,
        serde_json::json!([{ "old_string": "let x = 1;", "new_string": "let x = 3;" }]),
    )
    .await
    .unwrap_err();
    assert!(error.contains("matches 2 locations"), "{error}");
    assert!(error.contains("lines 1, 3"), "{error}");
    assert!(error.contains("replace_all"), "{error}");
    assert_eq!(read(&root), "let x = 1;\nlet y = 2;\nlet x = 1;\n");
}

#[tokio::test]
async fn replace_all_updates_every_occurrence() {
    let root = workspace("let x = 1;\nlet y = 2;\nlet x = 1;\n");
    run(
        &root,
        serde_json::json!([{
            "old_string": "let x = 1;",
            "new_string": "let x = 3;",
            "replace_all": true,
        }]),
    )
    .await
    .unwrap();
    assert_eq!(read(&root), "let x = 3;\nlet y = 2;\nlet x = 3;\n");
}

#[tokio::test]
async fn missing_anchor_reports_the_closest_region() {
    let root = workspace("fn main() {\n    let value = compute();\n    println!(\"{value}\");\n}\n");
    let error = run(
        &root,
        serde_json::json!([{
            "old_string": "let value = compute_the_thing();",
            "new_string": "let value = 1;",
        }]),
    )
    .await
    .unwrap_err();
    assert!(error.contains("not found"), "{error}");
    // The diagnostic points at the nearest line with context and numbers.
    assert!(error.contains("Closest region"), "{error}");
    assert!(error.contains("compute()"), "{error}");
}

#[tokio::test]
async fn trailing_whitespace_differences_still_match() {
    // The file has trailing spaces the model did not reproduce.
    let root = workspace("fn main() {   \n    let a = 1;  \n}\n");
    let result = run(
        &root,
        serde_json::json!([{
            "old_string": "fn main() {\n    let a = 1;",
            "new_string": "fn main() {\n    let a = 2;",
        }]),
    )
    .await
    .unwrap();
    assert!(result.contains("trimmed"), "{result}");
    assert!(read(&root).contains("let a = 2;"));
}

#[tokio::test]
async fn indentation_differences_match_and_reindent() {
    // The file nests the block one level deeper than the model assumed.
    let root = workspace("fn main() {\n    if ready {\n        let a = 1;\n    }\n}\n");
    let result = run(
        &root,
        serde_json::json!([{
            "old_string": "let a = 1;",
            "new_string": "let a = 2;\nlet b = 3;",
        }]),
    )
    .await
    .unwrap();
    assert!(result.contains("exact"), "{result}");
    // A single-line anchor matches exactly; multi-line re-indentation is
    // covered by the test below.
    assert!(read(&root).contains("let a = 2;\n        let b = 3;"));
}

#[tokio::test]
async fn multi_line_indent_normalized_block_is_reindented() {
    let root = workspace(
        "fn main() {\n    if ready {\n        let a = 1;\n        let b = 2;\n    }\n}\n",
    );
    // The model writes the block flush-left (a common transcription error).
    let result = run(
        &root,
        serde_json::json!([{
            "old_string": "let a = 1;\nlet b = 2;",
            "new_string": "let a = 10;\nlet b = 20;",
        }]),
    )
    .await
    .unwrap();
    assert!(result.contains("indent-normalized"), "{result}");
    let content = read(&root);
    assert!(content.contains("        let a = 10;"), "{content}");
    assert!(content.contains("        let b = 20;"), "{content}");
}

#[tokio::test]
async fn crlf_line_endings_are_preserved() {
    let root = workspace("fn main() {\r\n    let a = 1;\r\n}\r\n");
    run(
        &root,
        serde_json::json!([{ "old_string": "let a = 1;", "new_string": "let a = 2;" }]),
    )
    .await
    .unwrap();
    let content = read(&root);
    assert_eq!(content, "fn main() {\r\n    let a = 2;\r\n}\r\n");
}

#[tokio::test]
async fn missing_trailing_newline_is_preserved() {
    let root = workspace("fn main() {}");
    run(
        &root,
        serde_json::json!([{ "old_string": "main", "new_string": "start" }]),
    )
    .await
    .unwrap();
    assert_eq!(read(&root), "fn start() {}");
}

#[tokio::test]
async fn stale_hash_guard_rejects_the_edit() {
    let root = workspace("alpha\n");
    // A hash that cannot match the current content.
    let mut ctx = context(&root);
    let result = EditFile
        .call(
            &[Value::Json(serde_json::json!({
                "path": "main.rs",
                "edits": [{ "old_string": "alpha", "new_string": "beta" }],
                "expected_sha256": "deadbeefdeadbeef",
            }))],
            &mut ctx,
        )
        .await;
    let error = result.unwrap_err().to_string();
    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(read(&root), "alpha\n");
}

#[tokio::test]
async fn matching_hash_guard_allows_the_edit() {
    let root = workspace("alpha\n");
    let hash = metteur_daemon::registry::tools::fs_tools::short_hash(b"alpha\n");
    let mut ctx = context(&root);
    EditFile
        .call(
            &[Value::Json(serde_json::json!({
                "path": "main.rs",
                "edits": [{ "old_string": "alpha", "new_string": "beta" }],
                "expected_sha256": hash,
            }))],
            &mut ctx,
        )
        .await
        .unwrap();
    assert_eq!(read(&root), "beta\n");
}

#[tokio::test]
async fn records_the_change_for_rollback() {
    let root = workspace("alpha\n");
    let mut ctx = context(&root);
    EditFile
        .call(
            &args("main.rs", serde_json::json!([{ "old_string": "alpha", "new_string": "beta" }])),
            &mut ctx,
        )
        .await
        .unwrap();
    assert_eq!(ctx.transaction_log.entries().len(), 1);
    assert_eq!(ctx.mutated_paths.len(), 1);
}

#[tokio::test]
async fn rejects_a_non_utf8_file() {
    let root = workspace("");
    std::fs::write(root.join("main.rs"), [0xff, 0xfe, 0x00]).unwrap();
    let error = run(
        &root,
        serde_json::json!([{ "old_string": "a", "new_string": "b" }]),
    )
    .await
    .unwrap_err();
    assert!(error.contains("not valid UTF-8"), "{error}");
}

#[tokio::test]
async fn empty_old_string_is_rejected() {
    let root = workspace("alpha\n");
    let error = run(&root, serde_json::json!([{ "old_string": "", "new_string": "b" }]))
        .await
        .unwrap_err();
    assert!(error.contains("must not be empty"), "{error}");
}

#[tokio::test]
async fn returns_a_readable_diff() {
    let root = workspace("alpha\nbeta\n");
    let result = run(
        &root,
        serde_json::json!([{ "old_string": "beta", "new_string": "BETA" }]),
    )
    .await
    .unwrap();
    assert!(result.contains("-beta"), "{result}");
    assert!(result.contains("+BETA"), "{result}");
}

#[tokio::test]
async fn later_blocks_see_earlier_results() {
    // The second block anchors on text the first block introduced.
    let root = workspace("start\nend\n");
    run(
        &root,
        serde_json::json!([
            { "old_string": "start", "new_string": "middle" },
            { "old_string": "middle", "new_string": "finish" },
        ]),
    )
    .await
    .unwrap();
    assert_eq!(read(&root), "finish\nend\n");
}
