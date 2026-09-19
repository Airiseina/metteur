//! Built-in tools for file and directory operations.

use async_trait::async_trait;
use metteur_shared::{ToolResultLifetime, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::workspace::fs::WorkspaceFs;

use crate::registry::tool::Tool;

use super::Args;

/// Default byte budget for one `ReadFile` response.
const READ_MAX_BYTES: usize = 16 * 1024;

/// Reads a file's contents with optional line windowing.
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "ReadFile"
    }

    fn description(&self) -> &str {
        "Reads a text file and returns it with 1-based line numbers, plus the \
         total line count and a content hash. Use `offset` and `limit` to read \
         a window of a large file. The line numbers are for reference only: \
         never include them in EditFile.old_string. Set `line_numbers` to \
         false to get the raw content (for blueprints that feed the text into \
         a validator or parser)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based, default 1)." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read." },
                "line_numbers": { "type": "boolean", "description": "Prefix each line with its number and append a summary (default true)." }
            },
            "required": ["path"]
        })
    }

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("ReadFile requires a path".to_string()))?;
        let offset = a.int("offset", 1).filter(|v| *v > 0).unwrap_or(1) as usize;
        let limit = a.int("limit", 2).filter(|v| *v > 0).map(|v| v as usize);
        let line_numbers = a.bool("line_numbers", 3).unwrap_or(true);

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let resolved = fs.resolve_existing(&path)?;
        if resolved.is_dir() {
            return Err(DaemonError::Execution(format!(
                "{path} is a directory; use ListDirectory or Glob instead"
            )));
        }
        let data = fs.read(&path)?;
        let text = String::from_utf8(data).map_err(|_| {
            DaemonError::Execution(format!(
                "{path} is not valid UTF-8; ReadFile only handles text files"
            ))
        })?;

        // A window of a large file cannot stand in for the whole file, so only
        // a complete read is allowed to supersede an earlier one.
        if offset == 1 && limit.is_none() {
            ctx.note_full_read([resolved]);
        } else {
            ctx.note_read_paths([resolved]);
        }
        if line_numbers {
            Ok(Value::String(render_lines(&text, offset, limit)))
        } else {
            Ok(Value::String(render_raw(&text, offset, limit)))
        }
    }
}

/// Returns a plain text window without numbering or a summary line.
fn render_raw(text: &str, offset: usize, limit: Option<usize>) -> String {
    if offset <= 1 && limit.is_none() {
        return text.to_string();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let start = (offset - 1).min(lines.len());
    let end = match limit {
        Some(limit) => start.saturating_add(limit).min(lines.len()),
        None => lines.len(),
    };
    lines[start..end].join("\n")
}

/// Renders a file window as numbered lines with a trailing summary.
///
/// The window is cut at whole lines so the model never sees a spliced line.
fn render_lines(text: &str, offset: usize, limit: Option<usize>) -> String {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let total = lines.len();
    let sha = short_hash(text.as_bytes());
    if total == 0 {
        return format!("[empty file | sha256: {sha}]");
    }

    let start = (offset - 1).min(total);
    let requested_end = match limit {
        Some(limit) => start.saturating_add(limit).min(total),
        None => total,
    };
    let width = total.to_string().len();
    let mut out = String::new();
    let mut end = start;
    for (index, line) in lines[start..requested_end].iter().enumerate() {
        let number = start + index + 1;
        // Strip a trailing CR so CRLF files render cleanly; EditFile works on
        // the same normalized view.
        let rendered = line.strip_suffix('\r').unwrap_or(line);
        let row = format!("{number:>width$}\t{rendered}\n");
        // Stop before exceeding the budget, keeping at least one line.
        if end > start && out.len() + row.len() >= READ_MAX_BYTES {
            break;
        }
        out.push_str(&row);
        end = start + index + 1;
    }

    let remaining = total.saturating_sub(end);
    if remaining > 0 {
        out.push_str(&format!(
            "[lines {}-{} of {total} | continue with offset={} | sha256: {sha}]\n",
            start + 1,
            end,
            end + 1
        ));
    } else {
        out.push_str(&format!("[lines {}-{} of {total} | sha256: {sha}]\n", start + 1, end));
    }
    out
}

/// Returns the first 16 hex characters of the SHA-256 digest.
pub fn short_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Writes content to a file, recording the change for rollback.
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "WriteFile"
    }

    fn description(&self) -> &str {
        "Writes content to a file at the given path, creating it if needed. \
         This replaces the whole file: to change part of an existing file, \
         prefer EditFile so unrelated content is preserved."
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

    fn max_result_bytes(&self) -> usize {
        4 * 1024
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
        // The permission mode decides whether this write is confirmed first.
        let summary = format!("write {} bytes", content.len());
        if !crate::sandbox::authorize_write(ctx, &resolved, &path, &summary).await? {
            return Err(DaemonError::Sandbox(format!("write denied by permission mode: {path}")));
        }
        let old_content = std::fs::read(&resolved).ok();
        ctx.transaction_log.record_file_write(
            resolved.clone(),
            old_content,
            content.clone().into_bytes(),
        );
        fs.write(&path, content.as_bytes())?;
        ctx.note_file_mutation(&resolved);
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
        "Lists the entries directly under the given directory, one per line \
         with a trailing `/` for subdirectories."
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

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("ListDirectory requires a path".to_string()))?;
        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let dir = fs.resolve_existing(&path)?;
        if !dir.is_dir() {
            return Err(DaemonError::Execution(format!("{path} is not a directory")));
        }
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_dir() {
                names.push(format!("{name}/"));
            } else {
                names.push(name);
            }
        }
        names.sort();
        Ok(Value::String(names.join("\n")))
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
        "Finds files whose name contains the given query. Prefer Glob for \
         precise name patterns and Grep to search file contents."
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

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
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
        let dir = fs.resolve_existing(&root)?;
        let mut matches = walk(&dir, &query)?;
        matches.sort();
        ctx.note_read_paths(matches.iter().cloned());
        // Workspace-relative display keeps results stable across machines.
        let relative: Vec<Value> = matches
            .into_iter()
            .map(|path| {
                let display = path
                    .strip_prefix(&ctx.workspace_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                Value::String(display)
            })
            .collect();
        Ok(Value::List(relative))
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
