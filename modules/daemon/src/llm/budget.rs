//! Context-window budgeting.
//!
//! The window guard keeps a request from overflowing the model's input limit.
//! It combines the model's input window (configured, or a built-in default for
//! a known model family) with the reserved output budget and a safety margin.
//!
//! The margin scales with how trustworthy the token estimate is: a real BPE
//! tokenizer is off by at most a percent or two, while the heuristic fallback
//! under-counts dense payloads by a third, so it needs far more headroom.

use metteur_shared::config::LlmModelConfig;
use metteur_shared::llm::token::counter_for;

/// Fallback output reservation when neither the model nor the call sets one.
const DEFAULT_OUTPUT_RESERVE: u64 = 8192;

/// Share of the window held back when the token count is exact.
const EXACT_MARGIN_RATIO: f64 = 0.02;

/// Share of the window held back when the token count is a heuristic estimate.
///
/// The flat `chars/4` ratio under-counts source code and JSON by 20-50%, so a
/// prose-sized margin would let requests through that the provider then rejects.
const HEURISTIC_MARGIN_RATIO: f64 = 0.15;

/// Built-in input windows for model families whose limit is public knowledge.
///
/// Without one of these the guard would stay disabled until an operator filled
/// in `context_window_input`, which nothing in the default configuration does —
/// so a long run would overflow the real window and be rejected with
/// `prompt is too long` instead of being compacted first.
///
/// Matched by prefix, longest first, so `gpt-4o-mini` resolves before
/// `gpt-4`. An explicit `context_window_input` always wins over these.
fn default_window(model_id: &str) -> Option<u64> {
    let id = model_id.to_ascii_lowercase();
    const M: u64 = 1_000_000;
    const K: u64 = 1_000;
    if id.starts_with("gpt-4o") || id.starts_with("chatgpt") {
        return Some(128 * K);
    }
    if id.starts_with("o1") || id.starts_with("o3") || id.starts_with("o4") {
        return Some(200 * K);
    }
    if id.starts_with("gpt-4") || id.starts_with("gpt-3") {
        // 3.5 and the original 4 accepted 4k/8k; the 4-turbo generation 128k.
        if id.contains("turbo") || id.contains("gpt-4-turbo") {
            return Some(128 * K);
        }
        return Some(8 * K);
    }
    if id.starts_with("claude-opus") {
        return Some(200 * K);
    }
    if id.starts_with("claude") {
        return Some(200 * K);
    }
    if id.starts_with("deepseek") {
        return Some(64 * K);
    }
    if id.starts_with("gemini-1.5") || id.starts_with("gemini-2") {
        return Some(M);
    }
    None
}

/// Returns the input-token budget for a model, or `None` when unmetered.
///
/// A model whose window is neither configured nor recognizable stays unmetered:
/// inventing a limit for an unknown family risks compacting a conversation that
/// would have fit.
pub fn context_window_budget(
    model: Option<&LlmModelConfig>,
    requested_max_tokens: Option<u64>,
) -> Option<u64> {
    let model = model?;
    let window = model
        .context_window_input
        .or_else(|| default_window(&model.model_id))?;
    let reserve = model
        .context_window_output
        .filter(|value| *value > 0)
        .or(requested_max_tokens)
        .unwrap_or(DEFAULT_OUTPUT_RESERVE)
        // Never reserve more than half the window: a large output setting must
        // not starve the input side.
        .min(window / 2);
    let ratio = match counter_for(&model.model_id) {
        metteur_shared::llm::token::TokenCounter::Exact(_) => EXACT_MARGIN_RATIO,
        metteur_shared::llm::token::TokenCounter::Heuristic => HEURISTIC_MARGIN_RATIO,
    };
    let margin = (window as f64 * ratio) as u64;
    Some(window.saturating_sub(reserve).saturating_sub(margin).max(1))
}

/// Whether the operator explicitly set this model's input window.
///
/// Distinct from "the budget resolved" because a window supplied by
/// [`default_window`] is a documented default, not an operator assertion — the
/// UI should not label it as an assumption.
pub fn has_configured_window(model: Option<&LlmModelConfig>) -> bool {
    model.is_some_and(|m| m.context_window_input.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, window: Option<u64>) -> LlmModelConfig {
        LlmModelConfig {
            model_id: id.to_string(),
            context_window_input: window,
            ..Default::default()
        }
    }

    #[test]
    fn configured_window_wins_over_the_built_in_default() {
        let m = model("gpt-4o", Some(40_000));
        let with_default = context_window_budget(Some(&m), None).unwrap();
        assert!(with_default < 40_000);
    }

    #[test]
    fn known_family_gets_a_window_without_configuration() {
        // This is the case that used to leave the guard silently disabled.
        let m = model("gpt-4o-2024-08-06", None);
        assert!(context_window_budget(Some(&m), None).is_some());
    }

    #[test]
    fn unknown_family_stays_unmetered() {
        let m = model("some-internal-experimental", None);
        assert!(context_window_budget(Some(&m), None).is_none());
    }

    #[test]
    fn heuristic_models_keep_a_wider_margin() {
        // Same window, different estimator trust: the heuristic path must leave
        // more room so an under-estimate cannot push a request over the edge.
        let exact = model("gpt-4o", Some(200_000));
        let heuristic = model("claude-sonnet-4", Some(200_000));
        let exact_budget = context_window_budget(Some(&exact), None).unwrap();
        let heuristic_budget = context_window_budget(Some(&heuristic), None).unwrap();
        assert!(
            heuristic_budget < exact_budget,
            "heuristic {heuristic_budget} should sit below exact {exact_budget}"
        );
    }

    #[test]
    fn no_model_is_unmetered() {
        assert!(context_window_budget(None, None).is_none());
    }
}
