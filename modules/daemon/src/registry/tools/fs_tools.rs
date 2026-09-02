//! Built-in tools for file and directory operations.

use async_trait::async_trait;
use metteur_shared::Value;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::workspace::fs::WorkspaceFs;

use crate::registry::tool::Tool;

use super::Args;

/// Reads a file's contents.
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "ReadFile"
    }

    fn description(&self) -> &str {
        "Reads the contents of a file at the given path."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("ReadFile requires a path".to_string()))?;
        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let data = fs.read(&path)?;
        Ok(Value::String(String::from_utf8_lossy(&data).to_string()))
    }
}

/// Writes content to a file, recording the change for rollback.
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "WriteFile"
    }

    fn description(&self) -> &str {
        "Writes content to a file at the given path, creating it if needed."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." },
                "content": { "type": "string", "description": "Content to write." }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("WriteFile requires a path".to_string()))?;
        let content = a
            .string("content", 1)
            .ok_or_else(|| DaemonError::Execution("WriteFile requires content".to_string()))?;

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let resolved = fs.resolve(&path)?;
        let old_content = std::fs::read(&resolved).ok();
        ctx.transaction_log.record_file_write(
            resolved.clone(),
            old_content,
            content.clone().into_bytes(),
        );
        fs.write(&path, content.as_bytes())?;
        Ok(Value::Bool(true))
    }
}

/// Lists the entries in a directory.
pub struct ListDirectory;

#[async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &str {
        "ListDirectory"
    }

    fn description(&self) -> &str {
        "Lists the entries directly under the given directory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the directory." }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("ListDirectory requires a path".to_string()))?;
        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let entries = fs.list(&path)?;
        let names: Vec<Value> =
            entries.into_iter().map(|p| Value::String(p.to_string_lossy().to_string())).collect();
        Ok(Value::List(names))
    }
}

/// Searches for files by name under a directory.
pub struct SearchFile;

#[async_trait]
impl Tool for SearchFile {
    fn name(&self) -> &str {
        "SearchFile"
    }

    fn description(&self) -> &str {
        "Searches for files whose name contains the given query under a directory."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "Root directory to search." },
                "query": { "type": "string", "description": "Substring to match against file names." }
            },
            "required": ["root", "query"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let root = a
            .string("root", 0)
            .ok_or_else(|| DaemonError::Execution("SearchFile requires a root".to_string()))?;
        let query = a
            .string("query", 1)
            .ok_or_else(|| DaemonError::Execution("SearchFile requires a query".to_string()))?;
        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let root = fs.resolve_existing(&root)?;
        let matches = walk(&root, &query)?;
        Ok(Value::List(
            matches.into_iter().map(|p| Value::String(p.to_string_lossy().to_string())).collect(),
        ))
    }
}

/// Recursively walks `dir`, returning paths whose file name contains `query`.
fn walk(dir: &std::path::Path, query: &str) -> DaemonResult<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().map(|n| n == ".metteur").unwrap_or(false) {
                continue;
            }
            out.extend(walk(&path, query)?);
        } else if path.file_name().map(|n| n.to_string_lossy().contains(query)).unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_ctx() -> ExecutionContext {
        ExecutionContext::new(
            std::sync::Arc::new(crate::registry::Registry::with_builtins()),
            crate::llm::LlmClientFactory::new(),
            std::env::temp_dir(),
        )
    }

    #[tokio::test]
    async fn read_file_returns_content() {
        let dir = std::env::temp_dir().join(format!("metteur-tool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "hello").unwrap();

        let mut ctx = new_ctx();
        let result = ReadFile
            .call(&[Value::String(path.to_string_lossy().to_string())], &mut ctx)
            .await
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "hello");
    }

    #[tokio::test]
    async fn write_file_records_transaction() {
        let dir = std::env::temp_dir().join(format!("metteur-tool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b.txt");

        let mut ctx = new_ctx();
        WriteFile
            .call(
                &[
                    Value::String(path.to_string_lossy().to_string()),
                    Value::String("data".to_string()),
                ],
                &mut ctx,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
        assert_eq!(ctx.transaction_log.entries().len(), 1);
    }

    #[tokio::test]
    async fn search_file_finds_matches() {
        let dir = std::env::temp_dir().join(format!("metteur-tool-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("lib.rs"), "pub fn lib() {}").unwrap();

        let mut ctx = new_ctx();
        let result = SearchFile
            .call(
                &[
                    Value::String(dir.to_string_lossy().to_string()),
                    Value::String("main".to_string()),
                ],
                &mut ctx,
            )
            .await
            .unwrap();
        let Value::List(items) = result else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 1);
    }
}
