//! Property-based tests running the blueprint interpreter on randomly
//! generated acyclic graphs.
//!
//! The invariant under test: for any well-formed acyclic blueprint of the
//! supported kinds, execution terminates without panicking — either the run
//! completes with events, or it fails gracefully with a daemon error.

use metteur_daemon::execution::{ExecutionEvent, Interpreter, SharedBlueprint};
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::Registry;
use metteur_shared::model::blueprint::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};
use proptest::collection::vec as vec_strategy;
use proptest::prelude::*;
use proptest::sample::select;
use std::sync::Arc;
use uuid::Uuid;

/// Pin layout for the kinds the generator uses (subset of the registry).
fn pins_of(kind: &str) -> Vec<(String, PinType)> {
    let mut pins: Vec<(String, PinType)> = Vec::new();
    match kind {
        "Start" => pins.push(("Exec".into(), PinType::ExecOutput)),
        "End" => {
            pins.push(("Exec".into(), PinType::ExecInput));
            return pins;
        }
        _ => {
            pins.push(("Exec".into(), PinType::ExecInput));
            pins.push(("Exec".into(), PinType::ExecOutput));
        }
    }
    match kind {
        "Start" => {
            pins.push(("A".into(), PinType::DataOutput));
            pins.push(("B".into(), PinType::DataOutput));
        }
        "Add" | "Subtract" | "Multiply" | "Divide" => {
            pins.push(("A".into(), PinType::DataInput));
            pins.push(("B".into(), PinType::DataInput));
        }
        "Validator" => {
            pins.push(("Actual".into(), PinType::DataInput));
            pins.push(("Expected".into(), PinType::DataInput));
        }
        "Judge" => pins.push(("Score".into(), PinType::DataInput)),
        _ => {}
    }
    pins
}

/// Generates a deterministic acyclic chain: node 0 is Start, exec edges
/// connect node `i` to `i+1`, and data input pins get random constants.
fn any_blueprint() -> impl Strategy<Value = Blueprint> {
    let kinds = vec_strategy(
        select(&["Add", "Subtract", "Multiply", "Divide", "Validator", "Judge", "End"]),
        1..=5,
    );
    let constants = vec_strategy((any::<usize>(), any::<String>(), any::<f64>()), 0..24);
    (kinds, constants).prop_map(|(mut kinds, constants)| {
        kinds.insert(0, "Start");
        let mut nodes = Vec::new();
        let mut entry_node_id = Uuid::nil();
        for (i, k) in kinds.iter().enumerate() {
            let node_id = Uuid::new_v4();
            if i == 0 {
                entry_node_id = node_id;
            }
            let pins: Vec<Pin> = pins_of(k)
                .into_iter()
                .map(|(name, pin_type)| Pin {
                    id: Uuid::new_v4(),
                    name,
                    pin_type,
                    data_type: DataType::Json,
                    ..Default::default()
                })
                .collect();
            nodes.push(Node {
                id: node_id,
                node_type: NodeType::Function,
                kind: (*k).to_string(),
                position: (0.0, 0.0),
                pins,
                data: serde_json::json!({}),
            });
        }
        // Exec chain edges node i -> i+1 (first exec output -> first exec input).
        let mut edges = Vec::new();
        for i in 0..nodes.len() - 1 {
            let src_pin =
                nodes[i].pins.iter().find(|p| p.pin_type == PinType::ExecOutput).map(|p| p.id);
            let dst_pin =
                nodes[i + 1].pins.iter().find(|p| p.pin_type == PinType::ExecInput).map(|p| p.id);
            if let (Some(source_pin), Some(target_pin)) = (src_pin, dst_pin) {
                edges.push(Edge {
                    id: Uuid::new_v4(),
                    source_node: nodes[i].id,
                    source_pin,
                    target_node: nodes[i + 1].id,
                    target_pin,
                });
            }
        }
        // Random constants onto matching data input pins.
        for (idx, name, value) in constants {
            let node_idx = idx % nodes.len();
            let is_input = nodes[node_idx]
                .pins
                .iter()
                .any(|p| p.name == name && p.pin_type == PinType::DataInput);
            if is_input {
                nodes[node_idx]
                    .data
                    .as_object_mut()
                    .expect("data is a json object")
                    .insert(name, serde_json::json!(value));
            }
        }
        Blueprint {
            id: Uuid::new_v4(),
            name: "fuzz".to_string(),
            nodes,
            edges,
            entry_node_id,
        }
    })
}

#[test]
fn interpreter_terminates_on_random_blueprints() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let strategy = any_blueprint();

    proptest::test_runner::TestRunner::new(proptest::test_runner::Config {
        cases: 128,
        // Do not persist regression files next to the test sources.
        failure_persistence: None,
        ..Default::default()
    })
    .run(&strategy, |bp| {
        let registry = Arc::new(Registry::with_builtins());
        runtime.block_on(async move {
            let mut interp =
                Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
            let shared: SharedBlueprint = Arc::new(parking_lot::RwLock::new(bp));
            let result = interp.run(&shared, None).await;
            // Termination is the invariant: Ok yields a finished run, Err is a
            // graceful failure (e.g. validation). Neither may hang or panic.
            if let Ok(events) = result {
                prop_assert!(
                    events.iter().any(|e| matches!(e, ExecutionEvent::NodeStarted { .. })),
                    "a completed run must have started nodes"
                );
            }
            Ok(())
        })
    })
    .expect("random blueprint execution must not panic");
}
