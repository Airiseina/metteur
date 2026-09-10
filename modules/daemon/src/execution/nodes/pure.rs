//! Pure-function scalar node executors: math, comparison and logic.

use std::cmp::Ordering;
use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{bool_input, bool_output, numeric_input, numeric_output};

/// A binary float node: `A <op> B -> Result`.
macro_rules! binary_float_node {
    ($name:ident, $kind:literal, $op:expr) => {
        /// A binary numeric node.
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
                numeric_output(node, "Result", ($op)(a, b))
            }
        }
    };
}

binary_float_node!(ModuloExecutor, "Modulo", |a: f64, b: f64| a % b);
binary_float_node!(PowerExecutor, "Power", |a: f64, b: f64| a.powf(b));
binary_float_node!(MinExecutor, "Min", |a: f64, b: f64| a.min(b));
binary_float_node!(MaxExecutor, "Max", |a: f64, b: f64| a.max(b));

/// A unary float node: `A -> Result`.
macro_rules! unary_float_node {
    ($name:ident, $kind:literal, $op:expr) => {
        /// A unary numeric node.
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
                numeric_output(node, "Result", ($op)(a))
            }
        }
    };
}

unary_float_node!(AbsExecutor, "Abs", |a: f64| a.abs());
unary_float_node!(RoundExecutor, "Round", |a: f64| a.round());

/// Compares two values by a common numeric axis, lexicographically for
/// strings and by equality for booleans; `None` when incomparable.
fn values_cmp(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => x.partial_cmp(y),
        _ => {
            let x = a.as_float()?;
            let y = b.as_float()?;
            x.partial_cmp(&y)
        }
    }
}

/// A comparison node: `A <pred> B -> Result(bool)`.
macro_rules! compare_node {
    ($name:ident, $kind:literal, $pred:expr) => {
        /// A comparison node.
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
                use super::value_input;
                let a = value_input(node, inputs, "A")?;
                let b = value_input(node, inputs, "B")?;
                bool_output(node, "Result", ($pred)(a, b))
            }
        }
    };
}

compare_node!(EqualExecutor, "Equal", |a: &Value, b: &Value| a == b);
compare_node!(NotEqualExecutor, "NotEqual", |a: &Value, b: &Value| a != b);
compare_node!(GreaterExecutor, "Greater", |a: &Value, b: &Value| {
    matches!(values_cmp(a, b), Some(Ordering::Greater))
});
compare_node!(LessExecutor, "Less", |a: &Value, b: &Value| {
    matches!(values_cmp(a, b), Some(Ordering::Less))
});
compare_node!(GreaterEqualExecutor, "GreaterEqual", |a: &Value, b: &Value| {
    matches!(values_cmp(a, b), Some(o) if o != Ordering::Less)
});
compare_node!(LessEqualExecutor, "LessEqual", |a: &Value, b: &Value| {
    matches!(values_cmp(a, b), Some(o) if o != Ordering::Greater)
});

/// A boolean binary node: `A && B / A || B -> Result`.
macro_rules! bool_binary_node {
    ($name:ident, $kind:literal, $op:expr) => {
        /// A boolean logic node.
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
                let a = bool_input(node, inputs, "A")?;
                let b = bool_input(node, inputs, "B")?;
                bool_output(node, "Result", ($op)(a, b))
            }
        }
    };
}

bool_binary_node!(AndExecutor, "And", |a: bool, b: bool| a && b);
bool_binary_node!(OrExecutor, "Or", |a: bool, b: bool| a || b);
bool_binary_node!(XorExecutor, "Xor", |a: bool, b: bool| a != b);

/// A boolean unary node: `!A -> Result`.
pub struct NotExecutor;

#[async_trait]
impl NodeExecutor for NotExecutor {
    fn kind(&self) -> &str {
        "Not"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let a = bool_input(node, inputs, "A")?;
        bool_output(node, "Result", !a)
    }
}

#[cfg(test)]
mod tests {
    use crate::execution::nodes::tests::{exec_bools, exec_floats};
    use metteur_shared::Value;

    #[tokio::test]
    async fn binary_math_produces_float_result() {
        let outputs = exec_floats("Modulo", &[("A", 9.0), ("B", 4.0)]).await.unwrap();
        assert!(matches!(outputs.get("Result"), Some(Value::Float(v)) if *v == 1.0));
    }

    #[tokio::test]
    async fn min_max_and_abs() {
        let m = exec_floats("Min", &[("A", 3.0), ("B", -2.0)]).await.unwrap();
        assert!(matches!(m.get("Result"), Some(Value::Float(v)) if *v == -2.0));
        let a = exec_floats("Abs", &[("A", -7.0)]).await.unwrap();
        assert!(matches!(a.get("Result"), Some(Value::Float(v)) if *v == 7.0));
    }

    #[tokio::test]
    async fn comparisons_and_logic() {
        let g = exec_floats("Greater", &[("A", 5.0), ("B", 3.0)]).await.unwrap();
        assert!(matches!(g.get("Result"), Some(Value::Bool(true))));
        let le = exec_floats("LessEqual", &[("A", 3.0), ("B", 3.0)]).await.unwrap();
        assert!(matches!(le.get("Result"), Some(Value::Bool(true))));
    }

    #[tokio::test]
    async fn logic_nodes() {
        let not = exec_bools("Not", &[("A", true)]).await.unwrap();
        assert!(matches!(not.get("Result"), Some(Value::Bool(false))));
        let and = exec_bools("And", &[("A", true), ("B", false)]).await.unwrap();
        assert!(matches!(and.get("Result"), Some(Value::Bool(false))));
        let or = exec_bools("Or", &[("A", false), ("B", true)]).await.unwrap();
        assert!(matches!(or.get("Result"), Some(Value::Bool(true))));
    }
}
