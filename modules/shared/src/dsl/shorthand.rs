//! Compiles a compact JSON *draft* into a blueprint.
//!
//! The canvas format is verbose: every pin carries an id, every node a UUID
//! and an explicit position, and data flow is expressed as separate edge
//! objects. That is fine for a GUI but wasteful for an LLM authoring a plan
//! token by token, and it invites mistakes (duplicated ids, dangling pin
//! references, overlapping nodes).
//!
//! The shorthand keeps what a plan actually needs to say — which steps exist,
//! how control flows, and where values come from — and derives everything else:
//!
//! ```json
//! {
//!   "name": "Add a flag",
//!   "nodes": {
//!     "start": { "kind": "Start" },
//!     "read":  { "kind": "ReadFile", "path": "src/main.rs" },
//!     "patch": { "kind": "EditFile", "path": "src/main.rs",
//!                "edits": [{ "old_string": "a", "new_string": "b" }] },
//!     "check": { "kind": "LspCheck", "path": "src/main.rs" }
//!   },
//!   "flow": ["start -> read -> patch -> check"]
//! }
//! ```
//!
//! Expansion is deterministic: pin ids, node ids and positions are all derived
//! from the draft, so the same draft always yields the same graph. Keys inside
//! a node object become its constants (the inspector's inline values), a
//! string value written as `$alias.pin` becomes a data wire, and `flow` turns
//! into execution edges.
//!
//! Why not the text DSL: it is designed for humans and its syntax is not in
//! any model's training data, so an LLM mis-writes it far more often than
//! JSON. The draft keeps JSON's familiarity while removing its verbosity; see
//! `Metteur.md` ("蓝图 DSL" — the DSL is explicitly not meant for LLM authors).

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::{SharedError, SharedResult};
use crate::model::{Blueprint, DataType, Edge, Node, NodeId, Pin, PinId, PinType};

use super::compile::{REGISTRY_TOOL_KINDS, node_type_of, template};

/// Maximum nodes a compiled blueprint may contain (mirrors the DSL limit).
const MAX_NODES: usize = 256;

/// Horizontal spacing between layout layers.
const LAYER_X: f32 = 260.0;

/// Vertical spacing between nodes inside a layer.
const LAYER_Y: f32 = 150.0;

/// Keys of a node object that describe the node rather than configure it.
const RESERVED_KEYS: &[&str] = &["kind", "at", "args"];

/// Compiles a draft (JSON text) into a blueprint.
pub fn compile_draft(source: &str) -> SharedResult<Blueprint> {
    let value: serde_json::Value = serde_json::from_str(source)
        .map_err(|err| SharedError::Invalid(format!("draft is not valid JSON: {err}")))?;
    compile_draft_value(&value)
}

