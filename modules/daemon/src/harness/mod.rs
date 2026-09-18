//! Harness prompt assembly.
//!
//! Every model conversation starts from a set of system fragments owned by the
//! harness: the agent's identity and working rules, a description of the
//! environment, and the workspace's project instructions. They are ordinary
//! [`SystemFragment`]s distinguished by their `harness.` scope prefix, which
//! lets [`HarnessPrompt::refresh`] replace exactly the harness-owned fragments
//! while leaving addon and node fragments alone.
//!
//! Fragment priority decides the rendered order (higher first): identity,
//! tool rules, coding standards, plan rules, environment, project
//! instructions, then whatever a node or the user appends.

pub mod environment;
pub mod project;
pub mod sections;

use metteur_shared::llm::{ContextManager, SystemFragment};

use crate::execution::context::ExecutionContext;

/// Scope prefix of every harness-owned fragment.
pub const SCOPE_PREFIX: &str = "harness.";

/// Renders the harness fragments of the current run.
pub struct HarnessPrompt;

impl HarnessPrompt {
    /// Builds the harness fragments for `ctx`'s workspace and configuration.
    pub async fn fragments(ctx: &ExecutionContext) -> Vec<SystemFragment> {
        let config = match &ctx.config {
            Some(config) => config.read().await.clone(),
            None => metteur_shared::config::Config::default(),
        };
        Self::fragments_for(&ctx.workspace_root, &config)
    }

    /// Builds the harness fragments for a workspace and configuration.
    pub fn fragments_for(
        workspace_root: &std::path::Path,
        config: &metteur_shared::config::Config,
    ) -> Vec<SystemFragment> {
        let mut fragments = sections::static_fragments();
        fragments.push(environment::fragment(workspace_root, model_label(config).as_deref()));
        if let Some(project) = project::fragment(workspace_root, &config.llm) {
            fragments.push(project);
        }
        if let Some(extra) = config
            .llm
            .system_prompt_append
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            fragments.push(SystemFragment {
                priority: sections::PRIORITY_APPEND,
                scope: format!("{SCOPE_PREFIX}append"),
                content: extra.to_string(),
            });
        }
        fragments
    }

    /// Replaces every harness fragment in `context`, keeping the rest.
    pub fn apply(context: &mut ContextManager, fragments: Vec<SystemFragment>) {
        context.system_fragments.retain(|fragment| !is_harness_fragment(fragment));
        context.system_fragments.extend(fragments);
    }

    /// Replaces the harness fragments only when the rendered text changed.
    ///
    /// Restoring a session rebuilds the harness from the current environment;
    /// writing an identical prefix back would still be a no-op for the model,
    /// but keeping the stored fragments untouched guarantees the request bytes
    /// stay identical, which is what the provider prefix cache needs. Returns
    /// whether the context was modified.
    pub fn refresh(context: &mut ContextManager, fragments: Vec<SystemFragment>) -> bool {
        // The whole system prompt is compared, not just the harness subset:
        // fragment priorities interleave with addon fragments, so a stale
        // priority would render differently from the fresh one even with
        // identical text.
        let kept = context.system_fragments.iter().filter(|f| !is_harness_fragment(f));
        let candidate = render_all(kept.chain(fragments.iter()));
        if candidate == render_all(context.system_fragments.iter()) {
            return false;
        }
        Self::apply(context, fragments);
        true
    }

    /// The system fragment used when the context is summarized.
    pub fn compress_fragment() -> SystemFragment {
        SystemFragment {
            priority: 0,
            scope: format!("{SCOPE_PREFIX}compress"),
            content: sections::COMPRESS.to_string(),
        }
    }
}

/// Whether a fragment is owned by the harness.
pub fn is_harness_fragment(fragment: &SystemFragment) -> bool {
    fragment.scope.starts_with(SCOPE_PREFIX)
}

/// Renders a fragment set the way a provider would: sorted by descending
/// priority, then scope and content, joined into one system prompt.
fn render_all<'a>(fragments: impl Iterator<Item = &'a SystemFragment>) -> String {
    let mut list: Vec<&SystemFragment> = fragments.collect();
    list.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.scope.cmp(&b.scope))
            .then_with(|| a.content.cmp(&b.content))
    });
    list.iter().map(|fragment| fragment.content.as_str()).collect::<Vec<_>>().join("\n\n")
}

/// The configured default model, rendered as `key` or `key (api_type)`.
fn model_label(config: &metteur_shared::config::Config) -> Option<String> {
    let key = config.llm.default_model.as_deref().filter(|key| !key.is_empty())?;
    let api_type = config
        .llm
        .models
        .get(key)
        .map(|model| model.api_type.as_str())
        .filter(|api_type| !api_type.is_empty());
    Some(match api_type {
        Some(api_type) => format!("{key} ({api_type})"),
        None => key.to_string(),
    })
}
