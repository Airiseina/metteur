//! String and type-conversion node executors.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_output, int_input, numeric_output, string_input, string_output, value_input};

/// A unary string node: `In -> Result`.
macro_rules! unary_string_node {
    ($name:ident, $kind:literal, $op:expr) => {
        /// A unary string node.
        pub struct $name;

        #[async_trait]
        impl NodeExecutor for $name {
            fn kind(&self) -> &str {
                $kind
            }

            async fn execute(
                &self,
                node: &Node,
                inputs: &HashMap<PinId, Value>,
                _ctx: &mut ExecutionContext,
            ) -> DaemonResult<HashMap<PinId, Value>> {
                let text = string_input(node, inputs, "In")?;
                string_output(node, "Result", ($op)(&text))
            }
        }
    };
}

unary_string_node!(UpperExecutor, "Upper", |s: &str| s.to_uppercase());
unary_string_node!(LowerExecutor, "Lower", |s: &str| s.to_lowercase());
unary_string_node!(TrimExecutor, "Trim", |s: &str| s.trim().to_string());

/// Counts the characters of a text input.
pub struct LengthExecutor;

#[async_trait]
impl NodeExecutor for LengthExecutor {
    fn kind(&self) -> &str {
        "Length"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let text = string_input(node, inputs, "In")?;
        numeric_output(node, "Result", text.chars().count() as f64)
    }
}

/// Concatenates two text inputs.
pub struct ConcatExecutor;

#[async_trait]
impl NodeExecutor for ConcatExecutor {
    fn kind(&self) -> &str {
        "Concat"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let a = string_input(node, inputs, "A")?;
        let b = string_input(node, inputs, "B")?;
        string_output(node, "Result", format!("{a}{b}"))
    }
}

/// Checks whether `A` contains `B`.
pub struct ContainsExecutor;

#[async_trait]
impl NodeExecutor for ContainsExecutor {
    fn kind(&self) -> &str {
        "Contains"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let text = string_input(node, inputs, "A")?;
        let needle = string_input(node, inputs, "B")?;
        bool_output(node, "Result", text.contains(&needle))
    }
}

/// Replaces every occurrence of `Find` in `Input` with `ReplaceWith`.
pub struct ReplaceExecutor;

#[async_trait]
impl NodeExecutor for ReplaceExecutor {
    fn kind(&self) -> &str {
        "Replace"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let input = string_input(node, inputs, "Input")?;
        let find = string_input(node, inputs, "Find")?;
        let replace = string_input(node, inputs, "ReplaceWith")?;
        if find.is_empty() {
            return Err(DaemonError::Execution("Replace 'Find' must not be empty".to_string()));
        }
        string_output(node, "Result", input.replace(&find, &replace))
    }
}

/// Extracts a substring: `In[start .. start+length]`.
pub struct SubstringExecutor;

#[async_trait]
impl NodeExecutor for SubstringExecutor {
    fn kind(&self) -> &str {
        "Substring"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let text = string_input(node, inputs, "In")?;
        let start = int_input(node, inputs, "Start")?.max(0) as usize;
        let length = int_input(node, inputs, "Length")?.max(0) as usize;
        let end = text.len().min(start.saturating_add(length));
        let chars: Vec<char> = text.chars().collect();
        let slice: String = chars.get(start..end.min(chars.len())).unwrap_or_default().iter().collect();
        string_output(node, "Result", slice)
    }
}

/// A conversion node: `In -> Result`.
macro_rules! convert_node {
    ($name:ident, $kind:literal, $convert:expr) => {
        /// A type-conversion node.
        pub struct $name;

        #[async_trait]
        impl NodeExecutor for $name {
            fn kind(&self) -> &str {
                $kind
            }

            async fn execute(
                &self,
                node: &Node,
                inputs: &HashMap<PinId, Value>,
                _ctx: &mut ExecutionContext,
            ) -> DaemonResult<HashMap<PinId, Value>> {
                let value = value_input(node, inputs, "In")?;
                let converted = ($convert)(value).ok_or_else(|| {
                    DaemonError::Execution(format!("cannot convert value to {}", $kind))
                })?;
                let pin = node
                    .pins
                    .iter()
                    .find(|p| p.name == "Result")
                    .ok_or_else(|| DaemonError::Execution("missing pin Result".to_string()))?;
                Ok(HashMap::from([(pin.id, converted)]))
            }
        }
    };
}

convert_node!(ToStringExecutor, "ToString", |v: &Value| Some(super::value_to_string(v).into()));
convert_node!(ToJsonExecutor, "ToJson", |v: &Value| {
    Some(Value::Json(super::value_to_json(v)))
});
convert_node!(ToIntExecutor, "ToInt", |v: &Value| match v {
    Value::Int(i) => Some(Value::Int(*i)),
    Value::Float(f) => Some(Value::Int(*f as i64)),
    Value::Json(j) => j.as_i64().map(Value::Int),
    Value::String(s) => s.trim().parse::<i64>().ok().map(Value::Int),
    _ => None,
});
convert_node!(ToFloatExecutor, "ToFloat", |v: &Value| match v {
    Value::Int(i) => Some(Value::Float(*i as f64)),
    Value::Float(f) => Some(Value::Float(*f)),
    Value::Json(j) => j.as_f64().map(Value::Float),
    Value::String(s) => s.trim().parse::<f64>().ok().map(Value::Float),
    _ => None,
});
convert_node!(ToBoolExecutor, "ToBool", |v: &Value| match v {
    Value::Bool(b) => Some(Value::Bool(*b)),
    Value::Int(i) => Some(Value::Bool(*i != 0)),
    Value::Json(j) => j.as_bool().map(Value::Bool),
    Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(Value::Bool(true)),
        "false" | "0" | "no" => Some(Value::Bool(false)),
        _ => None,
    },
    _ => None,
});

/// Parses a JSON document from text: `In -> Result(json)`.
pub struct ParseJsonExecutor;

#[async_trait]
impl NodeExecutor for ParseJsonExecutor {
    fn kind(&self) -> &str {
        "ParseJson"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let text = string_input(node, inputs, "In")?;
        let json = serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| DaemonError::Execution(format!("invalid JSON: {e}")))?;
        let pin = node
            .pins
            .iter()
            .find(|p| p.name == "Result")
            .ok_or_else(|| DaemonError::Execution("missing pin Result".to_string()))?;
        Ok(HashMap::from([(pin.id, Value::Json(json))]))
    }
}