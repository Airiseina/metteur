//! Integration tests for the blueprint DSL.

use metteur_shared::SharedError;
use metteur_shared::dsl::{compile, decompile};

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
#[test]
fn validator_retry_args_fold_into_retry_object() {
    let source = "entry s: Start(A = 1)\nv: Validator(Actual <- s.A, Expected = 1, retry_max_attempts = 3, retry_rollback = false)\ns -> v\n";
    let bp = compile(source).unwrap();
    let validator = bp.nodes.iter().find(|n| n.kind == "Validator").unwrap();
    assert_eq!(validator.data["retry"]["max_attempts"], 3);
    assert_eq!(validator.data["retry"]["rollback"], false);
    assert!(validator.data.get("retry_max_attempts").is_none());
}

#[test]
fn validator_retry_survives_decompile_round_trip() {
    let source = "entry s: Start(A = 1)\nv: Validator(Actual <- s.A, Expected = 1, retry_max_attempts = 3)\ns -> v\n";
    let bp = compile(source).unwrap();
    let text = decompile(&bp);
    let bp2 = compile(&text).unwrap();
    let validator = bp2.nodes.iter().find(|n| n.kind == "Validator").unwrap();
    assert_eq!(validator.data["retry"]["max_attempts"], 3);
}

#[test]
fn branch_exec_pins_use_capitalized_names() {
    let bp = compile("entry s: Start\nb: Branch(Condition = true)\ns -> b\n").unwrap();
    let branch = bp.nodes.iter().find(|n| n.kind == "Branch").unwrap();
    let names: Vec<&str> = branch
        .pins
        .iter()
        .filter(|p| p.pin_type == metteur_shared::PinType::ExecOutput)
        .map(|p| p.name.as_str())
        .collect();
    assert!(names.contains(&"True"));
    assert!(names.contains(&"False"));
}

#[test]
fn switch_cases_become_case_pins() {
    let source = "entry s: Start\nsw: Switch(Case = \"a\", cases = [\"a\", \"b\"])\ns -> sw\n";
    let bp = compile(source).unwrap();
    let switch = bp.nodes.iter().find(|n| n.kind == "Switch").unwrap();
    let names: Vec<&str> = switch
        .pins
        .iter()
        .filter(|p| p.pin_type == metteur_shared::PinType::ExecOutput)
        .map(|p| p.name.as_str())
        .collect();
    assert!(names.contains(&"Case_a"));
    assert!(names.contains(&"Case_b"));
    assert!(names.contains(&"Default"));
}

#[test]
fn foreach_and_variable_nodes_compile() {
    let source = "entry s: Start\nfe: ForEach(List = [1,2])\nset: VariableSet(Name = \"x\", Value = 1)\nget: VariableGet(Name = \"x\")\ns -> fe\nfe.Body -> set\nfe.Completed -> get\n";
    let bp = compile(source).unwrap();
    assert_eq!(bp.nodes.len(), 4);
    let fe = bp.nodes.iter().find(|n| n.kind == "ForEach").unwrap();
    assert!(fe.pins.iter().any(|p| p.name == "Body"));
    assert!(fe.pins.iter().any(|p| p.name == "Completed"));
    let text = decompile(&bp);
    let bp2 = compile(&text).unwrap();
    assert_eq!(bp2.nodes.len(), bp.nodes.len());
}

#[test]
fn context_release_and_registry_tools_compile() {
    // Both the deterministic context node and the model-facing tool must be
    // declarable from the DSL.
    let source = "blueprint \"Trim\"
entry start: Start()
trim: ContextRelease(Context <- start.Context, all = true)
read: ReleaseContext(patterns = [\"src/**/*.rs\"], keep_recent = 2)
start -> trim
trim -> read
";
    let bp = compile(source).unwrap();
    let trim = bp.nodes.iter().find(|n| n.kind == "ContextRelease").expect("node present");
    assert_eq!(trim.data["all"], true);
    assert!(trim.pins.iter().any(|p| p.name == "Released"));
    let read = bp.nodes.iter().find(|n| n.kind == "Tool").expect("tool node present");
    assert_eq!(read.data["tool_name"], "ReleaseContext");
}
