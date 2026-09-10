//! Property-based tests for the blueprint DSL.
//!
//! Two invariants hold for any input:
//! 1. [`decompile`] followed by [`compile`] preserves the graph structure
//!    (node count, edge count) and every constant on data input pins.
//! 2. [`compile`] never panics, whatever the source text is.

use metteur_shared::dsl::{compile, decompile};
use metteur_shared::model::blueprint::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};
use proptest::collection::vec as vec_strategy;
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::FileFailurePersistence;
use uuid::Uuid;

/// Minimal pin layout for the node kinds the generator knows about.
fn pins_of(kind: &str) -> Vec<(String, PinType)> {
    let mut pins: Vec<(String, PinType)> = Vec::new();
    match kind {
        "Start" => pins.push(("Exec".into(), PinType::ExecOutput)),
        "End" => pins.push(("Exec".into(), PinType::ExecInput)),
        "Branch" => {
            pins.push(("Exec".into(), PinType::ExecInput));
            pins.push(("True".into(), PinType::ExecOutput));
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
        // Pin names must match the DSL templates, otherwise a constant cannot
        // survive the round trip (the template defines the canonical layout).
        "Validator" => {
            pins.push(("Actual".into(), PinType::DataInput));
            pins.push(("Expected".into(), PinType::DataInput));
        }
        "Judge" => {
            pins.push(("Score".into(), PinType::DataInput));
            pins.push(("Result".into(), PinType::DataInput));
        }
        _ => {}
    }
    pins
}

/// Generates an acyclic chain blueprint: node 0 is Start, exec edges connect
/// node `i` to `i+1`, and a random subset of data input pins gets constants.
fn any_blueprint() -> impl Strategy<Value = Blueprint> {
    let kinds = vec_strategy(
        select(&[
            "Add",
            "Subtract",
            "Multiply",
            "Divide",
            "Validator",
            "Judge",
            "Tool",
            "Abstract",
            "End",
        ]),
        1..=5,
    );
    let constants = vec_strategy((any::<usize>(), any::<String>(), any::<i64>()), 0..24);
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
        // Drop constants onto matching data input pins of the target node.
        for (idx, name, value) in constants {
            if nodes.is_empty() {
                continue;
            }
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
            name: "prop".to_string(),
            nodes,
            edges,
            entry_node_id,
        }
    })
}

proptest! {
    // Pin the regression file next to this test: the default SourceParallel
    // persistence cannot resolve a crate root from an integration test and
    // warns on every run.
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/dsl_proptest.proptest-regressions",
        )))),
        ..ProptestConfig::default()
    })]

    #[test]
    fn round_trip_preserves_structure(input in any_blueprint()) {
        let text = decompile(&input);
        let recompiled = compile(&text).expect("round trip must compile");
        assert_eq!(recompiled.nodes.len(), input.nodes.len());
        assert_eq!(recompiled.edges.len(), input.edges.len());

        for (a, b) in input.nodes.iter().zip(recompiled.nodes.iter()) {
            for pin in a.pins.iter().filter(|p| p.pin_type == PinType::DataInput) {
                // Constants live under the pin's semantic key (display name
                // when the key is absent), which decompile/compile preserve.
                let key_before = pin.key.as_deref().unwrap_or(&pin.name);
                let before = a.data.get(key_before);
                let after = b.pins
                    .iter()
                    .find(|p| p.name == pin.name && p.pin_type == PinType::DataInput)
                    .and_then(|p| {
                        let key = p.key.as_deref().unwrap_or(&p.name);
                        b.data.get(key)
                    });
                assert_eq!(before, after, "constant on pin {} drifted", pin.name);
            }
        }
    }

    #[test]
    fn compile_never_panics(
        input in vec_strategy(any::<char>(), 0..200).prop_map(|cs| cs.into_iter().collect::<String>())
    ) {
        let _ = compile(&input);
    }
}
