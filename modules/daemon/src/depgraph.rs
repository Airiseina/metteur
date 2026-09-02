//! Heuristic import scanning for the workspace dependency graph.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use metteur_shared::Value;

use crate::error::DaemonResult;

/// Dependency information for one workspace file.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DepEntry {
    /// Files this file imports.
    pub imports: Vec<String>,
    /// Files importing this file.
    pub imported_by: Vec<String>,
}

/// The workspace-wide dependency graph keyed by `/`-separated relative paths.
pub type DepGraph = HashMap<String, DepEntry>;

/// Directories excluded from scanning.
const SKIPPED_DIRS: &[&str] =
    &[".metteur", ".git", "target", "node_modules", "dist", "build", ".idea", ".vscode"];

/// Scans the workspace tree and derives the import graph.
///
/// Supported languages are Rust (`mod` declarations), TypeScript/JavaScript
/// (relative imports) and Python (package-relative imports). Unresolvable
/// references are ignored.
pub fn scan(root: &Path) -> DepGraph {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);

    let mut graph: DepGraph = files.iter().map(|f| (f.rel.clone(), DepEntry::default())).collect();

    // Collect edges first so the graph can be mutated without aliasing.
    let mut edges: Vec<(String, String)> = Vec::new();
    for file in &files {
        let content = match std::fs::read_to_string(&file.abs) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let targets = match file.ext.as_str() {
            "rs" => resolve_rust(file, &content, &files),
            "ts" | "tsx" | "js" | "jsx" => resolve_js_ts(file, &content, &files),
            "py" => resolve_python(file, &content, &files),
            _ => Vec::new(),
        };
        for target in targets {
            if target != file.rel && !edges.contains(&(file.rel.clone(), target.clone())) {
                edges.push((file.rel.clone(), target));
            }
        }
    }
    for (from, to) in edges {
        if let Some(entry) = graph.get_mut(&from) {
            entry.imports.push(to.clone());
        }
        if let Some(back) = graph.get_mut(&to) {
            back.imported_by.push(from);
        }
    }
    graph
}

/// Returns the dependency entry for one file within the execution's workspace.
pub fn lookup(
    ctx: &crate::execution::context::ExecutionContext,
    path: &str,
) -> DaemonResult<DepEntry> {
    use crate::workspace::fs::WorkspaceFs;
    let fs = WorkspaceFs::new(ctx.workspace_root.clone());
    let abs = fs.resolve_existing(path)?;
    let rel =
        abs.strip_prefix(&ctx.workspace_root).unwrap_or(&abs).to_string_lossy().replace('\\', "/");
    let normalized = rel.trim_start_matches('/');
    Ok(scan(&ctx.workspace_root).remove(normalized).unwrap_or_default())
}

/// Converts a [`Value`] list argument into a string list.
#[allow(dead_code)]
fn value_strings(value: &[Value]) -> Vec<String> {
    value.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

struct SourceFile {
    rel: String,
    abs: PathBuf,
    ext: String,
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<SourceFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                collect_files(root, &path, out);
            }
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        if !matches!(ext.as_str(), "rs" | "ts" | "tsx" | "js" | "jsx" | "py") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        out.push(SourceFile {
            rel: rel.trim_start_matches('/').to_string(),
            abs: path,
            ext,
        });
    }
}

/// Resolves Rust `mod x;` declarations to their module files.
fn resolve_rust(file: &SourceFile, content: &str, all: &[SourceFile]) -> Vec<String> {
    let base_dir = Path::new(&file.rel).parent().unwrap_or(Path::new("")).to_path_buf();
    let is_mod_file = matches!(
        Path::new(&file.rel).file_name().map(|n| n == "mod.rs" || n == "main.rs" || n == "lib.rs"),
        Some(true)
    );
    let mut targets = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("mod ") else {
            continue;
        };
        let name = rest.split(';').next().unwrap_or("").trim();
        // Skip inline modules (`mod x { ... }`) and attributes.
        if name.is_empty() || name.contains('{') || trimmed.starts_with('#') {
            continue;
        }
        // A `foo.rs`/`foo/mod.rs` sits beside its parent for non-mod files,
        // otherwise it lives in the declaring directory.
        let candidates: Vec<PathBuf> = if is_mod_file {
            vec![base_dir.join(format!("{name}.rs")), base_dir.join(name).join("mod.rs")]
        } else {
            let parent_dir = base_dir.parent().unwrap_or(Path::new("")).to_path_buf();
            vec![
                parent_dir
                    .join(base_dir.file_name().unwrap_or_default())
                    .join(format!("{name}.rs")),
                parent_dir.join(base_dir.file_name().unwrap_or_default()).join(name).join("mod.rs"),
            ]
        };
        for candidate in candidates {
            let candidate_rel = normalize_rel(&candidate);
            if all.iter().any(|f| f.rel == candidate_rel) {
                targets.push(candidate_rel);
                break;
            }
        }
    }
    targets
}

/// Resolves relative imports in TS/JS sources.
fn resolve_js_ts(file: &SourceFile, content: &str, all: &[SourceFile]) -> Vec<String> {
    let mut specifiers = Vec::new();
    for line in content.lines() {
        for token in ["from \"", "from '", "import(\"", "import('"] {
            if let Some(idx) = line.find(token) {
                let rest = &line[idx + token.len()..];
                if let Some(end) = rest.find(['"', '\'']) {
                    specifiers.push(rest[..end].to_string());
                }
            }
        }
    }
    let base_dir = Path::new(&file.rel).parent().unwrap_or(Path::new("")).to_path_buf();
    let mut targets = Vec::new();
    for spec in specifiers {
        if !spec.starts_with("./") && !spec.starts_with("../") {
            continue;
        }
        let stripped: PathBuf = base_dir.join(spec.trim_start_matches("./"));
        let base = display(&stripped);
        let candidates = [
            format!("{base}.ts"),
            format!("{base}.tsx"),
            format!("{base}.js"),
            format!("{base}.jsx"),
            format!("{base}/index.ts"),
            format!("{base}/index.js"),
            base,
        ];
        for candidate in candidates {
            if all.iter().any(|f| f.rel == candidate) {
                targets.push(candidate);
                break;
            }
        }
    }
    targets
}

