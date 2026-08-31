//! Token usage reported by an LLM request.

use serde::{Deserialize, Serialize};

/// Token usage for a single LLM request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the input.
    pub input_tokens: u64,
    /// Tokens produced as output.
    pub output_tokens: u64,
    /// Reasoning tokens (only reported by some models).
    pub reasoning_tokens: u64,
    /// Total tokens consumed.
    pub total_tokens: u64,
}
