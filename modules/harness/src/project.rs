//! Project instruction files: workspace-owned rules for the agent.

use std::path::Path;

use metteur_shared::config::LlmConfig;
use metteur_shared::llm::SystemFragment;

use super::sections::{PRIORITY_PROJECT, SCOPE_PREFIX};

/// Loads the first project instruction file that exists at the workspace root.
///
/// Only the root is probed: a file further down would be ambiguous when
/// several exist, and the agent can read a nested one with `ReadFile`.
///
/// Candidate names come from configuration, which a workspace controls, so a
/// name that is not a plain file name is ignored — otherwise a repository could
/// point the prompt at an arbitrary file on the machine.
pub fn fragment(workspace_root: &Path, config: &LlmConfig) -> Option<SystemFragment> {
    if !config.project_instructions {
        return None;
    }
    let (name, text) = config.project_instruction_files.iter().find_map(|name| {
        if !is_plain_file_name(name) {
            return None;
        }
        let candidate = workspace_root.join(name);
        let text = std::fs::read_to_string(&candidate).ok()?;
        Some((name.clone(), text))
    })?;
    let content = text.trim();
    if content.is_empty() {
        return None;
    }
    let capped = cap_bytes(content, config.project_instructions_max_bytes as usize);
    Some(SystemFragment {
        priority: PRIORITY_PROJECT,
        scope: format!("{SCOPE_PREFIX}project"),
        content: format!("<project-instructions source=\"{name}\">\n{capped}\n</project-instructions>"),
    })
}

/// Whether `name` may be probed at the workspace root.
///
/// Names with a path separator (or a bare `..`) could reach outside the
/// workspace, so they are rejected rather than resolved.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != ".."
        && !name.contains(['/', '\\'])
        && !name.contains(':')
        && Path::new(name).components().count() == 1
}

/// Truncates `text` to at most `max` bytes on a character boundary.
///
/// `max == 0` disables the cap. The truncation note is part of the returned
/// text so the model knows the file was cut rather than short.
fn cap_bytes(text: &str, max: usize) -> String {
    if max == 0 || text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[... truncated at {max} bytes]", &text[..end])
}
