//! Validator node executor.
//!
//! A validator checks a runtime value against a deterministic rule and emits
//! a boolean `Passed` result. The rule is chosen by `node.data.mode` (see
//! [`ValidationMode`]); numeric equality is the default for backwards
//! compatibility.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_output, is_empty, value_to_json, value_to_string};

/// The deterministic rule applied by a validator node.
enum ValidationMode {
    /// `Actual == Expected`.
    Eq,
    /// `Actual != Expected`.
    Ne,
    /// `Actual < Expected` (numeric).
    Lt,
    /// `Actual <= Expected` (numeric).
    Lte,
    /// `Actual > Expected` (numeric).
    Gt,
    /// `Actual >= Expected` (numeric).
    Gte,
    /// `Actual` contains `Expected` (string substring or list membership).
    Contains,
    /// `Actual` (as a string) matches `node.data.regex`.
    Regex,
    /// `Actual` is non-empty.
    NotEmpty,
    /// `Actual` (as JSON) equals `node.data.expected`.
    Json,
}

/// A validator node that checks a value against an expected value.
pub struct ValidatorExecutor;

#[async_trait]
impl NodeExecutor for ValidatorExecutor {
    fn kind(&self) -> &str {
        "Validator"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let actual = get_input(inputs, node, "Actual")?;
        let mode = parse_mode(node)?;
        let passed = match mode {
            ValidationMode::Eq => {
                let expected = get_input(inputs, node, "Expected")?;
                actual == expected
            }
            ValidationMode::Ne => {
                let expected = get_input(inputs, node, "Expected")?;
                actual != expected
            }
            ValidationMode::Lt => compare_numeric(inputs, node, actual, |a, e| a < e)?,
            ValidationMode::Lte => compare_numeric(inputs, node, actual, |a, e| a <= e)?,
            ValidationMode::Gt => compare_numeric(inputs, node, actual, |a, e| a > e)?,
            ValidationMode::Gte => compare_numeric(inputs, node, actual, |a, e| a >= e)?,
            ValidationMode::Contains => contains(actual, get_input(inputs, node, "Expected")?),
            ValidationMode::Regex => regex_matches(node, &actual)?,
            ValidationMode::NotEmpty => !is_empty(&actual),
            ValidationMode::Json => {
                let expected = node.data.get("expected").unwrap_or(&serde_json::Value::Null);
                value_to_json(&actual) == *expected
            }
        };
        bool_output(node, "Passed", passed)
    }
}

/// Parses the validation mode from `node.data.mode`.
fn parse_mode(node: &Node) -> DaemonResult<ValidationMode> {
    let mode = node.data.get("mode").and_then(|v| v.as_str()).unwrap_or("eq");
    let normalized = mode.to_ascii_lowercase();
    Ok(match normalized.as_str() {
        "eq" | "equal" | "=" => ValidationMode::Eq,
        "ne" | "not_equal" | "!=" => ValidationMode::Ne,
        "lt" | "<" => ValidationMode::Lt,
        "lte" | "<=" => ValidationMode::Lte,
        "gt" | ">" => ValidationMode::Gt,
        "gte" | ">=" => ValidationMode::Gte,
        "contains" => ValidationMode::Contains,
        "regex" => ValidationMode::Regex,
        "not_empty" | "exists" => ValidationMode::NotEmpty,
        "json" => ValidationMode::Json,
        other => return Err(DaemonError::Execution(format!("unknown validation mode '{other}'"))),
    })
}

/// Applies a numeric comparison between `Actual` and the `Expected` pin.
fn compare_numeric(
    inputs: &HashMap<PinId, Value>,
    node: &Node,
    actual: Value,
    f: impl Fn(f64, f64) -> bool,
) -> DaemonResult<bool> {
    let expected = get_input(inputs, node, "Expected")?;
    let a = actual
        .as_float()
        .ok_or_else(|| DaemonError::Execution("input Actual is not numeric".to_string()))?;
    let b = expected
        .as_float()
        .ok_or_else(|| DaemonError::Execution("input Expected is not numeric".to_string()))?;
    Ok(f(a, b))
}

/// Implements `Actual contains Expected` for strings and lists.
fn contains(actual: Value, needle: Value) -> bool {
    match (actual, needle) {
        (Value::List(items), needle) => items.contains(&needle),
        (Value::String(hay), Value::String(needle)) => hay.contains(&needle),
        _ => false,
    }
}

/// Implements `Actual` matching `node.data.regex` over its string form.
fn regex_matches(node: &Node, actual: &Value) -> DaemonResult<bool> {
    let pattern = node
        .data
        .get("regex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DaemonError::Execution("regex mode requires data.regex".to_string()))?;
    let re = regex::Regex::new(pattern)
        .map_err(|e| DaemonError::Execution(format!("invalid regex: {e}")))?;
    Ok(re.is_match(&value_to_string(actual)))
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