/// Resolves package-relative Python imports.
fn resolve_python(file: &SourceFile, content: &str, all: &[SourceFile]) -> Vec<String> {
    let mut targets = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("from ") {
            let mut tokens = rest.split_whitespace();
            let Some(module) = tokens.next() else {
                continue;
            };
            // `from . import b` yields tokens [".", "import", "b"]; the item
            // after the `import` keyword may be a submodule or a symbol.
            let item = match (tokens.next(), tokens.next()) {
                (Some("import"), Some(item)) => Some(item.split(',').next().unwrap_or(item)),
                _ => None,
            };
            let base = relative_module_dir(&file.rel, module);
            match item {
                Some(item) => {
                    push_if_exists(all, &mut targets, &format!("{base}/{item}.py"));
                    push_if_exists(all, &mut targets, &format!("{base}/{item}/__init__.py"));
                    // A plain symbol may also live in the package __init__.
                    if !item.contains('.') {
                        push_if_exists(all, &mut targets, &format!("{base}/__init__.py"));
                    }
                }
                None => {
                    push_if_exists(all, &mut targets, &format!("{base}.py"));
                    push_if_exists(all, &mut targets, &format!("{base}/__init__.py"));
                }
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            let module =
                rest.split(',').next().unwrap_or("").split_whitespace().next().unwrap_or("");
            let (_, name) = split_leading_dots(module);
            if name.is_empty() {
                continue;
            }
            let rel_path = name.replace('.', "/");
            push_if_exists(all, &mut targets, &format!("{rel_path}.py"));
            push_if_exists(all, &mut targets, &format!("{rel_path}/__init__.py"));
        }
    }
    targets
}

/// Splits a module reference into its leading-dot count and the bare name.
fn split_leading_dots(module: &str) -> (usize, &str) {
    let trimmed = module.trim_start_matches('.');
    let dots = module.len() - trimmed.len();
    (dots, trimmed)
}

/// Resolves a possibly dotted/relative module reference against the declaring
/// file's directory: one dot stays in place, each extra dot moves up a level.
fn relative_module_dir(rel: &str, module: &str) -> String {
    let (dots, name) = split_leading_dots(module);
    let mut dir = Path::new(rel).parent().unwrap_or(Path::new("")).to_path_buf();
    for _ in 1..dots {
        dir = dir.parent().unwrap_or(Path::new("")).to_path_buf();
    }
    let base = display(&dir);
    join_module(&base, name)
}

fn join_module(base: &str, module: &str) -> String {
    if module.is_empty() {
        return base.to_string();
    }
    if base.is_empty() {
        return module.replace('.', "/");
    }
    format!("{base}/{}", module.replace('.', "/"))
}

fn push_if_exists(all: &[SourceFile], targets: &mut Vec<String>, candidate: &str) {
    if all.iter().any(|f| f.rel == candidate) && !targets.iter().any(|t| t == candidate) {
        targets.push(candidate.to_string());
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").trim_start_matches('/').to_string()
}

fn normalize_rel(path: &Path) -> String {
    display(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn resolves_rust_modules() {
        let dir = std::env::temp_dir().join(format!("metteur-dep-{}", uuid::Uuid::new_v4()));
        write(&dir, "src/main.rs", "mod util;\nfn main() {}\n");
        write(&dir, "src/util.rs", "pub fn go() {}\n");

        let graph = scan(&dir);
        assert!(graph["src/main.rs"].imports.contains(&"src/util.rs".to_string()));
        assert!(graph["src/util.rs"].imported_by.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn resolves_relative_js_imports() {
        let dir = std::env::temp_dir().join(format!("metteur-dep-{}", uuid::Uuid::new_v4()));
        write(&dir, "app/index.ts", "import { x } from \"./lib\";\n");
        write(&dir, "app/lib.ts", "export const x = 1;\n");

        let graph = scan(&dir);
        assert!(graph["app/index.ts"].imports.contains(&"app/lib.ts".to_string()));
    }

    #[test]
    fn resolves_python_packages() {
        let dir = std::env::temp_dir().join(format!("metteur-dep-{}", uuid::Uuid::new_v4()));
        write(&dir, "pkg/__init__.py", "");
        write(&dir, "pkg/a.py", "from . import b\n");
        write(&dir, "pkg/b.py", "");

        let graph = scan(&dir);
        assert!(graph["pkg/a.py"].imports.contains(&"pkg/b.py".to_string()));
    }

    #[test]
    fn skipped_directories_are_ignored() {
        let dir = std::env::temp_dir().join(format!("metteur-dep-{}", uuid::Uuid::new_v4()));
        write(&dir, "src/a.rs", "");
        write(&dir, "target/overlay/b.rs", "");

        let graph = scan(&dir);
        assert!(!graph.contains_key("target/overlay/b.rs"));
    }

    #[test]
    fn value_strings_extracts_text() {
        let values = vec![Value::String("a".into()), Value::Int(1)];
        assert_eq!(value_strings(&values), vec!["a".to_string()]);
    }
}
