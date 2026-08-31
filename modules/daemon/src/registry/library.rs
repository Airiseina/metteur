//! Function library persistence and built-in function definitions.
//!
//! Functions are stored in the `functions` column family of either the global
//! database or a workspace database and mirrored into the shared [`Registry`]
//! so the frame-aware interpreter can resolve them by name or id at call time.

use metteur_shared::model::function::{FnPin, FunctionEntry, FunctionSignature, FunctionSource};
use metteur_shared::{Blueprint, DataType, Edge, Node, NodeType, Pin, PinType};

use crate::error::{DaemonError, DaemonResult};
use crate::persistence::Db;

/// Builds the built-in `ChainOfThought` function.
///
/// The body is `FunctionEntry -> CallLLM(think step by step) -> FunctionExit`,
/// taking no inputs and returning the LLM answer as `Result`. It doubles as a
/// smoke-test of the function library machinery.
pub fn builtin_chain_of_thought() -> FunctionEntry {
    let fn_id = uuid::Uuid::new_v4();
    let entry_node = uuid::Uuid::new_v4();
    let llm = uuid::Uuid::new_v4();
    let exit_node = uuid::Uuid::new_v4();

    let entry_exec = uuid::Uuid::new_v4();
    let llm_exec_in = uuid::Uuid::new_v4();
    let llm_exec_out = uuid::Uuid::new_v4();
    let exit_exec = uuid::Uuid::new_v4();
    let llm_result = uuid::Uuid::new_v4();
    let exit_result = uuid::Uuid::new_v4();

    let entry_pin = |id: uuid::Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
        id,
        name: name.to_string(),
        pin_type,
        data_type,
    };

    FunctionEntry {
        id: fn_id,
        name: "ChainOfThought".to_string(),
        description: "Asks the LLM to reason step by step before answering.".to_string(),
        signature: FunctionSignature {
            inputs: Vec::new(),
            outputs: vec![FnPin {
                name: "Result".to_string(),
                data_type: DataType::String,
                description: None,
            }],
        },
        body: Blueprint {
            id: fn_id,
            name: "ChainOfThought".to_string(),
            nodes: vec![
                Node {
                    id: entry_node,
                    node_type: NodeType::Event,
                    kind: "FunctionEntry".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![entry_pin(entry_exec, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: llm,
                    node_type: NodeType::Function,
                    kind: "CallLLM".to_string(),
                    position: (120.0, 0.0),
                    pins: vec![
                        entry_pin(llm_exec_in, "Exec", PinType::ExecInput, DataType::Void),
                        entry_pin(llm_exec_out, "Exec", PinType::ExecOutput, DataType::Void),
                        entry_pin(llm_result, "Result", PinType::DataOutput, DataType::String),
                    ],
                    data: serde_json::json!({
                        "system": "Think step by step, then give a final answer.",
                        "prompt": "Answer the user's request.",
                    }),
                },
                Node {
                    id: exit_node,
                    node_type: NodeType::Event,
                    kind: "FunctionExit".to_string(),
                    position: (260.0, 0.0),
                    pins: vec![
                        entry_pin(exit_exec, "Exec", PinType::ExecInput, DataType::Void),
                        entry_pin(exit_result, "Result", PinType::DataInput, DataType::String),
                    ],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: uuid::Uuid::new_v4(),
                    source_node: entry_node,
                    source_pin: entry_exec,
                    target_node: llm,
                    target_pin: llm_exec_in,
                },
                Edge {
                    id: uuid::Uuid::new_v4(),
                    source_node: llm,
                    source_pin: llm_exec_out,
                    target_node: exit_node,
                    target_pin: exit_exec,
                },
                Edge {
                    id: uuid::Uuid::new_v4(),
                    source_node: llm,
                    source_pin: llm_result,
                    target_node: exit_node,
                    target_pin: exit_result,
                },
            ],
            entry_node_id: entry_node,
        },
        source: FunctionSource::Builtin,
    }
}

/// Stores a function entry into a database under the `functions` column family.
pub fn save(db: &Db, entry: &FunctionEntry) -> DaemonResult<()> {
    let data =
        serde_json::to_vec(entry).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    db.put(crate::persistence::cf::FUNCTIONS, entry.name.as_bytes(), &data)
}

/// Loads all stored functions from a database.
pub fn load_all(db: &Db) -> DaemonResult<Vec<FunctionEntry>> {
    let mut out = Vec::new();
    for (_, value) in db.scan(crate::persistence::cf::FUNCTIONS)? {
        let entry: FunctionEntry =
            serde_json::from_slice(&value).map_err(|e| DaemonError::Serialization(e.to_string()))?;
        out.push(entry);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Removes a function by name from a database.
pub fn delete(db: &Db, name: &str) -> DaemonResult<()> {
    db.delete(crate::persistence::cf::FUNCTIONS, name.as_bytes())
}

/// Returns whether the entry is well-formed enough to execute.
pub fn validate(entry: &FunctionEntry) -> Result<(), String> {
    if entry.name.is_empty() {
        return Err("function name must not be empty".to_string());
    }
    entry
        .body
        .nodes
        .iter()
        .find(|n| n.kind == metteur_shared::model::function::FUNCTION_ENTRY_KIND)
        .ok_or_else(|| "function body is missing a FunctionEntry node".to_string())?;
    entry
        .body
        .nodes
        .iter()
        .find(|n| n.kind == metteur_shared::model::function::FUNCTION_EXIT_KIND)
        .ok_or_else(|| "function body is missing a FunctionExit node".to_string())?;
    let _ = FunctionEntry::derive_signature(&entry.body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_removes_function_row() {
        let dir = std::env::temp_dir().join(format!("metteur-fn-{}", uuid::Uuid::new_v4()));
        let db = Db::open(&dir).unwrap();
        let entry = builtin_chain_of_thought();

        save(&db, &entry).unwrap();
        assert!(!load_all(&db).unwrap().is_empty());

        delete(&db, &entry.name).unwrap();
        assert!(load_all(&db).unwrap().is_empty());
    }
}