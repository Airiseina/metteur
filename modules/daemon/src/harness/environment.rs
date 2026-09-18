//! The environment block injected into the harness prompt.

use metteur_shared::llm::SystemFragment;

use super::SCOPE_PREFIX;
use super::sections::PRIORITY_ENVIRONMENT;

/// Renders the environment block for a workspace.
///
/// The block contains only facts that stay constant for the lifetime of a
/// session (path, platform, date, model), so restoring a session usually
/// reproduces it byte for byte and leaves the cached prefix intact. The date
/// rolls over at most once per day.
pub fn fragment(workspace_root: &std::path::Path, model: Option<&str>) -> SystemFragment {
    let mut content = String::from("<environment>\n");
    content.push_str(&format!("workspace: {}\n", workspace_root.display()));
    content.push_str(&format!("platform: {} ({})\n", std::env::consts::OS, std::env::consts::ARCH));
    content.push_str(&format!("date: {}\n", chrono::Utc::now().format("%Y-%m-%d")));
    if let Some(model) = model {
        content.push_str(&format!("model: {model}\n"));
    }
    content.push_str("</environment>");
    SystemFragment {
        priority: PRIORITY_ENVIRONMENT,
        scope: format!("{SCOPE_PREFIX}environment"),
        content,
    }
}
