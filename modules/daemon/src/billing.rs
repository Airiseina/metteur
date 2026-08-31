//! Model pricing and run usage aggregation.
//!
//! Billing only computes costs from the per-model price table. Peak/off-peak
//! pricing rules live per model in `llm.models` and are evaluated by the LLM
//! layer, not here.

use std::collections::HashMap;

use metteur_shared::config::{BillingConfig, LlmModelConfig, ModelPricing};

use crate::error::DaemonResult;

/// The computed cost of a single LLM call.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    /// Cost in millionths of the configured currency.
    pub micros: u64,
    /// Currency code from the configuration.
    pub currency: String,
}

/// Per-model aggregated usage for one execution run.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUsageSummary {
    pub model: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost_micros: u64,
}

/// Aggregated usage and cost for one execution run.
#[derive(Debug, Clone, Default)]
pub struct UsageSummaryData {
    pub currency: String,
    pub total_cost_micros: u64,
    pub models: Vec<ModelUsageSummary>,
}

/// Computes the cost of one LLM call.
///
/// `pricing` is the model's effective price table (see [`effective_pricing`]);
/// `None` means the model has no configured pricing. Reasoning tokens are
/// billed as output tokens.
pub fn cost(pricing: Option<&ModelPricing>, currency: &str, usage: &metteur_shared::Usage) -> Option<Cost> {
    let p = pricing?;
    let input = p.input_per_mtok * usage.input_tokens as f64 / 1_000_000.0;
    let output = p.output_per_mtok * (usage.output_tokens + usage.reasoning_tokens) as f64
        / 1_000_000.0;
    let total = input + output;

    Some(Cost {
        micros: (total * 1_000_000.0).round() as u64,
        currency: if currency.is_empty() {
            "USD".to_string()
        } else {
            currency.to_string()
        },
    })
}

/// Best-effort price table for a model, honouring its pricing `kind`.
///
/// `default` → flat `prices`; `tiered` → the first tier's prices; `peak` →
/// the window-external `default_prices`. Time-aware evaluation of peak windows
/// is left to the LLM layer.
pub fn effective_pricing(model: &LlmModelConfig) -> Option<ModelPricing> {
    match model.pricing.kind.as_str() {
        "tiered" => model.pricing.tiers.first().map(|t| t.prices.clone()),
        "peak" => model.pricing.default_prices.clone(),
        _ => model.pricing.prices.clone().or_else(|| model.pricing.default_prices.clone()),
    }
}

/// Aggregates the `llm.usage` audit entries of one run.
///
/// Models without pricing contribute token counts but zero cost.
pub fn run_usage(
    db: &crate::persistence::Db,
    config: &BillingConfig,
    run_id: &str,
) -> DaemonResult<UsageSummaryData> {
    let writer = crate::audit::AuditWriter::new(db.clone());
    let mut by_model: HashMap<String, ModelUsageSummary> = HashMap::new();
    let mut total = 0u64;

    for entry in writer.list()? {
        if entry.operation != "llm.usage" {
            continue;
        }
        if entry.detail.get("run_id").and_then(|v| v.as_str()) != Some(run_id) {
            continue;
        }
        let get = |key: &str| entry.detail.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        let model =
            entry.detail.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        let slot = by_model.entry(model.clone()).or_insert_with(|| ModelUsageSummary {
            model,
            calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cost_micros: 0,
        });
        slot.calls += 1;
        slot.input_tokens += get("input_tokens");
        slot.output_tokens += get("output_tokens");
        slot.reasoning_tokens += get("reasoning_tokens");
        let cost_micros = get("cost_micros");
        slot.cost_micros += cost_micros;
        total += cost_micros;
    }

    let mut models: Vec<ModelUsageSummary> = by_model.into_values().collect();
    models.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.model.cmp(&b.model)));
    Ok(UsageSummaryData {
        currency: if config.currency.is_empty() {
            "USD".to_string()
        } else {
            config.currency.clone()
        },
        total_cost_micros: total,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_shared::config::ModelPricing;
    use metteur_shared::Usage;

    fn pricing(input: f64, output: f64) -> ModelPricing {
        ModelPricing {
            input_per_mtok: input,
            output_per_mtok: output,
            ..Default::default()
        }
    }

    #[test]
    fn computes_peak_cost() {
        let p = pricing(1.0, 10.0);
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            reasoning_tokens: 0,
            total_tokens: 1_100_000,
        };
        let cost = cost(Some(&p), "USD", &usage).unwrap();
        assert_eq!(cost.micros, 2_000_000); // $1 input + $1 output
        assert_eq!(cost.currency, "USD");
    }

    #[test]
    fn unpriced_models_have_no_cost() {
        assert!(cost(None, "USD", &Usage::default()).is_none());
    }
}
