//! Integration tests for the search tools (`Grep`, `Glob`) and `ReadFile`.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::{
    Registry, Tool, tools::fs_tools::ReadFile, tools::search::Glob, tools::search::Grep,
};
use metteur_shared::Value;

/// Builds a throwaway workspace with the given files.
fn workspace(files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-search-{}", uuid::Uuid::new_v4()));
    for (relative, content) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn context(root: &std::path::Path) -> ExecutionContext {
    ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        root.to_path_buf(),
    )
}

fn json_args(value: serde_json::Value) -> Vec<Value> {
    vec![Value::Json(value)]
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => panic!("expected string result, got {other:?}"),
    }
}

#[tokio::test]
async fn grep_finds_matches_with_line_numbers() {
    let root = workspace(&[
        ("src/lib.rs", "fn alpha() {}\nfn beta() {}\n"),
        ("src/main.rs", "fn main() { alpha(); }\n"),
    ]);
    let mut ctx = context(&root);
    let result = Grep.call(&json_args(serde_json::json!({ "pattern": "alpha" })), &mut ctx).await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("src/lib.rs:
---
1:4:"), "{text}");
    assert!(text.contains("src/main.rs:
---
1:13:"), "{text}");
    assert!(text.contains("2 match(es)"), "{text}");
}

#[tokio::test]
async fn grep_respects_gitignore() {
    let root = workspace(&[
        (".gitignore", "ignored/\n"),
        ("ignored/hidden.rs", "let secret = 1;\n"),
        ("src/lib.rs", "let value = 1;\n"),
    ]);
    let mut ctx = context(&root);
    let result = Grep.call(&json_args(serde_json::json!({ "pattern": "let" })), &mut ctx).await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains("ignored/"), "{text}");
}

#[tokio::test]
async fn grep_skips_target_and_metteur_directories() {
    let root = workspace(&[
        ("target/debug/out.rs", "let generated = 1;\n"),
        (".metteur/db/log", "let state = 1;\n"),
        ("src/lib.rs", "let real = 1;\n"),
    ]);
    let mut ctx = context(&root);
    let result = Grep.call(&json_args(serde_json::json!({ "pattern": "let" })), &mut ctx).await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains("target/"), "{text}");
    assert!(!text.contains(".metteur"), "{text}");
}

#[tokio::test]
async fn grep_filters_by_glob() {
    let root = workspace(&[
        ("src/lib.rs", "let value = 1;\n"),
        ("docs/notes.md", "let value = 2;\n"),
    ]);
    let mut ctx = context(&root);
    let result = Grep
        .call(
            &json_args(serde_json::json!({ "pattern": "let value", "glob": "**/*.rs" })),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains("notes.md"), "{text}");
}

#[tokio::test]
async fn grep_honors_case_insensitivity() {
    let root = workspace(&[("src/lib.rs", "let Value = 1;\n")]);
    let mut ctx = context(&root);
    let sensitive = Grep
        .call(&json_args(serde_json::json!({ "pattern": "value" })), &mut ctx)
        .await
        .unwrap();
    assert!(text_of(&sensitive).contains("No matches"));

    let insensitive = Grep
        .call(
            &json_args(serde_json::json!({ "pattern": "value", "case_insensitive": true })),
            &mut ctx,
        )
        .await
        .unwrap();
    assert!(text_of(&insensitive).contains("src/lib.rs"));
}

#[tokio::test]
async fn grep_reports_invalid_regex() {
    let root = workspace(&[("src/lib.rs", "fn main() {}\n")]);
    let mut ctx = context(&root);
    let error = Grep
        .call(&json_args(serde_json::json!({ "pattern": "(" })), &mut ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid regular expression"), "{error}");
}

#[tokio::test]
async fn grep_includes_context_lines() {
    let root = workspace(&[("src/lib.rs", "one\ntwo\nthree\nfind me\nfive\nsix\n")]);
    let mut ctx = context(&root);
    let result = Grep
        .call(
            &json_args(serde_json::json!({ "pattern": "find me", "context_lines": 1 })),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("3- three"), "{text}");
    assert!(text.contains("5- five"), "{text}");
}

#[tokio::test]
async fn grep_caps_matches_and_says_so() {
    let mut content = String::new();
    for index in 0..20 {
        content.push_str(&format!("hit {index}\n"));
    }
    let root = workspace(&[("src/lib.rs", &content)]);
    let mut ctx = context(&root);
    let result = Grep
        .call(
            &json_args(serde_json::json!({ "pattern": "hit", "max_matches": 3 })),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("[3 match(es)"), "{text}");
    assert!(text.contains("limit"), "{text}");
}

#[tokio::test]
async fn grep_skips_binary_files() {
    let root = workspace(&[("src/lib.rs", "let value = 1;\n")]);
    std::fs::write(root.join("src/blob.bin"), [0u8, 1, 2, 3, b'l', b'e', b't']).unwrap();
    let mut ctx = context(&root);
    let result = Grep.call(&json_args(serde_json::json!({ "pattern": "let" })), &mut ctx).await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("binary file(s) skipped"), "{text}");
}

#[tokio::test]
async fn glob_matches_by_pattern() {
    let root = workspace(&[
        ("src/lib.rs", ""),
        ("src/nested/deep.rs", ""),
        ("src/notes.md", ""),
        ("Cargo.toml", "[package]\n"),
    ]);
    let mut ctx = context(&root);
    let result =
        Glob.call(&json_args(serde_json::json!({ "pattern": "**/*.rs" })), &mut ctx).await.unwrap();
    let text = text_of(&result);
    assert!(text.contains("src/lib.rs"), "{text}");
    assert!(text.contains("src/nested/deep.rs"), "{text}");
    assert!(!text.contains("notes.md"), "{text}");
}

#[tokio::test]
async fn glob_is_sorted_and_reports_empty() {
    let root = workspace(&[("b.rs", ""), ("a.rs", "")]);
    let mut ctx = context(&root);
    let result =
        Glob.call(&json_args(serde_json::json!({ "pattern": "*.rs" })), &mut ctx).await.unwrap();
    let text = text_of(&result);
    let a = text.find("a.rs").unwrap();
    let b = text.find("b.rs").unwrap();
    assert!(a < b, "results must be sorted: {text}");

    let empty =
        Glob.call(&json_args(serde_json::json!({ "pattern": "*.cpp" })), &mut ctx).await.unwrap();
    assert!(text_of(&empty).contains("No files match"));
}

#[tokio::test]
async fn read_file_numbers_lines_and_reports_the_total() {
    let root = workspace(&[("src/lib.rs", "alpha\nbeta\ngamma\n")]);
    let mut ctx = context(&root);
    let result =
        ReadFile.call(&json_args(serde_json::json!({ "path": "src/lib.rs" })), &mut ctx).await
            .unwrap();
    let text = text_of(&result);
    assert!(text.contains("1\talpha"), "{text}");
    assert!(text.contains("3\tgamma"), "{text}");
    assert!(text.contains("[lines 1-3 of 3"), "{text}");
    assert!(text.contains("sha256:"), "{text}");
}

#[tokio::test]
async fn read_file_window_reports_the_continuation() {
    let mut content = String::new();
    for index in 1..=10 {
        content.push_str(&format!("line {index}\n"));
    }
    let root = workspace(&[("src/lib.rs", &content)]);
    let mut ctx = context(&root);
    let result = ReadFile
        .call(
            &json_args(serde_json::json!({ "path": "src/lib.rs", "offset": 3, "limit": 2 })),
            &mut ctx,
        )
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("3\tline 3"), "{text}");
    assert!(text.contains("4\tline 4"), "{text}");
    assert!(!text.contains("5\tline 5"), "{text}");
    assert!(text.contains("continue with offset=5"), "{text}");
}

