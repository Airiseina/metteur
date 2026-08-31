//! Judge node executor.
//!
//! A judge decides whether a task succeeded. By default it keeps the legacy
//! behavior: the `Success` flag is true when the `Score` input reaches
//! `node.data.threshold` (default `1.0`). Alternatively, when
//! `node.data.success_criteria` is set, the `Result` input is checked against a
//! small deterministic rule object, which takes precedence over the score.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_output, is_empty, numeric_input, value_to_json, value_to_string};

/// A judge node that marks a task as successful.
pub struct JudgeExecutor;

#[async_trait]
impl NodeExecutor for JudgeExecutor {
    fn kind(&self) -> &str {
        "Judge"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let success = if node.data.get("success_criteria").is_some() {
            let result = get_input(inputs, node, "Result")?;
            matches_success_criteria(node, &result)?
        } else {
            let score = numeric_input(inputs, node, "Score")?;
            let threshold = node
                .data
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            score >= threshold
        };
        bool_output(node, "Success", success)
    }
}

/// Judges whether `result` meets `node.data.success_criteria`.
///
/// Supported criteria, chosen by `success_criteria.type`:
/// - `non_empty`: the result carries content.
/// - `regex`: the result's string form matches `success_criteria.pattern`.
/// - `equals`: the result's JSON form equals `success_criteria.expected`.
fn matches_success_criteria(node: &Node, result: &Value) -> DaemonResult<bool> {
    let criteria = node
        .data
        .get("success_criteria")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DaemonError::Execution("success_criteria must be a JSON object".to_string()))?;
    let kind = criteria
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DaemonError::Execution("success_criteria.type is required".to_string()))?;

    match kind {
        "non_empty" | "not_empty" => Ok(!is_empty(result)),
        "regex" => {
            let pattern = criteria
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    DaemonError::Execution(
                        "success_criteria regex requires .pattern".to_string(),
                    )
                })?;
            let re = regex::Regex::new(pattern)
                .map_err(|e| DaemonError::Execution(format!("invalid regex: {e}")))?;
            Ok(re.is_match(&value_to_string(result)))
        }
        "equals" => {
            let expected = criteria
                .get("expected")
                .ok_or_else(|| DaemonError::Execution("success_criteria equals requires .expected".to_string()))?;
            Ok(value_to_json(result) == *expected)
        }
        other => Err(DaemonError::Execution(format!(
            "unknown success criteria type '{other}'"
        ))),
    }
}

/// Looks up an input value by pin name.
fn get_input(inputs: &HashMap<PinId, Value>, node: &Node, name: &str) -> DaemonResult<Value> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| DaemonError::Execution(format!("missing pin {name}")))?;
    inputs
        .get(&pin.id)
        .cloned()
        .ok_or_else(|| DaemonError::Execution(format!("missing input {name}")))
}