/// Compiles a draft from an already-parsed JSON value.
pub fn compile_draft_value(value: &serde_json::Value) -> SharedResult<Blueprint> {
    let object = value
        .as_object()
        .ok_or_else(|| SharedError::Invalid("draft must be a JSON object".to_string()))?;

    let name = object
        .get("name")
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("Untitled")
        .to_string();
    let entry_alias = object.get("entry").and_then(|value| value.as_str()).map(str::to_string);

    let entries = collect_nodes(object.get("nodes"))?;
    if entries.is_empty() {
        return Err(SharedError::Invalid("draft has no nodes".to_string()));
    }
    if entries.len() > MAX_NODES {
        return Err(SharedError::Invalid(format!(
            "draft has too many nodes ({} > {MAX_NODES})",
            entries.len()
        )));
    }

    // Stable node ids: derived from the alias so repeated compiles of the same
    // draft produce the same ids (the canvas keys off them).
    let node_ids: HashMap<&str, NodeId> =
        entries.iter().map(|(alias, _)| (alias.as_str(), stable_id(alias))).collect();

    let mut blueprint = Blueprint {
        id: stable_id(&name),
        name,
        nodes: Vec::new(),
        edges: Vec::new(),
        entry_node_id: NodeId::nil(),
    };
    let mut src_pins: HashMap<(String, String), (NodeId, PinId)> = HashMap::new();
    let mut pending_wires: Vec<PendingWire> = Vec::new();

    for (alias, draft) in &entries {
        let node_id = node_ids[alias.as_str()];
        let kind = draft
            .get("kind")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                SharedError::Invalid(format!("node '{alias}' is missing a `kind`"))
            })?;
        let spec = template(kind).ok_or_else(|| {
            SharedError::Invalid(format!(
                "unknown node kind '{kind}' (node '{alias}'){}",
                suggest_kind(kind)
            ))
        })?;

        let mut pins = Vec::new();
        for entry in &spec {
            let mut pin =
                Pin::data(entry.name.to_string(), entry.pin_type, entry.data_type.clone(), stable_id(&format!("{alias}.{}", entry.name)));
            if !entry.key.is_empty() {
                pin.key = Some(entry.key.to_string());
            }
            if entry.pin_type == PinType::DataOutput {
                src_pins.insert(
                    (alias.clone(), entry.name.to_string()),
                    (node_id, pin.id),
                );
                // Outputs may also be referenced by their semantic key.
                if !entry.key.is_empty() {
                    src_pins
                        .entry((alias.clone(), entry.key.to_string()))
                        .or_insert((node_id, pin.id));
                }
            }
            pins.push(pin);
        }

        let mut data = serde_json::Map::new();
        for (key, value) in draft {
            if RESERVED_KEYS.contains(&key.as_str()) {
                continue;
            }
            // A `$alias.pin` string is a wire, not a constant: wiring it in as
            // a literal would inject a dangling reference into node data.
            if let Some(reference) = value.as_str().and_then(parse_reference) {
                pending_wires.push(PendingWire {
                    target: (alias.clone(), key.clone()),
                    reference: format!("{}.{}", reference.0, reference.1),
                });
                continue;
            }
            let canonical = spec
                .iter()
                .find(|entry| entry.name == key || entry.key == key)
                .map(|entry| entry.key)
                .filter(|key| !key.is_empty())
                .unwrap_or(key.as_str());
            data.insert(canonical.to_string(), value.clone());
        }

        // `Start` carries ad-hoc initial attributes: a constant with no
        // matching template pin becomes a data output so a downstream node can
        // read it as `$start.<name>`. The Start executor emits values by pin
        // name, so this matches how the canvas and the text DSL treat it.
        if kind == "Start" {
            for (key, _) in &data {
                let known = pins.iter().any(|pin| {
                    pin.name == *key || pin.key.as_deref() == Some(key.as_str())
                });
                if known {
                    continue;
                }
                let pin_id = stable_id(&format!("{alias}.{key}"));
                pins.push(Pin {
                    id: pin_id,
                    key: Some(key.clone()),
                    name: key.clone(),
                    pin_type: PinType::DataOutput,
                    data_type: DataType::Any,
                    ..Default::default()
                });
                src_pins.insert((alias.clone(), key.clone()), (node_id, pin_id));
            }
        }

        // A generic `Tool` node has no per-argument pins, so `args` declares
        // them explicitly. This is how an MCP or addon tool (whose argument
        // names the draft author must be told) becomes usable.
        if kind == "Tool"
            && let Some(args) = draft.get("args").and_then(|value| value.as_object())
        {
            for (arg, value) in args {
                if let Some(reference) = value.as_str().and_then(parse_reference) {
                    pending_wires.push(PendingWire {
                        target: (alias.clone(), arg.clone()),
                        reference: format!("{}.{}", reference.0, reference.1),
                    });
                } else {
                    data.insert(arg.clone(), value.clone());
                }
                if pins
                    .iter()
                    .any(|pin| pin.name == *arg && pin.pin_type == PinType::DataInput)
                {
                    continue;
                }
                pins.push(Pin::data(
                    arg.clone(),
                    PinType::DataInput,
                    DataType::Any,
                    stable_id(&format!("{alias}.{arg}")),
                ));
            }
        }

        blueprint.nodes.push(Node {
            id: node_id,
            node_type: node_type_of(kind),
            kind: {
                // A named registry tool compiles to a `Tool` node carrying its
                // own name, exactly as the text DSL does.
                if REGISTRY_TOOL_KINDS.contains(&kind) {
                    data.insert(
                        "tool_name".to_string(),
                        serde_json::Value::String(kind.to_string()),
                    );
                    "Tool".to_string()
                } else {
                    kind.to_string()
                }
            },
            position: (0.0, 0.0),
            pins,
            data: serde_json::Value::Object(data),
        });
    }

    // Entry: an explicit `entry`, else a Start node, else the first node.
    blueprint.entry_node_id = match &entry_alias {
        Some(alias) => *node_ids
            .get(alias.as_str())
            .ok_or_else(|| SharedError::Invalid(format!("entry '{alias}' is not a declared node")))?,
        None => entries
            .iter()
            .find(|(_, draft)| draft.get("kind").and_then(|k| k.as_str()) == Some("Start"))
            .map(|(alias, _)| node_ids[alias.as_str()])
            .unwrap_or_else(|| node_ids[entries[0].0.as_str()]),
    };

    // Data wires, from `$alias.pin` values and the `wires` list.
    let mut all_wires = pending_wires;
    all_wires.extend(parse_wire_list(object.get("wires"))?);
    for wire in all_wires {
        add_data_wire(&mut blueprint, &node_ids, &src_pins, &wire)?;
    }

    // Execution edges from the `flow` list.
    for edge in parse_flow(object.get("flow"))? {
        add_exec_edge(&mut blueprint, &node_ids, &edge)?;
    }

    let entry = blueprint.entry_node_id;
    layout(&mut blueprint, entry);
    Ok(blueprint)
}

