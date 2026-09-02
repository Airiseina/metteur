//! Compiles parsed statements into a [`Blueprint`].
//!
//! Node pin layouts come from a built-in template table (the authoritative
//! source is the daemon registry; the table only covers the core node kinds).
//! Errors carry the source line/column recorded by the parser.

use std::collections::HashMap;

use uuid::Uuid;

use crate::error::{SharedError, SharedResult};
use crate::model::blueprint::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};

use super::parser::Statement;

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

/// Compiles `source` into a blueprint with fresh UUIDs.
pub fn compile(source: &str) -> SharedResult<Blueprint> {
    let statements = super::parser::parse(source)?;
    let mut name = "Untitled".to_string();
    let mut nodes: Vec<&Statement> = Vec::new();
    let mut exec_edges: Vec<&Statement> = Vec::new();
    let mut data_wires: Vec<&Statement> = Vec::new();

    for stmt in &statements {
        match stmt {
            Statement::Header { name: n } => name = n.clone(),
            Statement::Node { .. } => nodes.push(stmt),
            Statement::ExecEdge { .. } => exec_edges.push(stmt),
            Statement::DataWire { .. } => data_wires.push(stmt),
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
        .find(|s| matches!(s, Statement::Node { is_entry: true, .. }))
        .unwrap_or(nodes[0]);
    let entry_alias = match entry {
        Statement::Node { alias, .. } => alias.as_str(),
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
            Statement::Node { alias, .. } => (alias.as_str(), Uuid::new_v4()),
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
            let pin = Pin {
                id: Uuid::new_v4(),
                name: p.name.to_string(),
                pin_type: p.pin_type,
                data_type: p.data_type,
            };
            if p.pin_type == PinType::DataOutput {
                src_pin_of.insert((alias.as_str(), p.name), (node_id, pin.id));
            }
            pins.push(pin);
        }
        let mut data = serde_json::Map::new();
        for (k, v) in constants {
            data.insert(k.clone(), v.clone());
        }
        blueprint.nodes.push(Node {
            id: node_id,
            node_type: match kind.as_str() {
                "Start" | "End" | "FunctionEntry" | "FunctionExit" => NodeType::Event,
                "Branch" => NodeType::Control,
                "Add" | "Subtract" | "Multiply" | "Divide" => NodeType::Pure,
                _ => NodeType::Function,
            },
            kind: kind.clone(),
            position: (blueprint.nodes.len() as f32 * 180.0 + 40.0, 220.0),
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
        let (source_node, source_pin_id) = src_pin_of
            .get(&(source.0.as_str(), source.1.as_str()))
            .cloned()
            .ok_or_else(|| {
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
        let source_node = *node_id_of
            .get(source.as_str())
            .ok_or_else(|| SharedError::Invalid(format!("unknown node '{source}' (line {line}, column {col})")))?;
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
        let target_node = *node_id_of
            .get(target.as_str())
            .ok_or_else(|| SharedError::Invalid(format!("unknown node '{target}' (line {line}, column {col})")))?;
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