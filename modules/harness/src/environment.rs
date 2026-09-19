//! The environment block injected into the harness prompt.
//!
//! Split in two on purpose: the workspace path and platform stay constant for
//! the lifetime of a session, while the date and the model can change between
//! turns. Two fragments let the volatile half render last, so a date rollover
//! invalidates only the tail of the cached provider prefix.

use metteur_shared::llm::SystemFragment;

use super::facts::EnvFacts;
use super::sections::{PRIORITY_ENV_DATE, PRIORITY_ENVIRONMENT, SCOPE_PREFIX};

/// Renders the stable half of the environment block.
pub fn fragment(facts: &EnvFacts) -> SystemFragment {
    let mut content = String::from("<environment>\n");
    content.push_str(&format!("workspace: {}\n", facts.workspace_root.display()));
    content.push_str(&format!("platform: {}\n", facts.platform()));
    content.push_str("</environment>");
    SystemFragment {
        priority: PRIORITY_ENVIRONMENT,
        scope: format!("{SCOPE_PREFIX}env.base"),
        content,
    }
}

/// Renders the volatile half: the current date and the configured model.
///
/// Rendered after [`fragment`], so a change here leaves every earlier section
/// cached.
pub fn date_fragment(facts: &EnvFacts) -> SystemFragment {
    let mut content = format!("current date: {}", facts.today);
    if let Some(model) = facts.model.as_deref().filter(|model| !model.is_empty()) {
        content.push_str(&format!("\nmodel: {model}"));
    }
    SystemFragment {
        priority: PRIORITY_ENV_DATE,
        scope: format!("{SCOPE_PREFIX}env.date"),
        content,
    }
}
