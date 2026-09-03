//! Core data model shared across crates.

pub mod blueprint;
pub mod function;
pub mod types;
pub mod value;

pub use blueprint::{
    Blueprint, DataType, Edge, EdgeId, Node, NodeId, NodeType, Pin, PinId, PinType,
};
pub use function::{
    CALL_FUNCTION_KIND, FUNCTION_ENTRY_KIND, FUNCTION_EXIT_KIND, FnPin, FunctionEntry,
    FunctionSignature, FunctionSource,
};
pub use types::{compatible, coerce};
pub use value::{Value, json_to_value};
