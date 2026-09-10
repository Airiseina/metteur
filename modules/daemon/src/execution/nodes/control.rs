//! Control-flow node executors.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_output, value_input};

/// A control-flow node that selects an output branch based on a condition.
///
/// The `Condition` pin accepts a boolean, a non-empty string, or a non-zero
/// number, and the result is the corresponding truth value.
pub struct BranchExecutor;

#[async_trait]
impl NodeExecutor for BranchExecutor {
    fn kind(&self) -> &str {
        "Branch"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let condition = read_condition(inputs, node)?;
        bool_output(node, "Result", condition)
    }
}

/// A control-flow node that routes to a named case branch.
///
/// The `Case` input is echoed to the `Result` data output; the interpreter
/// selects the `ExecOutput` pin named `Case_<value>` (falling back to
/// `Default`) from that value.
pub struct SwitchExecutor;

#[async_trait]
impl NodeExecutor for SwitchExecutor {
    fn kind(&self) -> &str {
        "Switch"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let case = value_input(node, inputs, "Case")?.clone();
        let pin =
            node.pins.iter().find(|p| p.name == "Result").ok_or_else(|| {
                DaemonError::Execution("switch node missing Result pin".to_string())
            })?;
        Ok(HashMap::from([(pin.id, case)]))
    }
}

/// Reads the `Condition` input as a truthy boolean.
fn read_condition(inputs: &HashMap<PinId, Value>, node: &Node) -> DaemonResult<bool> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == "Condition")
        .ok_or_else(|| DaemonError::Execution("missing pin Condition".to_string()))?;
    let value = inputs
        .get(&pin.id)
        .ok_or_else(|| DaemonError::Execution("missing input Condition".to_string()))?;
    match value {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => Ok(!s.is_empty()),
        Value::Int(i) => Ok(*i != 0),
        Value::Float(f) => Ok(*f != 0.0),
        Value::Null => Ok(false),
        other => Err(DaemonError::Execution(format!(
            "Condition is not boolean, string, or numeric: {other:?}"
        ))),
    }
}
