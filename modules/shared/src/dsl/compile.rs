//! Compiles parsed statements into a [`Blueprint`].
//!
//! Node pin layouts come from a built-in template table (the authoritative
//! source is the daemon registry; the table only covers the core node kinds).
//! Errors carry the source line/column recorded by the parser.

use std::collections::{HashMap, VecDeque};

use uuid::Uuid;

use crate::error::{SharedError, SharedResult};
use crate::model::blueprint::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};

use super::parser::Statement;

/// Maximum nodes a compiled blueprint may contain (arbitrary safety limit).
const MAX_NODES: usize = 256;

/// A pin template entry.
pub(crate) struct PinSpec {
    /// Semantic key (matches canvas/node-data keys); empty for exec pins.
    pub(crate) key: &'static str,
    pub(crate) name: &'static str,
    pub(crate) pin_type: PinType,
    pub(crate) data_type: DataType,
}

/// Maps the template's canonical wire name to the pin's semantic key (the key
/// canvas node data and the DSL init block use).
pub(crate) fn key_of(name: &str) -> &'static str {
    match name {
        "A" => "a",
        "B" => "b",
        "In" => "in",
        "Result" => "result",
        "Context" => "context",
        "Text" => "text",
        "Role" => "role",
        "System" => "system",
        "Prompt" => "prompt",
        "ReasoningEffort" => "reasoning_effort",
        "Model" => "model",
        "Temperature" => "temperature",
        "TopP" => "top_p",
        "MaxTokens" => "max_tokens",
        "MaxIterations" => "max_iterations",
        "Seed" => "seed",
        "Input" => "input",
        "Find" => "find",
        "ReplaceWith" => "replaceWith",
        "Start" => "start",
        "Length" => "length",
        "Keep" => "keep",
        "List" => "list",
        "Item" => "item",
        "Index" => "index",
        "Object" => "object",
        "Path" => "path",
        "Value" => "value",
        "ItemA" => "itemA",
        "ItemB" => "itemB",
        "ItemC" => "itemC",
        "Ms" => "ms",
        "Message" => "message",
        "Allowed" => "allowed",
        "ToolName" => "tool_name",
        "Command" => "command",
        "Condition" => "cond",
        "Actual" => "actual",
        "Expected" => "expected",
        "Passed" => "passed",
        "Score" => "score",
        "Success" => "success",
        "Errors" => "errors",
        "Warnings" => "warnings",
        "Diagnostics" => "diagnostics",
        "TimeoutMs" => "timeout_ms",
        other => other.to_ascii_lowercase().leak(),
    }
}

/// Returns the pin layout for a known node kind, if DataType::Any.
/// Registry tool names the DSL accepts as node kinds.
///
/// Each one compiles to a `Tool` node whose `data.tool_name` selects the
/// registered tool; the shared crate cannot query the daemon registry, so the
/// list is explicit and grows with the tool set.
pub(crate) const REGISTRY_TOOL_KINDS: &[&str] = &[
    "ReadFile",
    "WriteFile",
    "EditFile",
    "ListDirectory",
    "SearchFile",
    "Grep",
    "Glob",
    "ExecuteCommand",
    "StartCommand",
    "JobStatus",
    "WaitJob",
    "KillJob",
    "GetDependencies",
    "CheckDiagnostics",
    "GetHover",
    "FindDefinition",
    "ReleaseContext",
];

/// Returns every node kind the DSL and draft formats accept.
///
/// Used for "did you mean" hints; the list is the union of the template table
/// and the registry tool kinds. A template that answers for a probe kind is
/// what defines membership, so this list cannot drift from [`template`].
pub(crate) fn known_kinds() -> Vec<&'static str> {
    const CANDS: &[&str] = &[
        "Start", "End", "Add", "Subtract", "Multiply", "Divide", "Modulo", "Power", "Min", "Max",
        "Abs", "Round", "Equal", "NotEqual", "Greater", "Less", "GreaterEqual", "LessEqual", "And",
        "Or", "Xor", "Not", "Concat", "Length", "Upper", "Lower", "Trim", "Contains", "Replace",
        "Substring", "ToString", "ToInt", "ToFloat", "ToBool", "ToJson", "ParseJson", "ListCreate",
        "ListAppend", "ListGet", "ListLength", "ListContains", "JsonGet", "JsonSet",
        "ContextCreate", "ContextClone", "ContextMerge", "ContextFilter", "ContextTrim",
        "ContextRelease", "ContextToText", "Delay", "VariableGet", "VariableSet", "Branch",
        "Switch", "ForEach", "RequestApproval", "CallLLM", "Tool", "Validator", "Judge",
        "LspCheck", "Abstract", "CallFunction", "FunctionEntry", "FunctionExit",
    ];
    let mut kinds: Vec<&'static str> = CANDS
        .iter()
        .copied()
        .filter(|kind| template(kind).is_some())
        .chain(REGISTRY_TOOL_KINDS.iter().copied())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

