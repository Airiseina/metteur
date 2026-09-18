//! Token usage reported by an LLM request.

use serde::{Deserialize, Serialize};

/// Token usage for a single LLM request.
///
/// `input_tokens` is the *total* input count, including the parts served from
/// and written to the provider's prompt cache; the two cache fields break that
/// total down. Providers report these differently, so each provider adapter
/// normalizes to this single convention (see the provider modules).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the input (cache reads and writes included).
    pub input_tokens: u64,
    /// Tokens produced as output.
    pub output_tokens: u64,
    /// Reasoning tokens (only reported by some models).
    pub reasoning_tokens: u64,
    /// Total tokens consumed.
    pub total_tokens: u64,
    /// Input tokens served from the provider's prompt cache.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Input tokens written into the provider's prompt cache.
    #[serde(default)]
    pub cache_write_input_tokens: u64,
}

impl Usage {
    /// Cache-hit ratio over the input, or `None` when nothing was reportable.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if self.input_tokens == 0 {
            return None;
        }
        Some(self.cached_input_tokens as f64 / self.input_tokens as f64)
    }

    /// Input tokens billed at the regular (uncached) rate.
    pub fn uncached_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cached_input_tokens)
            .saturating_sub(self.cache_write_input_tokens)
    }

    /// Accumulates another usage into this one.
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_write_input_tokens += other.cache_write_input_tokens;
    }
}
