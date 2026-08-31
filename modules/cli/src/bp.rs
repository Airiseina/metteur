//! Conversions between blueprint JSON files and protobuf messages.
//!
//! The on-disk format mirrors `metteur_shared::Blueprint`: camelCase fields,
//! node positions as `[x, y]` pairs and node data embedded as raw JSON.

use anyhow::Context;
use metteur_proto::proto::{Blueprint, Edge, Node, Pin};
use serde_json::{Value, json};

/// Parses a blueprint JSON document into its proto form.
pub fn from_json(text: &str) -> anyhow::Result<Blueprint> {
    let doc: Value = serde_json::from_str(text).context("blueprint file is not valid JSON")?;
    Ok(Blueprint {
        id: uuid_field(&doc, "id")?,
        name: str_field(&doc, "name"),
        entry_node_id: uuid_field(&doc, "entry_node_id")?,
        nodes: doc
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| nodes.iter().map(parse_node).collect())
            .transpose()?
            .unwrap_or_default(),
        edges: doc
            .get("edges")
            .and_then(Value::as_array)
            .map(|edges| edges.iter().map(parse_edge).collect())
            .transpose()?
            .unwrap_or_default(),
    })
}

/// Renders a proto blueprint as pretty JSON text.
pub fn to_json(bp: &Blueprint) -> anyhow::Result<String> {
    serde_json::to_string_pretty(&json!({
        "id": bp.id,
        "name": bp.name,
        "entry_node_id": bp.entry_node_id,
        "nodes": bp.nodes.iter().map(node_value).collect::<Vec<_>>(),
        "edges": bp.edges.iter().map(edge_value).collect::<Vec<_>>(),
    }))
    .context("failed to serialize blueprint")
}

/// Overrides the blueprint id after validating it is a UUID.
pub fn with_id(mut bp: Blueprint, id: &str) -> anyhow::Result<Blueprint> {
    uuid::Uuid::parse_str(id).context("blueprint id must be a UUID")?;
    bp.id = id.to_string();
    Ok(bp)
}

fn parse_node(node: &Value) -> anyhow::Result<Node> {
    let data = node.get("data").cloned().unwrap_or_else(|| json!({}));
    let position =
        node.get("position").and_then(Value::as_array).map(|p| (num(p.first()), num(p.get(1))));
    let pins = node
        .get("pins")
        .and_then(Value::as_array)
        .map(|pins| pins.iter().map(parse_pin).collect())
        .transpose()?
        .unwrap_or_default();
    Ok(Node {
        id: uuid_field(node, "id")?,
        node_type: str_field(node, "node_type"),
        kind: str_field(node, "kind"),
        pos_x: position.unwrap_or((0.0, 0.0)).0,
        pos_y: position.unwrap_or((0.0, 0.0)).1,
        pins,
        data_json: serde_json::to_string(&data).context("invalid node data")?,
    })
}

fn parse_pin(pin: &Value) -> anyhow::Result<Pin> {
    Ok(Pin {
        id: uuid_field(pin, "id")?,
        name: str_field(pin, "name"),
        pin_type: str_field(pin, "pin_type"),
        data_type: str_field(pin, "data_type"),
    })
}

fn parse_edge(edge: &Value) -> anyhow::Result<Edge> {
    Ok(Edge {
        id: uuid_field(edge, "id")?,
        source_node: uuid_field(edge, "source_node")?,
        source_pin: uuid_field(edge, "source_pin")?,
        target_node: uuid_field(edge, "target_node")?,
        target_pin: uuid_field(edge, "target_pin")?,
    })
}

/// Reads a string field that must be a UUID.
fn uuid_field(obj: &Value, field: &str) -> anyhow::Result<String> {
    let value = obj
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("missing blueprint field '{field}'"))?;
    uuid::Uuid::parse_str(value)
        .with_context(|| format!("blueprint field '{field}' is not a UUID"))?;
    Ok(value.to_string())
}

fn str_field(obj: &Value, field: &str) -> String {
    obj.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn num(value: Option<&Value>) -> f32 {
    value.and_then(Value::as_f64).unwrap_or(0.0) as f32
}

fn node_value(node: &Node) -> Value {
    json!({
        "id": node.id,
        "node_type": node.node_type,
        "kind": node.kind,
        "position": [node.pos_x, node.pos_y],
        "pins": node.pins.iter().map(|p| json!({
            "id": p.id,
            "name": p.name,
            "pin_type": p.pin_type,
            "data_type": p.data_type,
        })).collect::<Vec<_>>(),
        "data": serde_json::from_str::<Value>(&node.data_json).unwrap_or(Value::Null),
    })
}

fn edge_value(edge: &Edge) -> Value {
    json!({
        "id": edge.id,
        "source_node": edge.source_node,
        "source_pin": edge.source_pin,
        "target_node": edge.target_node,
        "target_pin": edge.target_pin,
    })
}
