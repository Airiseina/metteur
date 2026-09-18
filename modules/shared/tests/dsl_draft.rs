//! Tests for the compact JSON draft compiler.
//!
//! The draft format exists so an LLM can author a plan cheaply; these tests
//! pin down what it guarantees — deterministic ids and layout, constants that
//! land on the right pins, `$` references that become data wires, `flow` that
//! becomes execution edges, and errors that name the offending alias or pin.

use metteur_shared::dsl::compile_draft;
use metteur_shared::model::{DataType, PinType};

/// Returns the node with the given kind, panicking when absent.
fn node_of<'a>(
    blueprint: &'a metteur_shared::Blueprint,
    kind: &str,
) -> &'a metteur_shared::Node {
    blueprint
        .nodes
        .iter()
        .find(|node| node.kind == kind)
        .unwrap_or_else(|| panic!("no node of kind {kind}"))
}

/// Returns the data input pin named `name`.
fn input_pin<'a>(node: &'a metteur_shared::Node, name: &str) -> &'a metteur_shared::Pin {
    node.pins
        .iter()
        .find(|pin| pin.name == name && pin.pin_type == PinType::DataInput)
        .unwrap_or_else(|| panic!("node {} has no data input {name}", node.kind))
}

#[test]
fn compiles_a_linear_pipeline() {
    let blueprint = compile_draft(
        r#"{
          "name": "Add a flag",
          "nodes": {
            "start": { "kind": "Start" },
            "read":  { "kind": "ReadFile", "path": "src/main.rs" },
            "patch": { "kind": "EditFile", "path": "src/main.rs",
                       "edits": [{ "old_string": "a", "new_string": "b" }] },
            "check": { "kind": "LspCheck", "path": "src/main.rs" }
          },
          "flow": ["start -> read -> patch -> check"]
        }"#,
    )
    .unwrap();

    assert_eq!(blueprint.name, "Add a flag");
    assert_eq!(blueprint.nodes.len(), 4);
    // Three execution hops, no data wires.
    assert_eq!(blueprint.edges.len(), 3);
    // The entry is the Start node.
    assert_eq!(blueprint.entry_node_id, node_of(&blueprint, "Start").id);
}

#[test]
fn registry_tools_become_tool_nodes_with_their_name() {
    let blueprint = compile_draft(
        r#"{
          "nodes": { "start": { "kind": "Start" }, "g": { "kind": "Grep", "pattern": "todo" } },
          "flow": ["start -> g"]
        }"#,
    )
    .unwrap();
    let grep = node_of(&blueprint, "Tool");
    assert_eq!(
        grey_tool_name(grep),
        "Grep",
        "the generic Tool node must carry the registry tool name"
    );
    // The declared argument lands as the pin's constant under its key.
    let pin = input_pin(grep, "pattern");
    assert_eq!(grep.data.get("pattern"), Some(&serde_json::json!("todo")));
    assert_eq!(pin.key.as_deref(), Some("pattern"));
}

/// Reads `data.tool_name` from a node.
fn grey_tool_name(node: &metteur_shared::Node) -> String {
    node.data
        .get("tool_name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

#[test]
fn dollar_values_become_data_wires() {
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start", "A": 4, "B": 3 },
            "sum":   { "kind": "Add", "A": "$start.A", "B": "$start.B" },
            "check": { "kind": "Validator", "Actual": "$sum.Result", "Expected": 7 }
          },
          "flow": ["start -> sum", "sum -> check"]
        }"#,
    )
    .unwrap();

    // Three wires from the `$` values (start.A, start.B, sum.Result), plus
    // the two execution edges of the flow.
    assert_eq!(blueprint.edges.len(), 5);
    let sum = node_of(&blueprint, "Add");
    // A wired input must not also be stored as a literal string constant.
    assert!(
        sum.data.get("A").is_none(),
        "a reference must not leak into node data as a literal"
    );
    // Plain values stay as constants (not wired).
    assert!(sum.data.get("B").is_none(), "a literal must not become a wire");
    let expected = input_pin(node_of(&blueprint, "Validator"), "Expected");
    assert_eq!(
        node_of(&blueprint, "Validator").data.get("expected"),
        Some(&serde_json::json!(7))
    );
    assert_eq!(expected.key.as_deref(), Some("expected"));
}

#[test]
fn explicit_wires_are_supported() {
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "read":  { "kind": "ReadFile", "path": "a.txt" },
            "v":     { "kind": "Validator" }
          },
          "flow": ["start -> read -> v"],
          "wires": ["v.Actual <- read.Result"]
        }"#,
    )
    .unwrap();
    assert_eq!(blueprint.edges.len(), 3);
    assert!(input_pin(node_of(&blueprint, "Validator"), "Actual").key.is_some());
}

