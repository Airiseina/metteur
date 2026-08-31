//! A human-friendly DSL that compiles into [`Blueprint`] JSON.
//!
//! The DSL is line-oriented; `#` starts a comment and arguments may span
//! lines inside parentheses:
//!
//! ```text
//! blueprint "MyFlow"
//! entry start: Start(A = 4, B = 3)
//! sum: Add(A <- start.A, B <- start.B)
//! verify: Validator(Actual <- sum.Result, mode = "gte", Expected = 6)
//! start -> sum
//! sum -> verify
//! ```
//!
//! Node pin layouts come from a built-in template table (the authoritative
//! source is the daemon registry; the table only covers the core node kinds).

use std::collections::HashMap;

use uuid::Uuid;

use crate::error::{SharedError, SharedResult};
use crate::model::blueprint::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};

/// Maximum nodes a compiled blueprint may contain (arbitrary safety limit).
const MAX_NODES: usize = 256;

/// A pin template entry.
struct PinSpec {
    name: &'static str,
    pin_type: PinType,
    data_type: DataType,
}

/// Returns the pin layout for a known node kind, if any.
fn template(kind: &str) -> Option<Vec<PinSpec>> {
    let i = |n: &'static str| PinSpec {
        name: n,
        pin_type: PinType::ExecInput,
        data_type: DataType::Void,
    };
    let o = |n: &'static str| PinSpec {
        name: n,
        pin_type: PinType::ExecOutput,
        data_type: DataType::Void,
    };
    let di = |n: &'static str, dt: DataType| PinSpec {
        name: n,
        pin_type: PinType::DataInput,
        data_type: dt,
    };
    let do_ = |n: &'static str, dt: DataType| PinSpec {
        name: n,
        pin_type: PinType::DataOutput,
        data_type: dt,
    };
    let exec = |exec_in: bool| {
        let mut pins = Vec::new();
        if exec_in {
            pins.push(i("Exec"));
            pins.push(o("Exec"));
        }
        pins
    };
    Some(match kind {
        "Start" => vec![o("Exec"), do_("A", DataType::Json), do_("B", DataType::Json)],
        "End" => vec![i("Exec")],
        "Add" | "Subtract" | "Multiply" | "Divide" => {
            let mut pins = exec(true);
            pins.push(di("A", DataType::Float));
            pins.push(di("B", DataType::Float));
            pins.push(do_("Result", DataType::Float));
            pins
        }
        "Branch" => {
            let mut pins = vec![i("Exec")];
            pins.push(PinSpec {
                name: "True",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(PinSpec {
                name: "False",
                pin_type: PinType::ExecOutput,
                data_type: DataType::Void,
            });
            pins.push(di("Condition", DataType::Json));
            pins.push(do_("Result", DataType::Bool));
            pins
        }
        "CallLLM" => {
            let mut pins = exec(true);
            pins.push(do_("Result", DataType::String));
            pins.push(di("Context", DataType::Json));
            pins.push(do_("Context", DataType::Json));
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
        "Tool" => {
            let mut pins = exec(true);
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

/// A parsed node statement.
struct NodeStmt {
    alias: String,
    kind: String,
    /// `(pin name, literal value)` constants written into `data`.
    constants: Vec<(String, serde_json::Value)>,
    /// `(pin name, source "alias.pin")` data edges.
    refs: Vec<(String, String)>,
    is_entry: bool,
}

/// Compiles `source` into a blueprint with fresh UUIDs.
pub fn compile(source: &str) -> SharedResult<Blueprint> {
    let statements = split_statements(source)?;
    let mut name = "Untitled".to_string();
    let mut nodes: Vec<NodeStmt> = Vec::new();
    let mut exec_edges: Vec<(String, String)> = Vec::new();
    let mut data_edges: Vec<(String, String)> = Vec::new();

    for stmt in &statements {
        if let Some(n) = stmt.strip_prefix("blueprint") {
            name = parse_blueprint_name(n)?;
        } else if let Some(entry) = stmt.strip_prefix("entry") {
            nodes.push(parse_node(entry, true)?);
        } else if let Some(rest) = stmt.strip_prefix('$') {
            // data wiring: alias.pin <- alias.pin
            data_edges.push(parse_data_wire(rest)?);
        } else if stmt.contains(" -> ") {
            let parts: Vec<&str> = stmt.split(" -> ").map(str::trim).collect();
            if parts.len() != 2 {
                return Err(SharedError::Invalid(format!(
                    "invalid edge '{stmt}'"
                )));
            }
            exec_edges.push((parts[0].to_string(), parts[1].to_string()));
        } else if stmt.contains(':') {
            nodes.push(parse_node(stmt, false)?);
        } else {
            return Err(SharedError::Invalid(format!("unrecognized statement '{stmt}'")));
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
        .find(|n| n.is_entry)
        .or_else(|| nodes.first())
        .ok_or_else(|| SharedError::Invalid("no entry node".to_string()))?;

    let mut blueprint = Blueprint {
        id: Uuid::new_v4(),
        name,
        nodes: Vec::new(),
        edges: Vec::new(),
        entry_node_id: Uuid::nil(),
    };
    let node_id_of: HashMap<&str, Uuid> = nodes
        .iter()
        .map(|n| (n.alias.as_str(), Uuid::new_v4()))
        .collect();
    let mut src_pin_of: HashMap<(&str, &str), (Uuid, Uuid)> = HashMap::new(); // (alias, pin) -> (node, pin)

    for stmt in &nodes {
        let node_id = node_id_of[stmt.alias.as_str()];
        if stmt.alias == entry.alias {
            blueprint.entry_node_id = node_id;
        }
        let spec = template(&stmt.kind).ok_or_else(|| {
            SharedError::Invalid(format!(
                "unknown node kind '{}' (line for '{}'); use a JSON blueprint instead",
                stmt.kind, stmt.alias
            ))
        })?;
        let mut pins = Vec::new();
        for p in &spec {
            let pin = Pin {
                id: Uuid::new_v4(),
                name: p.name.to_string(),
                pin_type: p.pin_type,
                data_type: p.data_type,
            };
            if p.pin_type == PinType::DataOutput {
                src_pin_of.insert((stmt.alias.as_str(), p.name), (node_id, pin.id));
            }
            pins.push(pin);
        }
        let mut data = serde_json::Map::new();
        for (k, v) in &stmt.constants {
            data.insert(k.clone(), v.clone());
        }
        blueprint.nodes.push(Node {
            id: node_id,
            node_type: match stmt.kind.as_str() {
                "Start" | "End" | "FunctionEntry" | "FunctionExit" => NodeType::Event,
                "Branch" => NodeType::Control,
                "Add" | "Subtract" | "Multiply" | "Divide" => NodeType::Pure,
                _ => NodeType::Function,
            },
            kind: stmt.kind.clone(),
            position: (blueprint.nodes.len() as f32 * 180.0 + 40.0, 220.0),
            pins,
            data: serde_json::Value::Object(data),
        });
    }

    // Data refs: target pin by name on the target node.
    for stmt in &nodes {
        for (target_pin, source) in &stmt.refs {
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
            let target_node = node_id_of[stmt.alias.as_str()];
            let target_pin_id = blueprint
                .nodes
                .iter()
                .find(|n| n.id == target_node)
                .and_then(|n| n.pins.iter().find(|p| p.name == *target_pin))
                .map(|p| p.id)
                .filter(|_| {
                    blueprint
                        .nodes
                        .iter()
                        .find(|n| n.id == target_node)
                        .is_some_and(|n| {
                            n.pins.iter().any(|p| p.name == *target_pin && p.pin_type == PinType::DataInput)
                        })
                })
                .ok_or_else(|| {
                    SharedError::Invalid(format!(
                        "node '{}' has no data input pin '{target_pin}'",
                        stmt.alias
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
    }

    // Data wires + exec edges.
    for (lhs, rhs) in &data_edges {
        let (target_alias, target_pin) = lhs
            .split_once('.')
            .ok_or_else(|| SharedError::Invalid(format!("invalid wire '{lhs}'")))?;
        let (source_alias, source_pin) = rhs
            .split_once('.')
            .ok_or_else(|| SharedError::Invalid(format!("invalid wire '{rhs}'")))?;
        let (source_node, source_pin_id) = src_pin_of
            .get(&(source_alias, source_pin))
            .cloned()
            .ok_or_else(|| SharedError::Invalid(format!("unknown data source '{rhs}'")))?;
        let target_node = node_id_of[target_alias];
        let target_pin_id = find_data_input(&blueprint, target_node, target_pin)?;
        blueprint.edges.push(Edge {
            id: Uuid::new_v4(),
            source_node,
            source_pin: source_pin_id,
            target_node,
            target_pin: target_pin_id,
        });
    }
    for (src, dst) in &exec_edges {
        let (src_alias, src_pin) = match src.split_once('.') {
            Some((a, p)) => (a, Some(p)),
            None => (src.as_str(), None),
        };
        let source_node = *node_id_of
            .get(src_alias)
            .ok_or_else(|| SharedError::Invalid(format!("unknown node '{src_alias}'")))?;
        let source_pin_id = {
            let node = blueprint.nodes.iter().find(|n| n.id == source_node).unwrap();
            match src_pin {
                Some(pin_name) => node
                    .pins
                    .iter()
                    .find(|p| p.name == pin_name && p.pin_type == PinType::ExecOutput)
                    .map(|p| p.id)
                    .ok_or_else(|| {
                        SharedError::Invalid(format!("node '{src_alias}' has no exec output '{pin_name}'"))
                    })?,
                None => node
                    .pins
                    .iter()
                    .find(|p| p.pin_type == PinType::ExecOutput)
                    .map(|p| p.id)
                    .ok_or_else(|| {
                        SharedError::Invalid(format!("node '{src_alias}' has no exec output"))
                    })?,
            }
        };
        let target_node = *node_id_of
            .get(dst.as_str())
            .ok_or_else(|| SharedError::Invalid(format!("unknown node '{dst}'")))?;
        let target_pin_id = blueprint
            .nodes
            .iter()
            .find(|n| n.id == target_node)
            .and_then(|n| n.pins.iter().find(|p| p.pin_type == PinType::ExecInput))
            .map(|p| p.id)
            .ok_or_else(|| SharedError::Invalid(format!("node '{dst}' has no exec input")))?;
        blueprint.edges.push(Edge {
            id: Uuid::new_v4(),
            source_node,
            source_pin: source_pin_id,
            target_node,
            target_pin: target_pin_id,
        });
    }
    Ok(blueprint)
}

/// Reads a data input pin by name, verifying it is a `DataInput`.
fn find_data_input(bp: &Blueprint, node_id: Uuid, pin_name: &str) -> SharedResult<Uuid> {
    bp.nodes
        .iter()
        .find(|n| n.id == node_id)
        .and_then(|n| {
            n.pins
                .iter()
                .find(|p| p.name == pin_name && p.pin_type == PinType::DataInput)
        })
        .map(|p| p.id)
        .ok_or_else(|| SharedError::Invalid(format!("node has no data input pin '{pin_name}'")))
}

/// Splits source into logical statements, joining lines inside parentheses.
fn split_statements(source: &str) -> SharedResult<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let is_code = current.is_empty();
        if is_code && !looks_like_new_stmt(line) {
            return Err(SharedError::Invalid(format!(
                "unexpected content on line {}: '{line}'",
                idx + 1
            )));
        }
        current.push_str(line);
        depth += line.matches('(').count();
        depth = depth.saturating_sub(line.matches(')').count());
        if depth == 0 {
            out.push(std::mem::take(&mut current));
        }
    }
    if depth > 0 {
        return Err(SharedError::Invalid("unbalanced parentheses".to_string()));
    }
    Ok(out)
}

/// Whether a line may start a new statement.
fn looks_like_new_stmt(line: &str) -> bool {
    line.starts_with("blueprint")
        || line.starts_with("entry ")
        || line.starts_with('$')
        || line.contains(" -> ")
        || line.contains(':')
}

/// Parses a `blueprint "Name"` header.
fn parse_blueprint_name(rest: &str) -> SharedResult<String> {
    let name = rest.trim().trim_matches('"').trim().to_string();
    if name.is_empty() {
        return Err(SharedError::Invalid("blueprint name is empty".to_string()));
    }
    Ok(name)
}

/// Parses `alias: Kind(args)` or a `entry alias: Kind(args)` suffix.
fn parse_node(rest: &str, is_entry: bool) -> SharedResult<NodeStmt> {
    let (alias, kind_and_args) = rest
        .split_once(':')
        .ok_or_else(|| SharedError::Invalid(format!("expected 'alias: Kind(...)', got '{rest}'")))?;
    let alias = alias.trim().to_string();
    let trimmed = kind_and_args.trim();
    let (kind, args) = match trimmed.find('(') {
        Some(idx) => {
            let kind = trimmed[..idx].trim().to_string();
            let rest = trimmed[idx + 1..].trim();
            if !rest.ends_with(')') {
                return Err(SharedError::Invalid(format!(
                    "expected 'Kind(...)' for node '{alias}'"
                )));
            }
            let args = rest[..rest.len() - 1].trim();
            (kind, args)
        }
        None => (trimmed.to_string(), ""),
    };
    let mut node = NodeStmt {
        alias,
        kind,
        constants: Vec::new(),
        refs: Vec::new(),
        is_entry,
    };
    for arg in args.split(',') {
        let arg = arg.trim();
        if arg.is_empty() {
            continue;
        }
        if let Some((pin, value)) = arg.split_once("<-") {
            let pin = pin.trim().to_string();
            if pin.is_empty() {
                return Err(SharedError::Invalid(format!("data ref with empty pin in '{arg}'")));
            }
            node.refs.push((pin, value.trim().to_string()));
        } else if let Some((pin, value)) = arg.split_once('=') {
            node.constants
                .push((pin.trim().to_string(), parse_literal(value.trim())?));
        } else {
            return Err(SharedError::Invalid(format!("invalid argument '{arg}'")));
        }
    }
    Ok(node)
}

/// Parses `alias.pin <- alias.pin` into `(target, source)`.
fn parse_data_wire(rest: &str) -> SharedResult<(String, String)> {
    let (lhs, rhs) = rest
        .split_once("<-")
        .ok_or_else(|| SharedError::Invalid(format!("expected '<-', got '{rest}'")))?;
    Ok((lhs.trim().to_string(), rhs.trim().to_string()))
}

/// Parses JSON-ish literals (numbers, strings, booleans, arrays, objects).
fn parse_literal(text: &str) -> SharedResult<serde_json::Value> {
    if text.starts_with('"') {
        return serde_json::from_str(text)
            .map_err(|e| SharedError::Invalid(format!("invalid string literal: {e}")));
    }
    if text == "true" {
        return Ok(serde_json::json!(true));
    }
    if text == "false" {
        return Ok(serde_json::json!(false));
    }
    if text.starts_with('[') || text.starts_with('{') {
        return serde_json::from_str(text)
            .map_err(|e| SharedError::Invalid(format!("invalid literal: {e}")));
    }
    if let Ok(n) = text.parse::<i64>() {
        return Ok(serde_json::json!(n));
    }
    if let Ok(f) = text.parse::<f64>() {
        return Ok(serde_json::json!(f));
    }
    Err(SharedError::Invalid(format!("unrecognized literal '{text}'")))
}

/// Renders a blueprint back to DSL text (best effort).
pub fn decompile(blueprint: &Blueprint) -> String {
    let mut out = String::new();
    out.push_str(&format!("blueprint \"{}\"\n", blueprint.name));
    let node_of = |id: Uuid| blueprint.nodes.iter().find(|n| n.id == id).unwrap();
    let entry = node_of(blueprint.entry_node_id);
    for (i, node) in blueprint.nodes.iter().enumerate() {
        let lit = |pin: &Pin| -> Option<String> {
            node.data
                .get(&pin.name)
                .and_then(|v| if v.is_null() { None } else { Some(v.to_string()) })
        };
        let args: Vec<String> = node
            .pins
            .iter()
            .filter(|p| p.pin_type == PinType::DataInput)
            .filter_map(|p| lit(p).map(|v| format!("{} = {v}", p.name)))
            .collect();
        let head = if node.id == entry.id {
            format!("entry n{i}: {}", node.kind)
        } else {
            format!("n{i}: {}", node.kind)
        };
        if args.is_empty() {
            out.push_str(&head);
        } else {
            out.push_str(&format!("{head}({})", args.join(", ")));
        }
        out.push('\n');
    }
    for edge in &blueprint.edges {
        let src = node_of(edge.source_node);
        let dst = node_of(edge.target_node);
        let src_idx = blueprint.nodes.iter().position(|n| n.id == src.id).unwrap();
        let dst_idx = blueprint.nodes.iter().position(|n| n.id == dst.id).unwrap();
        let src_pin = src.pins.iter().find(|p| p.id == edge.source_pin).unwrap();
        if src_pin.pin_type == PinType::ExecOutput {
            let pin = if src_pin.name == "Exec" {
                String::new()
            } else {
                format!(".{}", src_pin.name)
            };
            out.push_str(&format!("n{src_idx}{pin} -> n{dst_idx}\n"));
        } else {
            let dst_pin = dst.pins.iter().find(|p| p.id == edge.target_pin).unwrap();
            out.push_str(&format!(
                "$n{dst_idx}.{} <- n{src_idx}.{}\n",
                dst_pin.name, src_pin.name
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // The validator expected the wiring let the run complete.
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
    }

    #[test]
    fn unbalanced_parens_are_rejected() {
        assert!(compile("entry a: Start(").is_err());
    }
}