pub(crate) fn template(kind: &str) -> Option<Vec<PinSpec>> {
    let i = |n: &'static str| PinSpec {
        key: "",
        name: n,
        pin_type: PinType::ExecInput,
        data_type: DataType::Void,
    };
    let o = |n: &'static str| PinSpec {
        key: "",
        name: n,
        pin_type: PinType::ExecOutput,
        data_type: DataType::Void,
    };
    let di = |n: &'static str, dt: DataType| PinSpec {
        key: key_of(n),
        name: n,
        pin_type: PinType::DataInput,
        data_type: dt,
    };
    let do_ = |n: &'static str, dt: DataType| PinSpec {
        key: key_of(n),
        name: n,
        pin_type: PinType::DataOutput,
        data_type: dt,
    };
    // Exec pins are named `x-in`/`x-out` to match the canvas convention.
    let exec = |exec_in: bool| {
        let mut pins = Vec::new();
        if exec_in {
            pins.push(i("x-in"));
            pins.push(o("x-out"));
        }
        pins
    };
    // Two numeric/typed inputs + one `Result` output.
    let bin = |a: DataType, b: DataType, r: DataType| {
        let mut pins = exec(true);
        pins.push(di("A", a));
        pins.push(di("B", b));
        pins.push(do_("Result", r));
        pins
    };
    // One `In` input + one `Result` output.
    let un = |i: DataType, r: DataType| {
        let mut pins = exec(true);
        pins.push(di("In", i));
        pins.push(do_("Result", r));
        pins
    };
    Some(match kind {
        "Start" => vec![o("x-out"), do_("Context", DataType::Context)],
        "End" => vec![i("x-in")],
        "Add" | "Subtract" | "Multiply" | "Divide" | "Modulo" | "Power" | "Min" | "Max" => {
            bin(DataType::Float, DataType::Float, DataType::Float)
        }
        "Abs" | "Round" => un(DataType::Float, DataType::Float),
        "Equal" | "NotEqual" => bin(DataType::Any, DataType::Any, DataType::Bool),
        "Greater" | "Less" | "GreaterEqual" | "LessEqual" => {
            bin(DataType::Float, DataType::Float, DataType::Bool)
        }
        "And" | "Or" | "Xor" => bin(DataType::Bool, DataType::Bool, DataType::Bool),
        "Not" => un(DataType::Bool, DataType::Bool),
        "Concat" => bin(DataType::String, DataType::String, DataType::String),
        "Length" => un(DataType::String, DataType::Int),
        "Upper" | "Lower" | "Trim" => un(DataType::String, DataType::String),
        "Contains" => bin(DataType::String, DataType::String, DataType::Bool),
        "Replace" => {
            let mut pins = exec(true);
            pins.push(di("Input", DataType::String));
            pins.push(di("Find", DataType::String));
            pins.push(di("ReplaceWith", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "Substring" => {
            let mut pins = exec(true);
            pins.push(di("In", DataType::String));
            pins.push(di("Start", DataType::Int));
            pins.push(di("Length", DataType::Int));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "ToString" | "ToInt" | "ToFloat" | "ToBool" | "ToJson" | "ParseJson" => {
            un(DataType::Any, DataType::Any)
        }
        "ListCreate" => {
            let mut pins = exec(true);
            for item in ["ItemA", "ItemB", "ItemC"] {
                pins.push(di(item, DataType::Any));
            }
            pins.push(do_("Result", DataType::List(Box::new(DataType::Any))));
            pins
        }
        "ListAppend" => {
            let mut pins = exec(true);
            pins.push(di("List", DataType::List(Box::new(DataType::Any))));
            pins.push(di("Item", DataType::Any));
            pins.push(do_("Result", DataType::List(Box::new(DataType::Any))));
            pins
        }
        "ListGet" => {
            let mut pins = exec(true);
            pins.push(di("List", DataType::List(Box::new(DataType::Any))));
            pins.push(di("Index", DataType::Int));
            pins.push(do_("Result", DataType::Any));
            pins
        }
        "ListLength" => {
            let mut pins = exec(true);
            pins.push(di("List", DataType::List(Box::new(DataType::Any))));
            pins.push(do_("Result", DataType::Int));
            pins
        }
        "ListContains" => {
            let mut pins = exec(true);
            pins.push(di("List", DataType::List(Box::new(DataType::Any))));
            pins.push(di("Item", DataType::Any));
            pins.push(do_("Result", DataType::Bool));
            pins
        }
        "JsonGet" => {
            let mut pins = exec(true);
            pins.push(di("Object", DataType::Json));
            pins.push(di("Path", DataType::String));
            pins.push(do_("Result", DataType::Any));
            pins
        }
        "JsonSet" => {
            let mut pins = exec(true);
            pins.push(di("Object", DataType::Json));
            pins.push(di("Path", DataType::String));
            pins.push(di("Value", DataType::Any));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "ContextCreate" => {
            let mut pins = exec(true);
            pins.push(di("System", DataType::String));
            pins.push(di("Prompt", DataType::String));
            pins.push(do_("Result", DataType::Context));
            pins
        }
        "ContextClone" => un(DataType::Context, DataType::Context),
        "ContextMerge" => {
            let mut pins = exec(true);
            pins.push(di("Context", DataType::Context));
            pins.push(di("Text", DataType::String));
            pins.push(di("Role", DataType::Choice));
            pins.push(do_("Result", DataType::Context));
            pins
        }
        "ContextFilter" => {
            let mut pins = exec(true);
            pins.push(di("Context", DataType::Context));
            pins.push(di("Role", DataType::Choice));
            pins.push(do_("Result", DataType::Context));
            pins
        }
        "ContextTrim" => {
            let mut pins = exec(true);
            pins.push(di("Context", DataType::Context));
            pins.push(di("Keep", DataType::Int));
            pins.push(do_("Result", DataType::Context));
            pins
        }
        "ContextRelease" => {
            let mut pins = exec(true);
            pins.push(di("Context", DataType::Context));
            pins.push(di("Paths", DataType::List(Box::new(DataType::String))));
            pins.push(di("Patterns", DataType::List(Box::new(DataType::String))));
            pins.push(di("Tools", DataType::List(Box::new(DataType::String))));
            pins.push(do_("Result", DataType::Context));
            pins.push(do_("Released", DataType::Int));
            pins.push(do_("Freed", DataType::Int));
            pins
        }
        "ContextToText" => un(DataType::Context, DataType::String),
        "Delay" => {
            let mut pins = exec(true);
            pins.push(di("Ms", DataType::Int));
            pins
        }
        "Branch" => {
            let mut pins = vec![i("x-in")];
            pins.push(PinSpec {
                key: "",
                name: "True",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(PinSpec {
                key: "",
                name: "False",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(di("Condition", DataType::Bool));
            pins.push(do_("Result", DataType::Bool));
            pins
        }
        "Switch" => {
            let mut pins = vec![i("x-in")];
            pins.push(PinSpec {
                key: "",
                name: "Default",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(di("Case", DataType::Any));
            pins.push(do_("Result", DataType::Any));
            pins
        }
        "ForEach" => {
            let mut pins = vec![i("x-in")];
            pins.push(PinSpec {
                key: "",
                name: "Body",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(PinSpec {
                key: "",
                name: "Completed",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(di("List", DataType::Any));
            pins.push(do_("Iteration", DataType::Any));
            pins.push(do_("Index", DataType::Int));
            pins
        }
        "VariableSet" => {
            let mut pins = exec(true);
            pins.push(di("Name", DataType::String));
            pins.push(di("Value", DataType::Any));
            pins.push(do_("Value", DataType::Any));
            pins
        }
        "VariableGet" => {
            let mut pins = exec(true);
            pins.push(di("Name", DataType::String));
            pins.push(do_("Value", DataType::Any));
            pins
        }
        "CallLLM" => {
            let mut pins = exec(true);
            pins.push(do_("Result", DataType::String));
            pins.push(do_("Context", DataType::Context));
            pins.push(di("Context", DataType::Context));
            pins.push(di("ReasoningEffort", DataType::Choice));
            pins.push(di("Model", DataType::String));
            pins.push(di("Prompt", DataType::String));
            pins.push(di("System", DataType::String));
            pins.push(di("Temperature", DataType::Float));
            pins.push(di("TopP", DataType::Float));
            pins.push(di("MaxTokens", DataType::Int));
            pins.push(di("MaxIterations", DataType::Int));
            pins.push(di("Seed", DataType::Int));
            pins
        }
        "Validator" => {
            let mut pins = exec(true);
            pins.push(di("Actual", DataType::Json));
            pins.push(di("Expected", DataType::Json));
            pins.push(do_("Passed", DataType::Bool));
            pins
        }
        "Judge" => {
            let mut pins = exec(true);
            pins.push(di("Score", DataType::Float));
            pins.push(di("Result", DataType::Json));
            pins.push(do_("Success", DataType::Bool));
            pins
        }
        "LspCheck" => {
            let mut pins = exec(true);
            pins.push(di("Path", DataType::String));
            pins.push(di("TimeoutMs", DataType::Int));
            pins.push(do_("Passed", DataType::Bool));
            pins.push(do_("Errors", DataType::Int));
            pins.push(do_("Warnings", DataType::Int));
            pins.push(do_("Diagnostics", DataType::String));
            pins.push(do_("Problems", DataType::List(Box::new(DataType::Json))));
            pins
        }
        "RequestApproval" => {
            let mut pins = vec![i("x-in")];
            pins.push(PinSpec {
                key: "",
                name: "x-out",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(PinSpec {
                key: "",
                name: "Approved",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(PinSpec {
                key: "",
                name: "Denied",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(di("Message", DataType::String));
            pins.push(do_("Allowed", DataType::Bool));
            pins
        }
        "Tool" => {
            let mut pins = exec(true);
            pins.push(di("ToolName", DataType::String));
            pins.push(di("Command", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        // Round 13 search/edit tools: the DSL names them directly and the
        // compiler rewrites the kind to `Tool` with `tool_name` set, so the
        // pins mirror each tool's JSON schema argument names.
        "Grep" => {
            let mut pins = exec(true);
            pins.push(di("pattern", DataType::String));
            pins.push(di("path", DataType::String));
            pins.push(di("glob", DataType::String));
            pins.push(di("case_insensitive", DataType::Bool));
            pins.push(di("context_lines", DataType::Int));
            pins.push(di("max_matches", DataType::Int));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "Glob" => {
            let mut pins = exec(true);
            pins.push(di("pattern", DataType::String));
            pins.push(di("path", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "EditFile" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(di("edits", DataType::Json));
            pins.push(di("expected_sha256", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "ReadFile" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(di("offset", DataType::Int));
            pins.push(di("limit", DataType::Int));
            pins.push(di("line_numbers", DataType::Bool));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "WriteFile" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(di("content", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "ListDirectory" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "SearchFile" => {
            let mut pins = exec(true);
            pins.push(di("root", DataType::String));
            pins.push(di("query", DataType::String));
            pins.push(do_("Result", DataType::List(Box::new(DataType::String))));
            pins
        }
        "ExecuteCommand" => {
            let mut pins = exec(true);
            pins.push(di("command", DataType::String));
            pins.push(di("cwd", DataType::String));
            pins.push(di("timeout_secs", DataType::Int));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "StartCommand" => {
            let mut pins = exec(true);
            pins.push(di("command", DataType::String));
            pins.push(di("cwd", DataType::String));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "JobStatus" => {
            let mut pins = exec(true);
            pins.push(di("job_id", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "WaitJob" => {
            let mut pins = exec(true);
            pins.push(di("job_id", DataType::String));
            pins.push(di("timeout_secs", DataType::Int));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "KillJob" => {
            let mut pins = exec(true);
            pins.push(di("job_id", DataType::String));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "GetDependencies" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "CheckDiagnostics" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(di("timeout_ms", DataType::Int));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "GetHover" | "FindDefinition" => {
            let mut pins = exec(true);
            pins.push(di("path", DataType::String));
            pins.push(di("line", DataType::Int));
            pins.push(di("character", DataType::Int));
            pins.push(do_("Result", DataType::Json));
            pins
        }
        "ReleaseContext" => {
            let mut pins = exec(true);
            pins.push(di("paths", DataType::List(Box::new(DataType::String))));
            pins.push(di("patterns", DataType::List(Box::new(DataType::String))));
            pins.push(di("tools", DataType::List(Box::new(DataType::String))));
            pins.push(di("all", DataType::Bool));
            pins.push(di("keep_recent", DataType::Int));
            pins.push(do_("Result", DataType::String));
            pins
        }
        "Abstract" => exec(true),
        "FunctionEntry" | "FunctionExit" | "CallFunction" => {
            let mut pins = exec(true);
            pins.push(do_("Result", DataType::Json));
            pins
        }
        _ => return None,
    })
}

/// Folds the DSL's flat retry args (`retry_max_attempts`, `retry_rollback`)
/// into the nested `retry` object the interpreter reads, merging with an
/// explicitly written `retry` object when both are present.
fn fold_validator_retry(data: &mut serde_json::Map<String, serde_json::Value>) {
    let flat = [("retry_max_attempts", "max_attempts"), ("retry_rollback", "rollback")];
    let mut retry = data.get("retry").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut changed = false;
    for (flat_key, nested_key) in flat {
        if let Some(v) = data.remove(flat_key) {
            retry.insert(nested_key.to_string(), v);
            changed = true;
        }
    }
    if changed {
        data.insert("retry".to_string(), serde_json::Value::Object(retry));
    }
}

/// Returns the node category a kind belongs to for compiled blueprints.
pub(crate) fn node_type_of(kind: &str) -> NodeType {
    match kind {
        "Start" | "End" | "FunctionEntry" | "FunctionExit" => NodeType::Event,
        "Branch" | "Switch" | "ForEach" | "RequestApproval" | "LspCheck" => NodeType::Control,
        "Add" | "Subtract" | "Multiply" | "Divide" | "Modulo" | "Power" | "Min" | "Max" | "Abs"
        | "Round" | "Equal" | "NotEqual" | "Greater" | "Less" | "GreaterEqual" | "LessEqual"
        | "And" | "Or" | "Xor" | "Not" | "Concat" | "Length" | "Upper" | "Lower" | "Trim"
        | "Contains" | "Replace" | "Substring" | "ToString" | "ToInt" | "ToFloat" | "ToBool"
        | "ToJson" | "ParseJson" | "ListCreate" | "ListAppend" | "ListGet" | "ListLength"
        | "ListContains" | "JsonGet" | "JsonSet" | "ContextCreate" | "ContextClone"
        | "ContextMerge" | "ContextFilter" | "ContextTrim" | "ContextRelease" | "ContextToText"
        | "Delay" | "VariableGet" => NodeType::Pure,
        _ => NodeType::Function,
    }
}

/// Compiles `source` into a blueprint with fresh UUIDs.
pub fn compile(source: &str) -> SharedResult<Blueprint> {
    let statements = super::parser::parse(source)?;
    let mut name = "Untitled".to_string();
    let mut nodes: Vec<&Statement> = Vec::new();
    let mut exec_edges: Vec<&Statement> = Vec::new();
    let mut data_wires: Vec<&Statement> = Vec::new();

    for stmt in &statements {
        match stmt {
            Statement::Header {
                name: n,
            } => name = n.clone(),
            Statement::Node {
                ..
            } => nodes.push(stmt),
            Statement::ExecEdge {
                ..
            } => exec_edges.push(stmt),
            Statement::DataWire {
                ..
            } => data_wires.push(stmt),
        }
    }

    if nodes.is_empty() {
        return Err(SharedError::Invalid("no nodes declared".to_string()));
    }
    if nodes.len() > MAX_NODES {
        return Err(SharedError::Invalid(format!(
            "too many nodes ({} > {MAX_NODES})",
            nodes.len()
        )));
    }

    let entry = nodes
        .iter()
        .copied()
        .find(|s| {
            matches!(
                s,
                Statement::Node {
                    is_entry: true,
                    ..
                }
            )
        })
        .unwrap_or(nodes[0]);
    let entry_alias = match entry {
        Statement::Node {
            alias,
            ..
        } => alias.as_str(),
        _ => "",
    };

    let mut blueprint = Blueprint {
        id: Uuid::new_v4(),
        name,
        nodes: Vec::new(),
        edges: Vec::new(),
        entry_node_id: Uuid::nil(),
    };
    let node_id_of: HashMap<&str, Uuid> = nodes
        .iter()
        .map(|s| match s {
            Statement::Node {
                alias,
                ..
            } => (alias.as_str(), Uuid::new_v4()),
            _ => unreachable!(),
        })
        .collect();
    let mut src_pin_of: HashMap<(&str, &str), (Uuid, Uuid)> = HashMap::new();

    for node_stmt in &nodes {
        let Statement::Node {
            alias,
            kind,
            constants,
            line,
            col,
            ..
        } = *node_stmt
        else {
            unreachable!("nodes only contains Node statements")
        };
        let node_id = node_id_of[alias.as_str()];
        if alias.as_str() == entry_alias {
            blueprint.entry_node_id = node_id;
        }
        let spec = template(kind).ok_or_else(|| {
            SharedError::Invalid(format!(
                "unknown node kind '{kind}' (line {line}, column {col}); use a JSON blueprint instead"
            ))
        })?;
        let mut pins = Vec::new();
        for p in &spec {
            let mut pin =
                Pin::data(p.name.to_string(), p.pin_type, p.data_type.clone(), Uuid::new_v4());
            if !p.key.is_empty() {
                pin.key = Some(p.key.to_string());
            }
            if p.pin_type == PinType::DataOutput {
                src_pin_of.insert((alias.as_str(), p.name), (node_id, pin.id));
            }
            pins.push(pin);
        }
        // Switch case branches are declared with `cases = [...]`; each entry
        // becomes a `Case_<value>` execution output pin.
        if kind == "Switch"
            && let Some(cases) =
                constants.iter().find(|(k, _)| k == "cases").and_then(|(_, v)| v.as_array())
        {
            for case in cases {
                let Some(text) = case.as_str() else {
                    return Err(SharedError::Invalid(format!(
                        "switch cases must be strings (line {line}, column {col})"
                    )));
                };
                let name = format!("Case_{text}");
                if pins.iter().any(|pin: &Pin| pin.name == name) {
                    continue;
                }
                pins.push(Pin::data(name, PinType::ExecOutput, DataType::Void, Uuid::new_v4()));
            }
        }
        // Start carries ad-hoc initial attributes: constants outside the
        // template become data output pins so downstream nodes can reference
        // them (`A <- start.A`). The Start executor emits data values by pin
        // name, matching the stored constant keys.
        if kind == "Start" {
            for (k, _) in constants {
                let known = spec.iter().any(|p| p.key == k || p.name == k)
                    || pins.iter().any(|pin: &Pin| &pin.name == k);
                if !known {
                    let id = Uuid::new_v4();
                    src_pin_of.insert((alias.as_str(), k.as_str()), (node_id, id));
                    pins.push(Pin {
                        id,
                        key: Some(k.clone()),
                        name: k.clone(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Any,
                        ..Default::default()
                    });
                }
            }
        }
        let mut data = serde_json::Map::new();
        for (k, v) in constants {
            // Store constants under the pin's canonical key when one exists, so
            // executor lookups and decompile agree whichever spelling the DSL
            // used (`A` in parens vs `a` in a block).
            let key = spec
                .iter()
                .find(|p| p.key == k || p.name == k)
                .map(|p| p.key)
                .filter(|k| !k.is_empty())
                .unwrap_or(k.as_str());
            data.insert(key.to_string(), v.clone());
        }
        if kind == "Validator" {
            fold_validator_retry(&mut data);
        }
        // The three Round 13 search/edit tools are ordinary registry tools, so
        // the DSL spells them by name and the compiler emits a `Tool` node with
        // the matching `tool_name`; everything else about the node is generic.
        let (node_kind, tool_name) = match kind.as_str() {
            // Registry tools the DSL names directly resolve to `Tool` nodes.
            k if REGISTRY_TOOL_KINDS.contains(&k) => ("Tool", Some(k.to_string())),
            _ => (kind.as_str(), None),
        };
        if let Some(tool_name) = tool_name {
            data.insert("tool_name".to_string(), serde_json::Value::String(tool_name));
        }
        blueprint.nodes.push(Node {
            id: node_id,
            node_type: node_type_of(node_kind),
            kind: node_kind.to_string(),
            position: (0.0, 0.0),
            pins,
            data: serde_json::Value::Object(data),
        });
    }

    // Data refs written inline inside node args (`pin <- alias.pin`).
    for node_stmt in &nodes {
        let Statement::Node {
            alias,
            refs,
            line,
            col,
            ..
        } = *node_stmt
        else {
            unreachable!("nodes only contains Node statements")
        };
        for (target_pin, source) in refs {
            let (source_alias, source_pin) = source
                .split_once('.')
                .ok_or_else(|| SharedError::Invalid(format!("invalid ref '{source}'")))?;
            let (source_node, source_pin_id) = src_pin_of
                .get(&(source_alias, source_pin))
                .cloned()
                .ok_or_else(|| {
                    SharedError::Invalid(format!(
                        "unknown data source '{source}' (node {source_alias} has no data pin '{source_pin}')"
                    ))
                })?;
            let target_node = *node_id_of
                .get(alias.as_str())
                .ok_or_else(|| SharedError::Invalid(format!("unknown node '{alias}'")))?;
            let target_pin_id = find_data_input(&blueprint, target_node, target_pin).map_err(
                |_| {
                    SharedError::Invalid(format!(
                        "node '{alias}' has no data input pin '{target_pin}' (line {line}, column {col})"
                    ))
                },
            )?;
            blueprint.edges.push(Edge {
                id: Uuid::new_v4(),
                source_node,
                source_pin: source_pin_id,
                target_node,
                target_pin: target_pin_id,
            });
        }
    }

    // Top-level data wires plus exec edges.
    for wire in data_wires {
        let Statement::DataWire {
            target,
            source,
            line,
            col,
        } = wire
        else {
            unreachable!("data_wires only contains DataWire statements")
        };
        let (source_node, source_pin_id) =
            src_pin_of.get(&(source.0.as_str(), source.1.as_str())).cloned().ok_or_else(|| {
                SharedError::Invalid(format!(
                    "unknown data source '{}.{}' (line {line}, column {col})",
                    source.0, source.1
                ))
            })?;
        let target_node = *node_id_of
            .get(target.0.as_str())
            .ok_or_else(|| SharedError::Invalid(format!("unknown node '{}'", target.0)))?;
        let target_pin_id = find_data_input(&blueprint, target_node, &target.1).map_err(|_| {
            SharedError::Invalid(format!(
                "node '{}' has no data input pin '{}' (line {line}, column {col})",
                target.0, target.1
            ))
        })?;
        blueprint.edges.push(Edge {
            id: Uuid::new_v4(),
            source_node,
            source_pin: source_pin_id,
            target_node,
            target_pin: target_pin_id,
        });
    }
    for edge in exec_edges {
        let Statement::ExecEdge {
            source,
            source_pin,
            target,
            line,
            col,
        } = edge
        else {
            unreachable!("exec_edges only contains ExecEdge statements")
        };
        let source_node = *node_id_of.get(source.as_str()).ok_or_else(|| {
            SharedError::Invalid(format!("unknown node '{source}' (line {line}, column {col})"))
        })?;
        let source_pin_id = {
            let node = blueprint.nodes.iter().find(|n| n.id == source_node).unwrap();
            match source_pin {
                Some(pin_name) => node
                    .pins
                    .iter()
                    .find(|p| p.name == *pin_name && p.pin_type == PinType::ExecOutput)
                    .map(|p| p.id)
                    .ok_or_else(|| {
                        SharedError::Invalid(format!(
                            "node '{source}' has no exec output '{pin_name}' (line {line}, column {col})"
                        ))
                    })?,
                None => node
                    .pins
                    .iter()
                    .find(|p| p.pin_type == PinType::ExecOutput)
                    .map(|p| p.id)
                    .ok_or_else(|| {
                        SharedError::Invalid(format!(
                            "node '{source}' has no exec output (line {line}, column {col})"
                        ))
                    })?,
            }
        };
        let target_node = *node_id_of.get(target.as_str()).ok_or_else(|| {
            SharedError::Invalid(format!("unknown node '{target}' (line {line}, column {col})"))
        })?;
        let target_pin_id = blueprint
            .nodes
            .iter()
            .find(|n| n.id == target_node)
            .and_then(|n| n.pins.iter().find(|p| p.pin_type == PinType::ExecInput))
            .map(|p| p.id)
            .ok_or_else(|| {
                SharedError::Invalid(format!(
                    "node '{target}' has no exec input (line {line}, column {col})"
                ))
            })?;
        blueprint.edges.push(Edge {
            id: Uuid::new_v4(),
            source_node,
            source_pin: source_pin_id,
            target_node,
            target_pin: target_pin_id,
        });
    }
    // Lay the graph out along the exec flow so imported blueprints read as a
    // cascade instead of one overlapping row.
    lay_out(&mut blueprint);
    Ok(blueprint)
}

/// Positions nodes in a layered grid: columns are exec-flow depth from the
/// entry node, rows keep the declaration order within a column.
fn lay_out(bp: &mut Blueprint) {
    let exec_next: Vec<(Uuid, Uuid)> = bp
        .edges
        .iter()
        .filter_map(|e| {
            let src = bp.nodes.iter().find(|n| n.id == e.source_node)?;
            let dst = bp.nodes.iter().find(|n| n.id == e.target_node)?;
            let is_exec_out =
                src.pins.iter().any(|p| p.id == e.source_pin && p.pin_type == PinType::ExecOutput);
            let is_exec_in =
                dst.pins.iter().any(|p| p.id == e.target_pin && p.pin_type == PinType::ExecInput);
            (is_exec_out && is_exec_in).then_some((e.source_node, e.target_node))
        })
        .collect();

    let mut layer_of: HashMap<Uuid, u32> = HashMap::new();
    let mut queue = VecDeque::new();
    layer_of.insert(bp.entry_node_id, 0);
    queue.push_back(bp.entry_node_id);
    while let Some(nid) = queue.pop_front() {
        let cur = layer_of[&nid];
        for &(s, t) in &exec_next {
            if s == nid && !layer_of.contains_key(&t) {
                layer_of.insert(t, cur + 1);
                queue.push_back(t);
            }
        }
    }
    // Nodes not reachable through the exec flow (data-only parts, detached
    // graphs) continue after the deepest exec layer.
    let mut fallback = layer_of.len() as u32;
    for n in &bp.nodes {
        layer_of.entry(n.id).or_insert_with(|| {
            fallback += 1;
            fallback - 1
        });
    }

    let mut column: HashMap<u32, u32> = HashMap::new();
    for n in &mut bp.nodes {
        let layer = layer_of[&n.id];
        let row = column.entry(layer).or_insert(0);
        n.position = (layer as f32 * 260.0 + 40.0, *row as f32 * 170.0 + 60.0);
        *row += 1;
    }
}

/// Reads a data input pin by name, verifying it is a `DataInput`.
fn find_data_input(bp: &Blueprint, node_id: Uuid, pin_name: &str) -> SharedResult<Uuid> {
    bp.nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| {
            n.pins.iter().find(|p| p.name == pin_name && p.pin_type == PinType::DataInput)
        })
        .map(|p| p.id)
        .ok_or_else(|| SharedError::Invalid(format!("node has no data input pin '{pin_name}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::decompile;

    /// A context-manager graph as exported by the canvas must recompile and
    /// keep its Context wiring (regression: the template table named context
    /// pins literally `DataType::Context` and Start lacked the Context output).
    #[test]
    fn context_blueprint_round_trips() {
        let source = r#"blueprint "abc.blueprint"
entry n0: Start
n1: End
n2: CallLLM
n3: ContextMerge
n4: ContextTrim
n2.x-out -> n3
$n3.Text <- n2.Result
$n3.Context <- n2.Context
n0.x-out -> n2
$n2.Context <- n0.Context
$n4.Context <- n3.Result
n3.x-out -> n4
n4.x-out -> n1
"#;
        let bp = compile(source).expect("canvas-derived DSL must compile");
        assert_eq!(bp.entry_node_id, bp.nodes[0].id);
        let out = decompile(&bp);
        assert!(out.contains("$n2.Context <- n0.Context"), "{out}");
        assert!(out.contains("$n4.Context <- n3.Result"), "{out}");
    }

    /// Exec pins use the canvas `x-in`/`x-out` names, so exported edges with
    /// explicit exec suffixes resolve after a round trip.
    #[test]
    fn exec_suffix_round_trips() {
        let source = "entry n0: Start\nn1: Add\nn2: End\nn0.x-out -> n1\nn1.x-out -> n2\n";
        let bp = compile(source).expect("suffixed exec edges must compile");
        let out = decompile(&bp);
        assert!(out.contains("n0.x-out -> n1"), "{out}");
    }

    /// Inline node constants (literals) survive a compile -> decompile round
    /// trip, so values filled in the inspector are not lost on export. Both the
    /// paren form and the `{ key: value }` block form compile; constants are
    /// stored (and re-emitted) under the pin's canonical key.
    #[test]
    fn constants_survive_decompile() {
        let source = "entry n0: Add(A = 8, B = 3)\nn1: End\nn0.x-out -> n1\n";
        let bp = compile(source).expect("constants must compile");
        assert_eq!(bp.nodes[0].data.get("a"), Some(&serde_json::json!(8)));
        let out = decompile(&bp);
        assert!(out.contains("a: 8"), "{out}");
        assert!(out.contains("b: 3"), "{out}");
    }

    /// The init-block syntax (`n: Kind { key: literal }`) stores node constants
    /// and survives a decompile round trip as the same block.
    #[test]
    fn init_block_stores_constants() {
        let source = "entry n0: CallLLM {\n  max_tokens: 256000\n  prompt: \"测试\"\n}\nn1: End\nn0.x-out -> n1\n";
        let bp = compile(source).expect("init block must compile");
        let node = &bp.nodes[0];
        assert_eq!(node.data.get("max_tokens"), Some(&serde_json::json!(256000)));
        assert_eq!(node.data.get("prompt"), Some(&serde_json::json!("测试")));
        let out = decompile(&bp);
        assert!(out.contains("max_tokens: 256000"), "{out}");
        assert!(out.contains("prompt: \"测试\""), "{out}");
    }

    /// Positions follow the exec flow: deeper layers move right, siblings in
    /// the same layer stack vertically instead of overlapping.
    #[test]
    fn layout_follows_exec_flow() {
        let source = "\
entry n0: Start
n1: Add
n2: Length
n3: End
n0.x-out -> n1
n0.x-out -> n2
n1.x-out -> n3
n2.x-out -> n3
";
        let bp = compile(source).expect("branching graph must compile");
        let by = |kind: &str| bp.nodes.iter().find(|n| n.kind == kind).expect(kind);
        assert!(by("Start").position.0 < by("Add").position.0);
        assert!(by("Add").position.0 < by("End").position.0);
        // Layer-1 siblings (Add, Length) occupy distinct rows.
        assert_ne!(by("Add").position.1, by("Length").position.1);
    }
}
