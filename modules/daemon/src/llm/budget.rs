//! Context-window budgeting.
//!
//! The window guard keeps a request from overflowing the model's input limit.
//! It combines the configured window (`llm.models[..].context_window_input`)
//! with the reserved output budget and a safety margin, because the token
//! estimate driving it is approximate.

use metteur_shared::config::LlmModelConfig;

/// Fallback output reservation when neither the model nor the call sets one.
const DEFAULT_OUTPUT_RESERVE: u64 = 8192;

/// Share of the window held back to absorb estimation error.
const SAFETY_MARGIN_RATIO: f64 = 0.05;

/// Returns the input-token budget for a model, or `None` when unmetered.
///
/// Without a configured input window the guard stays disabled: inventing a
/// limit would break models whose window the configuration does not describe.
pub fn context_window_budget(
    model: Option<&LlmModelConfig>,
    requested_max_tokens: Option<u64>,
) -> Option<u64> {
    let model = model?;
    let window = model.context_window_input?;
    let reserve = model
        .context_window_output
        .filter(|value| *value > 0)
        .or(requested_max_tokens)
        .unwrap_or(DEFAULT_OUTPUT_RESERVE)
        // Never reserve more than half the window: a large output setting must
        // not starve the input side.
        .min(window / 2);
    let margin = (window as f64 * SAFETY_MARGIN_RATIO) as u64;
    Some(window.saturating_sub(reserve).saturating_sub(margin).max(1))
}