/// A data wire waiting for its endpoints to be resolved.
struct PendingWire {
    /// Target node alias and input pin name.
    target: (String, String),
    /// Source reference (`alias.pin`).
    reference: String,
}

/// A parsed execution edge.
struct ExecEdge {
    /// Source alias and optional output pin name.
    source: (String, Option<String>),
    /// Target alias.
    target: String,
    /// Source text, for error messages.
    spelled: String,
}

/// Collects the node map, accepting both the compact object form and an array
/// of `{ "id": ..., "kind": ... }` objects (some authors emit the latter).
fn collect_nodes(
    value: Option<&serde_json::Value>,
) -> SharedResult<Vec<(String, serde_json::Map<String, serde_json::Value>)>> {
    let value = value.ok_or_else(|| SharedError::Invalid("draft has no `nodes`".to_string()))?;
    let mut out = Vec::new();
    match value {
        serde_json::Value::Object(map) => {
            for (alias, node) in map {
                let node = node.as_object().ok_or_else(|| {
                    SharedError::Invalid(format!("node '{alias}' must be an object"))
                })?;
                out.push((alias.clone(), node.clone()));
            }
        }
        serde_json::Value::Array(items) => {
            for (index, node) in items.iter().enumerate() {
                let node = node.as_object().ok_or_else(|| {
                    SharedError::Invalid(format!("node #{index} must be an object"))
                })?;
                let alias = node
                    .get("id")
                    .or_else(|| node.get("alias"))
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        SharedError::Invalid(format!(
                            "node #{index} needs an `id` when `nodes` is an array"
                        ))
                    })?;
                out.push((alias.to_string(), node.clone()));
            }
        }
        _ => {
            return Err(SharedError::Invalid(
                "`nodes` must be an object (or an array of objects with `id`)".to_string(),
            ));
        }
    }
    let mut seen = HashSet::new();
    for (alias, _) in &out {
        if !seen.insert(alias.clone()) {
            return Err(SharedError::Invalid(format!("duplicate node id '{alias}'")));
        }
    }
    if out.len() > MAX_NODES {
        return Err(SharedError::Invalid(format!(
            "too many nodes ({} > {MAX_NODES})",
            out.len()
        )));
    }
    Ok(out)
}

/// Parses a `$alias.pin` reference, returning the two parts.
fn parse_reference(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('$')?;
    let (alias, pin) = rest.split_once('.')?;
    if alias.is_empty() || pin.is_empty() {
        return None;
    }
    Some((alias.to_string(), pin.to_string()))
}

/// Parses the optional `wires` list (`"target.pin <- source.pin"`).
fn parse_wire_list(value: Option<&serde_json::Value>) -> SharedResult<Vec<PendingWire>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| SharedError::Invalid("`wires` must be an array of strings".to_string()))?;
    let mut out = Vec::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| SharedError::Invalid("`wires` entries must be strings".to_string()))?;
        let (target, source) = text.split_once("<-").ok_or_else(|| {
            SharedError::Invalid(format!(
                "wire '{text}' must be written as \"target.pin <- source.pin\""
            ))
        })?;
        let (target_alias, target_pin) = target.trim().split_once('.').ok_or_else(|| {
            SharedError::Invalid(format!("wire '{text}' has no target pin (expected `node.pin`)"))
        })?;
        let reference = source.trim().to_string();
        if parse_reference(&format!("${reference}")).is_none() {
            return Err(SharedError::Invalid(format!(
                "wire '{text}' has no source pin (expected `node.pin`)"
            )));
        }
        out.push(PendingWire {
            target: (target_alias.to_string(), target_pin.to_string()),
            reference,
        });
    }
    Ok(out)
}

