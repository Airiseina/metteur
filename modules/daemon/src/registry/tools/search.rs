//! Content and filename search tools: `Grep` and `Glob`.
//!
//! Both walk the workspace through `ignore`, so `.gitignore` / `.ignore`
//! rules apply and generated directories (`target`, `node_modules`, the
//! `.metteur` metadata dir) are skipped by default. Results are sorted to keep
//! repeated calls byte-identical, which helps the provider prefix cache.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use ignore::WalkBuilder;
use metteur_shared::{ToolResultLifetime, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::workspace::fs::WorkspaceFs;

use crate::registry::tool::Tool;

use super::Args;

/// Hard cap on matches returned by `Grep` regardless of the request.
const GREP_MAX_MATCHES_HARD: usize = 1000;

/// Default match budget for `Grep`.
const GREP_MAX_MATCHES_DEFAULT: usize = 100;

/// Hard cap on paths returned by `Glob`.
const GLOB_MAX_PATHS: usize = 200;

/// Files larger than this are skipped by `Grep` (likely data, not code).
const GREP_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Directories always excluded from both tools, regardless of ignore files.
///
/// `.metteur` holds daemon state (databases, locks) and the rest are generated
/// build/dependency output; all are noise for a source search.
const ALWAYS_IGNORED: &[&str] = &[".metteur", ".git", "target", "node_modules", "dist", "build"];

/// Searches file contents for a regular expression.
pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Searches file contents across the workspace with a regular \
         expression, honoring .gitignore. Results are grouped per file as \
         `line:column: text` under a `path:` heading, with optional context \
         lines shown as `line- text`. Narrow the search with `glob` (for \
         example `**/*.rs`) when the workspace is large."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regular expression to search for." },
                "path": { "type": "string", "description": "Directory or file to search (default: workspace root)." },
                "glob": { "type": "string", "description": "Only search paths matching this glob, e.g. `**/*.rs`." },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive matching." },
                "context_lines": { "type": "integer", "minimum": 0, "description": "Lines of context around each match (default 0)." },
                "max_matches": { "type": "integer", "minimum": 1, "description": "Maximum matches to return (default 100, hard cap 1000)." }
            },
            "required": ["pattern"]
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
        let pattern = a
            .string("pattern", 0)
            .ok_or_else(|| DaemonError::Execution("Grep requires a pattern".to_string()))?;
        let search_path = a.string("path", 1);
        let glob = a.string("glob", 2);
        let case_insensitive = a
            .get("case_insensitive", 3)
            .and_then(|v| match v {
                Value::Bool(b) => Some(b),
                Value::Json(j) => j.as_bool(),
                _ => None,
            })
            .unwrap_or(false);
        let context_lines = a.int("context_lines", 4).unwrap_or(0).clamp(0, 10) as usize;
        let max_matches = a
            .int("max_matches", 5)
            .filter(|v| *v > 0)
            .map(|v| (v as usize).min(GREP_MAX_MATCHES_HARD))
            .unwrap_or(GREP_MAX_MATCHES_DEFAULT);

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let root = match &search_path {
            Some(path) if !path.is_empty() => fs.resolve_existing(path)?,
            _ => ctx.workspace_root.clone(),
        };
        let root = if root.is_dir() {
            root
        } else {
            root.parent().map(Path::to_path_buf).unwrap_or(ctx.workspace_root.clone())
        };

        let matcher = build_regex(&pattern, case_insensitive)?;
        let glob_set = match &glob {
            Some(glob) if !glob.is_empty() => Some(build_glob(glob)?),
            _ => None,
        };

        let outcome = search_contents(&ctx.workspace_root, &root, &matcher, glob_set.as_ref(), context_lines, max_matches)?;
        ctx.note_read_paths(outcome.files.iter().cloned());
        Ok(Value::String(outcome.rendered))
    }
}

/// Lists workspace files whose path matches a glob pattern.
pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Lists workspace files whose path matches a glob pattern \
         (`*` stays within a directory, `**` crosses directories), honoring \
         .gitignore. Use it to locate files by name or extension."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. `src/**/*.rs` or `**/*.toml`." },
                "path": { "type": "string", "description": "Subdirectory to search under (default: workspace root)." }
            },
            "required": ["pattern"]
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
        let pattern = a
            .string("pattern", 0)
            .ok_or_else(|| DaemonError::Execution("Glob requires a pattern".to_string()))?;
        let search_path = a.string("path", 1);

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let root = match &search_path {
            Some(path) if !path.is_empty() => fs.resolve_existing(path)?,
            _ => ctx.workspace_root.clone(),
        };

        let matcher = build_glob(&pattern)?;
        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in walker(&root) {
            let path = entry?;
            if !path.is_file() {
                continue;
            }
            let relative = display_path(&ctx.workspace_root, &path);
            if !matcher.is_match(&relative) {
                continue;
            }
            if matches.len() >= GLOB_MAX_PATHS {
                truncated = true;
                break;
            }
            matches.push(relative);
        }
        matches.sort();
        let root = ctx.workspace_root.clone();
        ctx.note_read_paths(matches.iter().map(|p| root.join(p)).collect::<Vec<_>>());

        let mut rendered = matches.join("\n");
        if truncated {
            rendered.push_str(&format!(
                "\n... [showing the first {GLOB_MAX_PATHS} matches; narrow the pattern]"
            ));
        }
        if rendered.is_empty() {
            rendered = format!("No files match '{pattern}'.");
        }
        Ok(Value::String(rendered))
    }
}