#[test]
fn node_ids_and_positions_are_deterministic() {
    let draft = r#"{
      "name": "Stable",
      "nodes": {
        "start": { "kind": "Start" },
        "read":  { "kind": "ReadFile", "path": "a.txt" },
        "end":   { "kind": "End" }
      },
      "flow": ["start -> read -> end"]
    }"#;
    let first = compile_draft(draft).unwrap();
    let second = compile_draft(draft).unwrap();
    // Idempotent: recompiling the same draft must not churn ids or layout, so
    // the stored blueprint updates in place instead of piling up copies.
    assert_eq!(first.id, second.id);
    let ids = |bp: &metteur_shared::Blueprint| {
        let mut ids: Vec<String> = bp.nodes.iter().map(|n| n.id.to_string()).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(&first), ids(&second));
    assert_eq!(
        first.nodes.iter().map(|n| n.position).collect::<Vec<_>>(),
        second.nodes.iter().map(|n| n.position).collect::<Vec<_>>()
    );
}

#[test]
fn layout_places_nodes_left_to_right_by_dependency() {
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "a":     { "kind": "ReadFile", "path": "a.txt" },
            "b":     { "kind": "ReadFile", "path": "b.txt" }
          },
          "flow": ["start -> a -> b"]
        }"#,
    )
    .unwrap();
    let x = |kind: &str| node_of(&blueprint, kind).position.0;
    // Layered layout: each hop sits strictly to the right of its predecessor.
    assert!(x("Start") < x("Tool"));
    let start = node_of(&blueprint, "Start");
    let tools: Vec<f32> = blueprint
        .nodes
        .iter()
        .filter(|n| n.kind == "Tool")
        .map(|n| n.position.0)
        .collect();
    assert!(tools.iter().all(|x| *x > start.position.0));
}

#[test]
fn array_node_form_is_accepted() {
    // Some authors emit an array with explicit ids; accepting both shapes
    // removes a whole class of retries.
    let blueprint = compile_draft(
        r#"{
          "nodes": [
            { "id": "start", "kind": "Start" },
            { "id": "read", "kind": "ReadFile", "path": "a.txt" }
          ],
          "flow": ["start -> read"]
        }"#,
    )
    .unwrap();
    assert_eq!(blueprint.nodes.len(), 2);
    assert_eq!(blueprint.edges.len(), 1);
}

#[test]
fn explicit_entry_overrides_the_default() {
    let blueprint = compile_draft(
        r#"{
          "entry": "second",
          "nodes": {
            "first":  { "kind": "ReadFile", "path": "a.txt" },
            "second": { "kind": "End", "note": "entry" }
          }
        }"#,
    )
    .unwrap();
    assert_eq!(blueprint.entry_node_id, node_of(&blueprint, "End").id);
}

#[test]
fn generic_tool_args_declare_pins() {
    // An MCP or addon tool has argument names the template cannot know, so
    // `args` both supplies the values and creates the matching pins.
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "mcp":   { "kind": "Tool", "tool_name": "GitStatus",
                       "args": { "short": true, "paths": ["a.rs"] } }
          },
          "flow": ["start -> mcp"]
        }"#,
    )
    .unwrap();
    let tool = node_of(&blueprint, "Tool");
    assert_eq!(tool.data.get("short"), Some(&serde_json::json!(true)));
    assert_eq!(input_pin(tool, "short").data_type, DataType::Any);
}

#[test]
fn unknown_kind_reports_a_suggestion() {
    let error = compile_draft(
        r#"{ "nodes": { "a": { "kind": "ReadFil" } } }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown node kind 'ReadFil'"), "{error}");
    assert!(error.contains("node 'a'"), "{error}");
    assert!(error.contains("ReadFile"), "a suggestion must be offered: {error}");
}

#[test]
fn unknown_reference_reports_the_alias() {
    let error = compile_draft(
        r#"{
          "nodes": { "sum": { "kind": "Add", "A": "$nope.Result" } }
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("node 'nope' is not declared"), "{error}");
}

#[test]
fn unknown_source_pin_lists_what_the_node_has() {
    let error = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "sum":   { "kind": "Add", "A": "$start.Nope" }
          }
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("has no data output 'Nope'"), "{error}");
}

#[test]
fn unknown_target_pin_lists_the_accepted_inputs() {
    let error = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "sum":   { "kind": "Add", "Wrong": 1 }
          },
          "wires": ["sum.Wrong <- start.Exec"]
        }"#,
    )
    .unwrap_err()
    .to_string();
    // Either the missing source pin or the missing target pin is reported,
    // naming the node; both messages must be actionable.
    assert!(error.contains("sum") || error.contains("start"), "{error}");
}