/// Parses the optional `flow` list.
///
/// Each entry is a chain: `"a -> b"`, `"a -> b -> c"`, or with an explicit
/// output pin `"check.True -> ok"`.
fn parse_flow(value: Option<&serde_json::Value>) -> SharedResult<Vec<ExecEdge>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| SharedError::Invalid("`flow` must be an array of strings".to_string()))?;
    let mut out = Vec::new();
    for item in items {
        let chain = item
            .as_str()
            .ok_or_else(|| SharedError::Invalid("`flow` entries must be strings".to_string()))?;
        let hops: Vec<&str> = chain.split("->").map(str::trim).collect();
        if hops.len() < 2 || hops.iter().any(|hop| hop.is_empty()) {
            return Err(SharedError::Invalid(format!(
                "flow '{chain}' must be written as \"from -> to\""
            )));
        }
        for pair in hops.windows(2) {
            out.push(ExecEdge {
                source: split_alias_pin(pair[0]),
                target: split_alias_pin(pair[1]).0,
                spelled: format!("{} -> {}", pair[0], pair[1]),
            });
        }
    }
    Ok(out)
}

/// Splits `alias` or `alias.Pin` into its parts.
fn split_alias_pin(text: &str) -> (String, Option<String>) {
    match text.split_once('.') {
        Some((alias, pin)) if !alias.is_empty() && !pin.is_empty() => {
            (alias.to_string(), Some(pin.to_string()))
        }
        _ => (text.to_string(), None),
    }
}

