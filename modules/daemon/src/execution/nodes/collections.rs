//! Collection node executors: list and JSON object operations.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_output, int_input, list_input, numeric_output, string_input, value_input};

/// Creates a list from up to three item inputs; unconnected items are skipped.
pub struct ListCreateExecutor;

#[async_trait]
impl NodeExecutor for ListCreateExecutor {
    fn kind(&self) -> &str {
        "ListCreate"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut items = Vec::new();
        for name in ["ItemA", "ItemB", "ItemC"] {
            if let Ok(value) = value_input(node, inputs, name) {
                items.push(value.clone());
            }
        }
        let pin = node.pins.iter().find(|p| p.name == "Result").unwrap();
        Ok(HashMap::from([(pin.id, Value::List(items))]))
    }
}

/// Appends an item to a list: `List + Item -> Result(list)`.
pub struct ListAppendExecutor;

#[async_trait]
impl NodeExecutor for ListAppendExecutor {
    fn kind(&self) -> &str {
        "ListAppend"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut items = list_input(node, inputs, "List")?;
        items.push(value_input(node, inputs, "Item")?.clone());
        let pin = node.pins.iter().find(|p| p.name == "Result").unwrap();
        Ok(HashMap::from([(pin.id, Value::List(items))]))
    }
}

/// Reads an element at a numeric index: `List + Index -> Result(any)`.
pub struct ListGetExecutor;

#[async_trait]
impl NodeExecutor for ListGetExecutor {
    fn kind(&self) -> &str {
        "ListGet"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let items = list_input(node, inputs, "List")?;
        let index = int_input(node, inputs, "Index")?;
        let item = items.get(index.max(0) as usize).cloned().unwrap_or(Value::Null);
        let pin = node.pins.iter().find(|p| p.name == "Result").unwrap();
        Ok(HashMap::from([(pin.id, item)]))
    }
}

/// Returns the element count: `List -> Result(int)`.
pub struct ListLengthExecutor;

#[async_trait]
impl NodeExecutor for ListLengthExecutor {
    fn kind(&self) -> &str {
        "ListLength"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let items = list_input(node, inputs, "List")?;
        numeric_output(node, "Result", items.len() as f64)
    }
}

/// Checks whether `List` contains `Item`: `-> Result(bool)`.
pub struct ListContainsExecutor;

#[async_trait]
impl NodeExecutor for ListContainsExecutor {
    fn kind(&self) -> &str {
        "ListContains"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let items = list_input(node, inputs, "List")?;
        let item = value_input(node, inputs, "Item")?;
        bool_output(node, "Result", items.contains(item))
    }
}

/// Resolves a dot/bracket path (`a.b[0]`) inside a JSON value.
fn json_at<'a>(json: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = json;
    for key in path.split('.') {
        // Bracket segments: `name[0]` -> name + 0.
        let (name, index) = match key.split_once('[') {
            Some((name, rest)) => {
                let idx = rest.trim_end_matches(']').parse::<usize>().ok();
                (name, idx)
            }
            None => (key, None),
        };
        if !name.is_empty() {
            current = current.get(name)?;
        }
        if let Some(idx) = index {
            current = current.get(idx)?;
        }
    }
    Some(current)
}

/// Reads a value at a JSON path: `Object + Path -> Result(any)`.
pub struct JsonGetExecutor;

#[async_trait]
impl NodeExecutor for JsonGetExecutor {
    fn kind(&self) -> &str {
        "JsonGet"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let path = string_input(node, inputs, "Path")?;
        let object = value_input(node, inputs, "Object")?;
        let parsed: Option<serde_json::Value> = match object {
            Value::Json(j) => Some(j.clone()),
            Value::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        };
        let Some(json) = parsed.as_ref() else {
            return Err(DaemonError::Execution("JsonGet 'Object' is not JSON".to_string()));
        };
        let found = json_at(json, &path).map(super::json_to_value);
        let pin = node.pins.iter().find(|p| p.name == "Result").unwrap();
        Ok(HashMap::from([(pin.id, found.unwrap_or(Value::Null))]))
    }
}

/// Sets `value` at the last segment, creating intermediate objects.
fn json_set_path(
    current: &mut serde_json::Value,
    segments: &[&str],
    value: serde_json::Value,
) -> bool {
    let Some((&key, rest)) = segments.split_first() else {
        return false;
    };
    if rest.is_empty() {
        match current {
            serde_json::Value::Object(map) => {
                map.insert(key.to_string(), value);
                true
            }
            _ => false,
        }
    } else {
        let child: &mut serde_json::Value = match current {
            serde_json::Value::Object(map) => {
                map.entry(key.to_string()).or_insert_with(|| serde_json::json!({}))
            }
            _ => return false,
        };
        json_set_path(child, rest, value)
    }
}

/// Writes a value at a JSON path, returning the modified object.
///
/// `Path` addresses an object field path (`a.b`); intermediate objects are
/// created when missing. `Value` accepts any input and is stored as JSON.
pub struct JsonSetExecutor;

#[async_trait]
impl NodeExecutor for JsonSetExecutor {
    fn kind(&self) -> &str {
        "JsonSet"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let path = string_input(node, inputs, "Path")?;
        let object = value_input(node, inputs, "Object")?;
        let value = super::value_to_json(value_input(node, inputs, "Value")?);
        let mut root = match object {
            Value::Json(j) => j.clone(),
            other => super::value_to_json(other),
        };
        let segments: Vec<&str> = path.split('.').collect();
        if !json_set_path(&mut root, &segments, value) {
            return Err(DaemonError::Execution(
                "JsonSet path traverses a non-object".to_string(),
            ));
        }
        let pin = node.pins.iter().find(|p| p.name == "Result").unwrap();
        Ok(HashMap::from([(pin.id, Value::Json(root))]))
    }
}