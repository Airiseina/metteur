//! Prompt assembly: which fragments a run carries and in what order.

use metteur_shared::config::Config;
use metteur_shared::llm::{ContextManager, SystemFragment};

use super::environment;
use super::facts::EnvFacts;
use super::project;
use super::sections::{self, PRIORITY_APPEND, SCOPE_PREFIX};

/// Renders the harness fragments of a run.
///
/// Assembly is a pure function of its inputs (facts, configuration and the
/// files the configuration points at), which is what makes the rendered prefix
/// reproducible across turns and processes — the precondition for provider
/// prefix caching.
pub struct HarnessPrompt;

impl HarnessPrompt {
    /// Builds the harness fragments for a workspace and configuration.
    pub fn fragments_for(facts: &EnvFacts, config: &Config) -> Vec<SystemFragment> {
        let mut fragments = sections::static_fragments();
        if let Some(project) = project::fragment(&facts.workspace_root, &config.llm) {
            fragments.push(project);
        }
        fragments.push(environment::fragment(facts));
        fragments.push(environment::date_fragment(facts));
        if let Some(extra) = config
            .llm
            .system_prompt_append
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            fragments.push(SystemFragment {
                priority: PRIORITY_APPEND,
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
pub fn render_all<'a>(fragments: impl Iterator<Item = &'a SystemFragment>) -> String {
    let mut list: Vec<&SystemFragment> = fragments.collect();
    list.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.scope.cmp(&b.scope))
            .then_with(|| a.content.cmp(&b.content))
    });
    list.iter().map(|fragment| fragment.content.as_str()).collect::<Vec<_>>().join("\n\n")
}
