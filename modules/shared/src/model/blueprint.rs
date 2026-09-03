//! Blueprint model: nodes, pins and edges forming an execution graph.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

/// Identifier for a node within a blueprint.
pub type NodeId = Uuid;

/// Identifier for a pin within a node.
pub type PinId = Uuid;

/// Identifier for an edge within a blueprint.
pub type EdgeId = Uuid;

/// The kind of a node, mirroring Unreal Engine blueprint categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeType {
    /// An event node (e.g. Start).
    Event,
    /// A call node with one execution input and one execution output.
    Function,
    /// A pure function node without execution pins.
    Pure,
    /// A control-flow node with one or more execution outputs.
    Control,
}

/// The direction and role of a pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinType {
    /// An execution input pin.
    ExecInput,
    /// An execution output pin.
    ExecOutput,
    /// A data input pin.
    DataInput,
    /// A data output pin.
    DataOutput,
}

/// The data type carried by a data pin.
///
/// Types are serialized as compact strings (`int`, `list<int>`, `object{a:int}`)
/// shared with the proto layer, the DSL and the web UI. Deserialization accepts
/// the legacy bare names (`"List"`, `"Float"`) so persisted blueprints and
/// function libraries stay readable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DataType {
    /// No data.
    #[default]
    Void,
    /// A boolean.
    Bool,
    /// A signed integer.
    Int,
    /// A floating point number.
    Float,
    /// A string.
    String,
    /// A list with a known element type.
    List(Box<DataType>),
    /// An arbitrary JSON value.
    Json,
    /// A structured object with a field type table; an empty table accepts
    /// any object.
    Object(HashMap<String, DataType>),
    /// Any type (wildcard, used by tool arguments and function parameters).
    Any,
    /// An LLM context manager flowing through the blueprint.
    Context,
    /// A constrained string chosen from a fixed set (rendered as a dropdown).
    Choice,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Void => write!(f, "void"),
            DataType::Bool => write!(f, "bool"),
            DataType::Int => write!(f, "int"),
            DataType::Float => write!(f, "float"),
            DataType::String => write!(f, "string"),
            DataType::List(inner) => write!(f, "list<{inner}>"),
            DataType::Json => write!(f, "json"),
            DataType::Object(fields) => {
                write!(f, "object{{")?;
                let mut first = true;
                for (name, ty) in fields {
                    if !first {
                        write!(f, ",")?;
                    }
                    first = false;
                    write!(f, "{name}:{ty}")?;
                }
                write!(f, "}}")
            }
            DataType::Any => write!(f, "any"),
            DataType::Context => write!(f, "context"),
            DataType::Choice => write!(f, "choice"),
        }
    }
}

impl FromStr for DataType {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let text = input.trim();
        if let Some(inner) = text.strip_prefix("list<").and_then(|t| t.strip_suffix('>')) {
            return Ok(DataType::List(Box::new(inner.parse()?)));
        }
        if let Some(inner) = text.strip_prefix("object{").and_then(|t| t.strip_suffix('}')) {
            let mut fields = HashMap::new();
            for part in inner.split(',') {
                if part.trim().is_empty() {
                    continue;
                }
                let (name, ty) = part.split_once(':').ok_or_else(|| {
                    format!("object field '{part}' must be 'name:type'")
                })?;
                fields.insert(name.trim().to_string(), ty.trim().parse()?);
            }
            return Ok(DataType::Object(fields));
        }
        Ok(match text.to_ascii_lowercase().as_str() {
            "void" | "null" | "none" => DataType::Void,
            "bool" | "boolean" => DataType::Bool,
            "int" | "integer" | "i64" => DataType::Int,
            "float" | "number" | "double" | "f64" => DataType::Float,
            "string" | "str" => DataType::String,
            "list" | "array" => DataType::List(Box::new(DataType::Any)),
            "json" => DataType::Json,
            "object" | "map" | "dict" => DataType::Object(HashMap::new()),
            "any" => DataType::Any,
            "context" => DataType::Context,
            "choice" | "enum" => DataType::Choice,
            other => return Err(format!("unknown data type '{other}'")),
        })
    }
}

impl Serialize for DataType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DataType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(de::Error::custom)
    }
}

/// A pin attached to a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// Unique identifier of the pin.
    pub id: PinId,
    /// Semantic key (stable across renames); the display `name` may differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Human-readable pin name.
    pub name: String,
    /// The role of the pin.
    pub pin_type: PinType,
    /// The data type carried by the pin.
    pub data_type: DataType,
    /// Default value used when the input is not connected (data inputs only).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Whether the input may stay unconnected (resolves to null).
    #[serde(default)]
    pub optional: bool,
    /// Allowed values for enumeration pins (e.g. reasoning effort).
    #[serde(default)]
    pub choices: Vec<String>,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
}

impl Pin {
    /// Creates a data pin without metadata.
    pub fn data(
        name: impl Into<String>,
        pin_type: PinType,
        data_type: DataType,
        id: PinId,
    ) -> Self {
        Self {
            id,
            key: None,
            name: name.into(),
            pin_type,
            data_type,
            default: None,
            optional: false,
            choices: Vec::new(),
            description: None,
        }
    }

    /// Creates an execution pin.
    pub fn exec(pin_type: PinType, id: PinId) -> Self {
        Self::data("Exec", pin_type, DataType::Void, id)
    }
}

impl Default for Pin {
    fn default() -> Self {
        Self::data(String::new(), PinType::DataInput, DataType::Any, Uuid::nil())
    }
}

/// A node in a blueprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Unique identifier of the node.
    pub id: NodeId,
    /// The kind of the node.
    pub node_type: NodeType,
    /// The concrete node implementation key (e.g. `Start`, `Add`, `Branch`).
    pub kind: String,
    /// UI layout position.
    pub position: (f32, f32),
    /// Pins owned by the node.
    pub pins: Vec<Pin>,
    /// Node-specific configuration.
    pub data: serde_json::Value,
}

/// A directed connection between two pins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Unique identifier of the edge.
    pub id: EdgeId,
    /// The source node.
    pub source_node: NodeId,
    /// The source pin.
    pub source_pin: PinId,
    /// The target node.
    pub target_node: NodeId,
    /// The target pin.
    pub target_pin: PinId,
}

/// A complete blueprint (execution plan).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blueprint {
    /// Unique identifier of the blueprint.
    pub id: Uuid,
    /// Human-readable blueprint name.
    pub name: String,
    /// All nodes in the blueprint.
    pub nodes: Vec<Node>,
    /// All edges in the blueprint.
    pub edges: Vec<Edge>,
    /// The entry node (typically the Start node).
    pub entry_node_id: NodeId,
}

impl Blueprint {
    /// Returns the node with the given id, if present.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Returns the pin with the given id across all nodes, if present.
    pub fn pin(&self, id: PinId) -> Option<&Pin> {
        self.nodes.iter().flat_map(|n| n.pins.iter()).find(|p| p.id == id)
    }

    /// Returns the edges originating from the given node.
    pub fn outgoing_edges(&self, node_id: NodeId) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.source_node == node_id).collect()
    }

    /// Returns the edges targeting the given node.
    pub fn incoming_edges(&self, node_id: NodeId) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.target_node == node_id).collect()
    }
}
