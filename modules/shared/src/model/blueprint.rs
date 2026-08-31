//! Blueprint model: nodes, pins and edges forming an execution graph.

use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    /// No data.
    Void,
    /// A boolean.
    Bool,
    /// A signed integer.
    Int,
    /// A floating point number.
    Float,
    /// A string.
    String,
    /// A list of values.
    List,
    /// An arbitrary JSON value.
    Json,
}

/// A pin attached to a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// Unique identifier of the pin.
    pub id: PinId,
    /// Human-readable pin name.
    pub name: String,
    /// The role of the pin.
    pub pin_type: PinType,
    /// The data type carried by the pin.
    pub data_type: DataType,
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
