//! Frame-scoped variable nodes.
//!
//! `VariableSet` writes into the innermost frame; `VariableGet` resolves
//! outwards through enclosing frames. Popping a function frame discards its
//! locals; root-frame variables live for the whole run.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{string_input, value_input};

/// A node that stores a value in the innermost frame under `Name`.
pub struct VariableSetExecutor;

#[async_trait]
impl NodeExecutor for VariableSetExecutor {
    fn kind(&self) -> &str {
        "VariableSet"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let name = string_input(node, inputs, "Name")?;
        let value = value_input(node, inputs, "Value")?.clone();
        ctx.variables_last_mut()
            .ok_or_else(|| DaemonError::Execution("no active frame".to_string()))?
            .insert(name, value.clone());
        let pin = node
            .pins
            .iter()
            .find(|p| p.name == "Value" && p.pin_type == metteur_shared::PinType::DataOutput)
            .ok_or_else(|| {
                DaemonError::Execution("VariableSet node missing Value output".to_string())
            })?;
        Ok(HashMap::from([(pin.id, value)]))
    }
}

/// A node that reads a frame-scoped variable by `Name`.
pub struct VariableGetExecutor;

#[async_trait]
impl NodeExecutor for VariableGetExecutor {
    fn kind(&self) -> &str {
        "VariableGet"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let name = string_input(node, inputs, "Name")?;
        let value = ctx
            .variables_frames()
            .rev()
            .find_map(|frame| frame.get(&name))
            .cloned()
            .ok_or_else(|| DaemonError::Execution(format!("undefined variable '{name}'")))?;
        let pin = node
            .pins
            .iter()
            .find(|p| p.name == "Value" && p.pin_type == metteur_shared::PinType::DataOutput)
            .ok_or_else(|| {
                DaemonError::Execution("VariableGet node missing Value output".to_string())
            })?;
        Ok(HashMap::from([(pin.id, value)]))
    }
}
