//! Sequential list iteration.
//!
//! The executor only validates the `List` input; the interpreter owns the
//! loop (see `Interpreter::enter_foreach`). Each iteration exposes the item
//! on `Iteration` and its position on `Index`, then drives the `Body` branch;
//! when the items are exhausted the `Completed` branch fires.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::value_input;

/// A node that iterates a list one item at a time, in order.
pub struct ForEachExecutor;

#[async_trait]
impl NodeExecutor for ForEachExecutor {
    fn kind(&self) -> &str {
        "ForEach"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        match value_input(node, inputs, "List")? {
            Value::List(_) => Ok(HashMap::new()),
            other => Err(DaemonError::Execution(format!(
                "ForEach List input must be a list, got {other:?}"
            ))),
        }
    }
}
