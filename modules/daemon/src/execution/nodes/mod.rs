//! Built-in node executors.

pub mod abstract_node;
pub mod arithmetic;
pub mod call_llm;
pub mod control;
pub mod function;
pub mod judge;
pub mod tool;
pub mod validator;

pub use abstract_node::AbstractExecutor;
pub use arithmetic::{AddExecutor, DivideExecutor, MultiplyExecutor, SubtractExecutor};
pub use call_llm::CallLlmExecutor;
pub use control::BranchExecutor;
pub use function::{CallFunctionExecutor, FunctionEntryExecutor, FunctionExitExecutor};
pub use judge::JudgeExecutor;
pub use tool::ToolExecutor;
pub use validator::ValidatorExecutor;

/// A terminal event node. It produces no outputs; execution ends when no exec
/// edge leaves it.
pub struct EndExecutor;

#[async_trait::async_trait]
impl crate::registry::NodeExecutor for EndExecutor {
    fn kind(&self) -> &str {
        "End"
    }

    async fn execute(
        &self,
        _node: &Node,
        _inputs: &HashMap<PinId, Value>,
        _ctx: &mut crate::execution::context::ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        Ok(HashMap::new())
    }
}

use std::collections::HashMap;

use metteur_shared::{Node, PinId, PinType, Value};

use crate::error::{DaemonError, DaemonResult};

/// The Start event node. Produces data output values from its `data` object.
pub struct StartExecutor;

#[async_trait::async_trait]
impl crate::registry::NodeExecutor for StartExecutor {
    fn kind(&self) -> &str {
        "Start"
    }

    async fn execute(
        &self,
        node: &Node,
        _inputs: &HashMap<PinId, Value>,
        _ctx: &mut crate::execution::context::ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut outputs = HashMap::new();
        if let serde_json::Value::Object(map) = &node.data {
            for pin in &node.pins {
                if pin.pin_type != PinType::DataOutput {
                    continue;
                }
                if let Some(value) = map.get(&pin.name) {
                    outputs.insert(pin.id, json_to_value(value));
                }
            }
        }
        Ok(outputs)
    }
}

/// Converts a JSON value into a shared [`Value`].
pub(crate) fn json_to_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
        serde_json::Value::Object(_) => Value::Json(json.clone()),
    }
}

/// Reads a numeric input by pin name.
pub(crate) fn numeric_input(
    inputs: &HashMap<PinId, Value>,
    node: &Node,
    name: &str,
) -> DaemonResult<f64> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| DaemonError::Execution(format!("missing pin {name}")))?;
    let value = inputs
        .get(&pin.id)
        .ok_or_else(|| DaemonError::Execution(format!("missing input {name}")))?;
    value.as_float().ok_or_else(|| DaemonError::Execution(format!("input {name} is not numeric")))
}

/// Writes a numeric value to the named output pin.
pub(crate) fn numeric_output(
    node: &Node,
    name: &str,
    value: f64,
) -> DaemonResult<HashMap<PinId, Value>> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| DaemonError::Execution(format!("missing pin {name}")))?;
    Ok(HashMap::from([(pin.id, Value::Float(value))]))
}

/// Writes a boolean value to the named output pin.
pub(crate) fn bool_output(
    node: &Node,
    name: &str,
    value: bool,
) -> DaemonResult<HashMap<PinId, Value>> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| DaemonError::Execution(format!("missing pin {name}")))?;
    Ok(HashMap::from([(pin.id, Value::Bool(value))]))
}

/// Writes a string value to the named output pin.
pub(crate) fn string_output(
    node: &Node,
    name: &str,
    value: impl Into<String>,
) -> DaemonResult<HashMap<PinId, Value>> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| DaemonError::Execution(format!("missing pin {name}")))?;
    Ok(HashMap::from([(pin.id, Value::String(value.into()))]))
}

/// Converts a value into a textual representation.
pub(crate) fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Json(j) => j.to_string(),
        other => format!("{other:?}"),
    }
}

/// Converts a value into its JSON representation.
pub(crate) fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null | Value::Context(_) => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::List(l) => serde_json::Value::Array(l.iter().map(value_to_json).collect()),
        Value::Json(j) => j.clone(),
    }
}

/// Returns true when a value carries no meaningful content.
pub(crate) fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null | Value::Context(_) => true,
        Value::Bool(b) => !*b,
        Value::Int(i) => *i == 0,
        Value::Float(f) => *f == 0.0,
        Value::String(s) => s.is_empty(),
        Value::List(l) => l.is_empty(),
        Value::Json(j) => match j {
            serde_json::Value::Null => true,
            serde_json::Value::Bool(b) => !*b,
            serde_json::Value::Number(n) => n.as_f64().map(|f| f == 0.0).unwrap_or(false),
            serde_json::Value::String(s) => s.is_empty(),
            serde_json::Value::Array(a) => a.is_empty(),
            serde_json::Value::Object(o) => o.is_empty(),
        },
    }
}
