//! Integration tests for the blueprint DSL.

use metteur_shared::dsl::{compile, decompile};
use metteur_shared::SharedError;

const FLOW: &str = "\
blueprint \"MyFlow\"
# a comment
entry start: Start(A = 4, B = 3)
sum: Add(A <- start.A, B <- start.B)
verify: Validator(Actual <- sum.Result, mode = \"gte\", Expected = 6)
start -> sum
sum -> verify
";

#[test]
fn compiles_reference_flow() {
    let bp = compile(FLOW).unwrap();
    assert_eq!(bp.name, "MyFlow");
    assert_eq!(bp.nodes.len(), 3);
    let start = bp.node(bp.entry_node_id).unwrap();
    assert_eq!(start.kind, "Start");
    assert_eq!(start.data["A"], 4);
    // start.exec -> sum.exec and sum.exec -> verify.exec (2) plus 3 data edges.
    assert_eq!(bp.edges.len(), 5);
}

#[test]
fn decompile_round_trips_structure() {
    let bp = compile(FLOW).unwrap();
    let text = decompile(&bp);
    let bp2 = compile(&text).unwrap();
    assert_eq!(bp2.nodes.len(), bp.nodes.len());
    assert_eq!(bp2.edges.len(), bp.edges.len());
}

#[test]
fn unknown_kind_reports_clear_error() {
    let err = compile("entry a: Nope(x = 1)").unwrap_err();
    assert!(err.to_string().contains("unknown node kind 'Nope'"));
    assert!(err.to_string().contains("line 1"));
}

#[test]
fn unbalanced_parens_are_rejected() {
    assert!(compile("entry a: Start(").is_err());
}

#[test]
fn args_may_span_lines_inside_parens() {
    let source = "blueprint \"T\"\nentry s: Start(\n  A = 4,\n  B = 3,\n)\ns -> e\ne: End\n";
    let bp = compile(source).unwrap();
    assert_eq!(bp.nodes.len(), 2);
    assert_eq!(bp.node(bp.entry_node_id).unwrap().data["A"], 4);
}

#[test]
fn blank_and_comment_only_lines_are_tolerated() {
    let source = "\
# leading comment

blueprint \"T\"

# another comment
entry s: Start(A = 1)

# trailing comment
";
    let bp = compile(source).unwrap();
    assert_eq!(bp.nodes.len(), 1);
}

#[test]
fn listed_entries_become_entry_node() {
    // Without an `entry` prefix the first node is the entry.
    let bp = compile("n1: Start(A = 1)\nn2: End").unwrap();
    assert_eq!(bp.node(bp.entry_node_id).unwrap().kind, "Start");
}

#[test]
fn hyphen_in_aliases_and_pins_is_supported() {
    let source = "entry main-flow: Start(A = 1)\nadd-one: Add(A <- main-flow.A, B = 2)\nmain-flow -> add-one\n";
    let bp = compile(source).unwrap();
    assert_eq!(bp.nodes.len(), 2);
    assert_eq!(bp.edges.len(), 2);
}

#[test]
fn data_wires_compile_to_edges() {
    let source = "entry s: Start(A = 1, B = 2)\nadd: Add(A = 1, B = 2)\ns -> add\n$add.A <- s.A\n";
    let bp = compile(source).unwrap();
    // 1 exec edge + 1 data wire.
    assert_eq!(bp.edges.len(), 2);
}

#[test]
fn errors_are_invalid_variant() {
    let err = compile("garbage without structure").unwrap_err();
    assert!(matches!(err, SharedError::Invalid(_)));
}