/// The outcome of a content search.
struct SearchOutcome {
    /// Rendered `path:line:col: text` report.
    rendered: String,
    /// Absolute paths of the files that produced matches.
    files: Vec<PathBuf>,
}

/// Compiles the search pattern, mapping regex errors to actionable text.
fn build_regex(pattern: &str, case_insensitive: bool) -> DaemonResult<regex::Regex> {
    let expression = if case_insensitive {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    };
    regex::Regex::new(&expression).map_err(|err| {
        DaemonError::Execution(format!("invalid regular expression '{pattern}': {err}"))
    })
}

/// Compiles a glob pattern into a matcher.
fn build_glob(pattern: &str) -> DaemonResult<globset::GlobMatcher> {
    globset::Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|err| DaemonError::Execution(format!("invalid glob '{pattern}': {err}")))
}

/// Walks `root` yielding files, honoring `.gitignore` and the built-in skips.
///
/// `require_git(false)` makes ignore files apply outside a git checkout too:
/// workspaces are frequently plain directories, and the rules are still the
/// author's intent.
fn walker(root: &Path) -> impl Iterator<Item = DaemonResult<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .follow_links(false);
    let skip: Vec<&'static str> = ALWAYS_IGNORED.to_vec();
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        !skip.iter().any(|ignored| *ignored == name)
    });
    builder.build().filter_map(|entry| match entry {
        Ok(entry) if entry.file_type().is_some_and(|t| t.is_file()) => {
            Some(Ok(entry.into_path()))
        }
        Ok(_) => None,
        Err(err) => Some(Err(DaemonError::Io(std::io::Error::other(err.to_string())))),
    })
}

/// Renders a path relative to the workspace root with forward slashes.
fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Searches file contents under `root`, rendering matches grouped by file.
fn search_contents(
    workspace_root: &Path,
    root: &Path,
    matcher: &regex::Regex,
    glob: Option<&globset::GlobMatcher>,
    context_lines: usize,
    max_matches: usize,
) -> DaemonResult<SearchOutcome> {
    let mut rendered = String::new();
    let mut matched_files = Vec::new();
    let mut total = 0usize;
    let mut skipped_binary = 0usize;
    let mut truncated = false;

    for entry in walker(root) {
        if truncated {
            break;
        }
        let path = entry?;
        if let Some(glob) = glob {
            let relative = display_path(workspace_root, &path);
            if !glob.is_match(&relative) {
                continue;
            }
        }
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > GREP_MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            skipped_binary += 1;
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            skipped_binary += 1;
            continue;
        };

        let lines: Vec<&str> = text.split('\n').collect();
        let mut file_header_written = false;
        let relative = display_path(workspace_root, &path);
        for (index, line) in lines.iter().enumerate() {
            if total >= max_matches {
                truncated = true;
                break;
            }
            let Some(found) = matcher.find(line) else {
                continue;
            };
            if !file_header_written {
                if !rendered.is_empty() {
                    rendered.push('\n');
                }
                rendered.push_str(&format!("{relative}:\n"));
                rendered.push_str("---\n");
                file_header_written = true;
                matched_files.push(path.clone());
            }
            let context_start = index.saturating_sub(context_lines);
            let context_end = (index + context_lines + 1).min(lines.len());
            for (context_index, context_line) in
                lines[context_start..context_end].iter().enumerate()
            {
                let number = context_start + context_index + 1;
                let trimmed = context_line.strip_suffix('\r').unwrap_or(context_line);
                if number == index + 1 {
                    rendered.push_str(&format!(
                        "{number}:{}: {trimmed}\n",
                        found.start() + 1
                    ));
                } else {
                    rendered.push_str(&format!("{number}- {trimmed}\n"));
                }
            }
            rendered.push('\n');
            total += 1;
        }
    }

    if total == 0 {
        rendered = "No matches found.".to_string();
    } else {
        let mut summary = format!("[{total} match(es)");
        if truncated {
            summary.push_str(&format!("; stopped at the {max_matches} match limit"));
        }
        if skipped_binary > 0 {
            summary.push_str(&format!("; {skipped_binary} binary file(s) skipped"));
        }
        summary.push_str("]\n");
        rendered = format!("{summary}{rendered}");
    }
    Ok(SearchOutcome {
        rendered,
        files: matched_files,
    })
}