#[tokio::test]
async fn read_file_rejects_a_directory() {
    let root = workspace(&[("src/lib.rs", "fn main() {}\n")]);
    let mut ctx = context(&root);
    let error = ReadFile
        .call(&json_args(serde_json::json!({ "path": "src" })), &mut ctx)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("is a directory"), "{error}");
}

#[tokio::test]
async fn read_file_notes_the_path_for_staleness_tracking() {
    let root = workspace(&[("src/lib.rs", "fn main() {}\n")]);
    let mut ctx = context(&root);
    ReadFile.call(&json_args(serde_json::json!({ "path": "src/lib.rs" })), &mut ctx).await.unwrap();
    assert_eq!(ctx.read_paths.len(), 1);
    assert!(ctx.read_paths[0].ends_with("src/lib.rs") || ctx.read_paths[0].ends_with("src\\lib.rs"));
}

#[tokio::test]
async fn read_file_reports_an_empty_file() {
    let root = workspace(&[("empty.txt", "")]);
    let mut ctx = context(&root);
    let result = ReadFile
        .call(&json_args(serde_json::json!({ "path": "empty.txt" })), &mut ctx)
        .await
        .unwrap();
    assert!(text_of(&result).contains("empty file"));
}

#[tokio::test]
async fn search_tools_are_registered_and_read_only() {
    let registry = Registry::with_builtins();
    for name in ["Grep", "Glob", "EditFile", "ReadFile"] {
        let tool = registry.tool(name).unwrap_or_else(|| panic!("{name} must be registered"));
        if name != "EditFile" {
            assert!(tool.read_only(), "{name} should be read-only");
        }
    }
    // Sorted output keeps the request body stable across calls.
    let names = registry.tool_names();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn grep_result_is_workspace_relative() {
    let root = workspace(&[("deep/dir/file.rs", "needle\n")]);
    let mut ctx = context(&root);
    let result = Grep.call(&json_args(serde_json::json!({ "pattern": "needle" })), &mut ctx).await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("deep/dir/file.rs") || text.contains("deep\\dir\\file.rs"), "{text}");
    assert!(!text.contains(&root.to_string_lossy().replace('\\', "/")), "absolute path leaked: {text}");
}