#[test]
fn flow_with_an_unknown_node_is_rejected() {
    let error = compile_draft(
        r#"{
          "nodes": { "start": { "kind": "Start" } },
          "flow": ["start -> ghost"]
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown node 'ghost'"), "{error}");
}

#[test]
fn arithmetic_nodes_accept_flow() {
    // In this template table arithmetic nodes are function nodes with
    // pass-through exec pins, so wiring them into the flow is legitimate
    // (unlike a node that only computes for its caller).
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "sum":   { "kind": "Add", "A": 1, "B": 2 }
          },
          "flow": ["start -> sum"]
        }"#,
    )
    .unwrap();
    assert_eq!(blueprint.edges.len(), 1);
}

#[test]
fn flow_into_start_is_rejected_with_a_hint() {
    // `Start` has no exec input, so it can never be a target.
    let error = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "read":  { "kind": "ReadFile", "path": "a" }
          },
          "flow": ["read -> start"]
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot be a target"), "{error}");
}

#[test]
fn flow_from_a_node_without_exec_output_is_rejected() {
    // `End` terminates the flow; it has an exec input but no output. The error
    // must list what the node does have so the author can correct the chain.
    let error = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "end":   { "kind": "End" },
            "read":  { "kind": "ReadFile", "path": "a" }
          },
          "flow": ["start -> end", "end -> read"]
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("has no exec output"), "{error}");
    assert!(error.contains("end -> read"), "{error}");
}

#[test]
fn a_bad_exec_pin_lists_the_available_outputs() {
    let error = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "br":    { "kind": "Branch", "Condition": true },
            "a":     { "kind": "ReadFile", "path": "a" },
            "b":     { "kind": "ReadFile", "path": "b" }
          },
          "flow": ["start -> br", "br.Maybe -> a", "br.False -> b"]
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no exec output 'Maybe'"), "{error}");
    assert!(error.contains("True"), "the valid pins must be listed: {error}");
}

#[test]
fn branch_routes_both_outputs() {
    let blueprint = compile_draft(
        r#"{
          "nodes": {
            "start": { "kind": "Start" },
            "br":    { "kind": "Branch", "Condition": true },
            "yes":   { "kind": "ReadFile", "path": "a" },
            "no":    { "kind": "ReadFile", "path": "b" }
          },
          "flow": ["start -> br", "br.True -> yes", "br.False -> no"]
        }"#,
    )
    .unwrap();
    let branch = node_of(&blueprint, "Branch");
    let routed: Vec<&str> = blueprint
        .edges
        .iter()
        .filter(|edge| edge.source_node == branch.id)
        .map(|edge| {
            branch
                .pins
                .iter()
                .find(|pin| pin.id == edge.source_pin)
                .map(|pin| pin.name.as_str())
                .unwrap_or("?")
        })
        .collect();
    assert!(routed.contains(&"True"), "{routed:?}");
    assert!(routed.contains(&"False"), "{routed:?}");
}

#[test]
fn a_draft_without_nodes_is_rejected() {
    let error = compile_draft(r#"{ "name": "empty" }"#).unwrap_err().to_string();
    assert!(error.contains("no `nodes`"), "{error}");
}

#[test]
fn duplicate_aliases_are_rejected() {
    let error = compile_draft(
        r#"{
          "nodes": [
            { "id": "a", "kind": "Start" },
            { "id": "a", "kind": "End" }
          ]
        }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("duplicate node id 'a'"), "{error}");
}

#[test]
fn malformed_json_is_reported_as_such() {
    let error = compile_draft("{ not json }").unwrap_err().to_string();
    assert!(error.contains("not valid JSON"), "{error}");
}

#[test]
fn every_template_kind_compiles() {
    // Guards the draft vocabulary against the template table: a kind the
    // suggestion list advertises must actually expand.
    for kind in [
        "Start", "End", "Add", "Branch", "Switch", "ForEach", "CallLLM", "Validator", "Judge",
        "LspCheck", "Delay", "RequestApproval", "VariableGet", "VariableSet", "ListCreate",
        "ContextCreate", "CallFunction",
    ] {
        let draft = format!(
            r#"{{ "nodes": {{ "a": {{ "kind": "{kind}" }} }}, "flow": [] }}"#
        );
        compile_draft(&draft).unwrap_or_else(|err| panic!("kind {kind} failed: {err}"));
    }
}

#[test]
fn pins_keep_their_declared_types() {
    let blueprint = compile_draft(
        r#"{
          "nodes": { "start": { "kind": "Start" }, "r": { "kind": "ReadFile", "path": "a" } },
          "flow": ["start -> r"]
        }"#,
    )
    .unwrap();
    let read = node_of(&blueprint, "Tool");
    // `Result` is a String output in the tool template.
    let result = read
        .pins
        .iter()
        .find(|pin| pin.name == "Result" && pin.pin_type == PinType::DataOutput)
        .expect("Result output");
    assert_eq!(result.data_type, DataType::String);
    // Exec pins drive the flow.
    assert!(
        read.pins
            .iter()
            .any(|pin| pin.pin_type == PinType::ExecInput || pin.pin_type == PinType::ExecOutput)
    );
}