/// Adds one data wire to the blueprint.
fn add_data_wire(
    blueprint: &mut Blueprint,
    node_ids: &HashMap<&str, NodeId>,
    src_pins: &HashMap<(String, String), (NodeId, PinId)>,
    wire: &PendingWire,
) -> SharedResult<()> {
    let (target_alias, target_pin_name) = &wire.target;
    let target_node = *node_ids.get(target_alias.as_str()).ok_or_else(|| {
        SharedError::Invalid(format!("node '{target_alias}' is not declared"))
    })?;
    let (source_alias, source_pin_name) = parse_reference(&format!("${}", wire.reference))
        .ok_or_else(|| SharedError::Invalid(format!("invalid reference '${}'", wire.reference)))?;
    if !node_ids.contains_key(source_alias.as_str()) {
        return Err(SharedError::Invalid(format!(
            "node '{source_alias}' is not declared (source of '${}')",
            wire.reference
        )));
    }
    let (source_node, source_pin) = src_pins
        .get(&(source_alias.clone(), source_pin_name.clone()))
        .copied()
        .ok_or_else(|| {
            let available = node_ids
                .get(source_alias.as_str())
                .and_then(|id| {
                    blueprint
                        .nodes
                        .iter()
                        .find(|node| node.id == *id)
                        .map(|node| {
                            node.pins
                                .iter()
                                .filter(|pin| pin.pin_type == PinType::DataOutput)
                                .map(|pin| pin.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                })
                .unwrap_or_default();
            SharedError::Invalid(format!(
                "node '{source_alias}' has no data output '{source_pin_name}'{}",
                if available.is_empty() {
                    String::new()
                } else {
                    format!("; it has: {available}")
                }
            ))
        })?;
    let target_pin = blueprint
        .nodes
        .iter()
        .find(|node| node.id == target_node)
        .and_then(|node| {
            node.pins
                .iter()
                .find(|pin| {
                    pin.pin_type == PinType::DataInput
                        && (pin.name == *target_pin_name
                            || pin.key.as_deref() == Some(target_pin_name.as_str()))
                })
                .map(|pin| pin.id)
        })
        .ok_or_else(|| {
            let available = data_inputs_of(blueprint, target_node);
            SharedError::Invalid(format!(
                "node '{target_alias}' has no data input '{target_pin_name}'{}",
                if available.is_empty() {
                    String::new()
                } else {
                    format!("; it accepts: {available}")
                }
            ))
        })?;
    blueprint.edges.push(Edge {
        id: stable_id(&format!("wire.{target_alias}.{target_pin_name}")),
        source_node,
        source_pin,
        target_node,
        target_pin,
    });
    Ok(())
}

/// Adds one execution edge to the blueprint.
fn add_exec_edge(
    blueprint: &mut Blueprint,
    node_ids: &HashMap<&str, NodeId>,
    edge: &ExecEdge,
) -> SharedResult<()> {
    let (source_alias, source_pin_name) = &edge.source;
    let source_node = *node_ids.get(source_alias.as_str()).ok_or_else(|| {
        SharedError::Invalid(format!("flow '{}': unknown node '{source_alias}'", edge.spelled))
    })?;
    let source_pin = blueprint
        .nodes
        .iter()
        .find(|node| node.id == source_node)
        .and_then(|node| match source_pin_name {
            Some(name) => node
                .pins
                .iter()
                .find(|pin| pin.pin_type == PinType::ExecOutput && pin.name == *name)
                .map(|pin| pin.id),
            None => node
                .pins
                .iter()
                .find(|pin| pin.pin_type == PinType::ExecOutput)
                .map(|pin| pin.id),
        })
        .ok_or_else(|| {
            let available = exec_outputs_of(blueprint, source_node);
            match source_pin_name {
                Some(name) => SharedError::Invalid(format!(
                    "flow '{}': node '{source_alias}' has no exec output '{name}'{}",
                    edge.spelled,
                    if available.is_empty() {
                        String::new()
                    } else {
                        format!("; it has: {available}")
                    }
                )),
                None => SharedError::Invalid(format!(
                    "flow '{}': node '{source_alias}' has no exec output",
                    edge.spelled
                )),
            }
        })?;
    let target_node = *node_ids.get(edge.target.as_str()).ok_or_else(|| {
        SharedError::Invalid(format!("flow '{}': unknown node '{}'", edge.spelled, edge.target))
    })?;
    let target_pin = blueprint
        .nodes
        .iter()
        .find(|node| node.id == target_node)
        .and_then(|node| node.pins.iter().find(|pin| pin.pin_type == PinType::ExecInput))
        .map(|pin| pin.id)
        .ok_or_else(|| {
            SharedError::Invalid(format!(
                "flow '{}': node '{}' cannot be a target (it has no exec input; pure functions take data inputs only)",
                edge.spelled, edge.target
            ))
        })?;
    blueprint.edges.push(Edge {
        id: stable_id(&format!("flow.{}", edge.spelled)),
        source_node,
        source_pin,
        target_node,
        target_pin,
    });
    Ok(())
}

/// Lists the data input names of a node, for error messages.
fn data_inputs_of(blueprint: &Blueprint, node_id: NodeId) -> String {
    blueprint
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .map(|node| {
            node.pins
                .iter()
                .filter(|pin| pin.pin_type == PinType::DataInput)
                .map(|pin| pin.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Lists the execution output names of a node, for error messages.
fn exec_outputs_of(blueprint: &Blueprint, node_id: NodeId) -> String {
    blueprint
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .map(|node| {
            node.pins
                .iter()
                .filter(|pin| pin.pin_type == PinType::ExecOutput)
                .map(|pin| pin.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Assigns positions with a deterministic layered layout.
///
/// Layers come from breadth-first distance along all edges, so control flow
/// reads left to right and a node sits after everything it depends on. The
/// draft never has to spell coordinates, and the result is stable for a given
/// graph.
fn layout(blueprint: &mut Blueprint, entry: NodeId) {
    let mut layer: HashMap<NodeId, usize> = HashMap::new();
    let mut queue = VecDeque::new();
    if !entry.is_nil() {
        layer.insert(entry, 0);
        queue.push_back(entry);
    }
    while let Some(current) = queue.pop_front() {
        let current_layer = layer[&current];
        for edge in blueprint.edges.iter().filter(|edge| edge.source_node == current) {
            let next_layer = current_layer + 1;
            if layer.get(&edge.target_node).is_none_or(|known| *known < next_layer) {
                layer.insert(edge.target_node, next_layer);
                queue.push_back(edge.target_node);
            }
        }
    }
    // Nodes no edge reaches (a disconnected fragment) go into the last layer.
    let fallback = layer.values().copied().max().unwrap_or(0) + 1;
    let mut used: HashMap<usize, usize> = HashMap::new();
    for index in 0..blueprint.nodes.len() {
        let node_id = blueprint.nodes[index].id;
        let column = layer.get(&node_id).copied().unwrap_or(fallback);
        let row = used.entry(column).or_insert(0);
        blueprint.nodes[index].position = (column as f32 * LAYER_X, *row as f32 * LAYER_Y);
        *row += 1;
    }
}

/// Derives a stable id from a string, so recompiling a draft is idempotent.
fn stable_id(seed: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes())
}

/// Suggests a close kind name for a typo, keeping the message actionable.
fn suggest_kind(kind: &str) -> String {
    let lowered = kind.to_ascii_lowercase();
    let candidates: Vec<&str> = super::compile::known_kinds()
        .into_iter()
        .filter(|candidate| candidate.to_ascii_lowercase().contains(&lowered))
        .take(5)
        .collect();
    if candidates.is_empty() {
        String::new()
    } else {
        format!("; did you mean one of: {}", candidates.join(", "))
    }
}
