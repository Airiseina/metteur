//! Arithmetic node executors.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{numeric_input, numeric_output};

macro_rules! binary_arith {
    ($name:ident, $kind:literal, $op:tt) => {
        /// A binary arithmetic node.
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
                let a = numeric_input(inputs, node, "A")?;
                let b = numeric_input(inputs, node, "B")?;
                numeric_output(node, "Result", a $op b)
            }
        }
    };
}

binary_arith!(AddExecutor, "Add", +);
binary_arith!(SubtractExecutor, "Subtract", -);
binary_arith!(MultiplyExecutor, "Multiply", *);
binary_arith!(DivideExecutor, "Divide", /);
