//! Generation parameters for LLM requests.

use serde::{Deserialize, Serialize};

/// The reasoning effort for reasoning-capable models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    /// No reasoning.
    None,
    /// Minimal reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Detailed step-by-step reasoning.
    High,
}

/// Sampling and generation parameters for a single LLM request.
///
/// All fields are optional; providers apply their own defaults when a field
/// is absent. Providers map these onto their vendor-specific parameter names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationParams {
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling probability mass.
    pub top_p: Option<f64>,
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sequences at which generation stops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Reasoning effort for reasoning models.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// A seed for reproducible sampling.
    pub seed: Option<i64>,
    /// Penalizes tokens based on their frequency in the text.
    pub presence_penalty: Option<f64>,
    /// Penalizes tokens based on their presence in the text.
    pub frequency_penalty: Option<f64>,
}
