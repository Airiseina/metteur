//! Execution checkpoints for crash-safe resume.
//!
//! A checkpoint serializes the interpreter's scheduling state (executed and
//! pending nodes, produced data values, the transaction log) after each node.
//! On restart, an interrupted run can be resumed from its latest checkpoint.

use std::collections::HashMap;

use metteur_shared::{NodeId, PinId, Value};
use serde::{Deserialize, Serialize};

use crate::error::{DaemonError, DaemonResult};
use crate::storage::persistence::{Db, cf};

use super::context::Frame;
use super::transaction::TransactionEntry;

/// The lifecycle status of an execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    /// Actively executing.
    Running,
    /// Saved but not currently executing (e.g. after a crash).
    Suspended,
    /// Finished successfully.
    Completed,
    /// Finished with an error; cannot be resumed.
    Failed,
}

impl RunStatus {
    /// Returns whether a run in this state may be resumed.
    pub fn resumable(&self) -> bool {
        matches!(self, RunStatus::Running | RunStatus::Suspended)
    }
}

/// A serializable snapshot of a paused execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionCheckpoint {
    /// The run identifier.
    pub run_id: uuid::Uuid,
    /// The blueprint being executed.
    pub blueprint_id: uuid::Uuid,
    /// The run status.
    pub status: RunStatus,
    /// Execution start time in milliseconds since the Unix epoch.
    pub started_at: u64,
    /// The time this checkpoint was written.
    pub updated_at: u64,
    /// The execution call stack.
    pub call_stack: Vec<Frame>,
    /// Values produced on data output pins.
    pub data_values: HashMap<PinId, Value>,
    /// Nodes that completed execution.
    pub executed: Vec<NodeId>,
    /// Nodes waiting to run (the work queue).
    pub pending: Vec<NodeId>,
    /// Nodes ever triggered during the run.
    pub triggered: Vec<NodeId>,
    /// Mutations recorded so far, for rollback.
    pub transaction_log: Vec<TransactionEntry>,
    /// Completion order of the root scheduler (retry segment boundary).
    #[serde(default)]
    pub executed_order: Vec<NodeId>,
    /// Failed validation attempts per validator node.
    #[serde(default)]
    pub attempt_counts: HashMap<NodeId, u32>,
    /// Rollback boundaries per validator node, refreshed on each pass.
    #[serde(default)]
    pub validation_marks: HashMap<NodeId, super::context::RetryMark>,
    /// Frame-scoped variable maps, innermost last (mirrors the call stack).
    #[serde(default)]
    pub variables: Vec<HashMap<String, Value>>,
    /// Active ForEach loops, innermost last.
    #[serde(default)]
    pub foreach_stack: Vec<super::context::ForEachState>,
    /// The agent's task list at this point in the run.
    #[serde(default)]
    pub todos: Vec<metteur_shared::llm::TodoItem>,
    /// The agent execution tree of the run.
    #[serde(default)]
    pub exec_tree: super::tree::ExecTree,
    /// Tree ids of entered function frames, innermost last.
    #[serde(default)]
    pub frame_trees: Vec<String>,
    /// The failure reason, if the run failed.
    pub error: Option<String>,
}

impl ExecutionCheckpoint {
    /// Builds an initial resumable checkpoint for a fresh run.
    pub fn running(run_id: uuid::Uuid, blueprint_id: uuid::Uuid, started_at: u64) -> Self {
        Self {
            run_id,
            blueprint_id,
            status: RunStatus::Running,
            started_at,
            updated_at: started_at,
            call_stack: Vec::new(),
            data_values: HashMap::new(),
            executed: Vec::new(),
            pending: Vec::new(),
            triggered: Vec::new(),
            transaction_log: Vec::new(),
            executed_order: Vec::new(),
            attempt_counts: HashMap::new(),
            validation_marks: HashMap::new(),
            variables: vec![HashMap::new()],
            foreach_stack: Vec::new(),
            todos: Vec::new(),
            exec_tree: super::tree::ExecTree::new(),
            frame_trees: Vec::new(),
            error: None,
        }
    }
}

/// A target for persisting execution checkpoints.
pub trait CheckpointSink: Send + Sync {
    /// The run id this sink writes for.
    fn run_id(&self) -> uuid::Uuid;

    /// Persists a checkpoint, overwriting the previous one for the run.
    fn write(&self, checkpoint: &ExecutionCheckpoint) -> DaemonResult<()>;
}

/// A checkpoint sink backed by the workspace RocksDB.
#[derive(Clone)]
pub struct DbCheckpointSink {
    db: Db,
    run_id: uuid::Uuid,
}

impl DbCheckpointSink {
    /// Creates a sink writing for `run_id`.
    pub fn new(db: Db, run_id: uuid::Uuid) -> Self {
        Self {
            db,
            run_id,
        }
    }

    /// Loads the checkpoint for `run_id`, if any.
    pub fn load(db: &Db, run_id: uuid::Uuid) -> DaemonResult<Option<ExecutionCheckpoint>> {
        let data = match db.get(cf::EXECUTION_STATE, run_id.as_bytes())? {
            Some(data) => data,
            None => return Ok(None),
        };
        let checkpoint: ExecutionCheckpoint =
            serde_json::from_slice(&data).map_err(|e| DaemonError::Serialization(e.to_string()))?;
        Ok(Some(checkpoint))
    }

    /// Lists all checkpoints in the database, oldest first.
    pub fn list(db: &Db) -> DaemonResult<Vec<ExecutionCheckpoint>> {
        let mut out = Vec::new();
        for (_, value) in db.scan(cf::EXECUTION_STATE)? {
            let checkpoint: ExecutionCheckpoint = serde_json::from_slice(&value)
                .map_err(|e| DaemonError::Serialization(e.to_string()))?;
            out.push(checkpoint);
        }
        out.sort_by_key(|c| c.started_at);
        Ok(out)
    }
}

impl CheckpointSink for DbCheckpointSink {
    fn run_id(&self) -> uuid::Uuid {
        self.run_id
    }

    fn write(&self, checkpoint: &ExecutionCheckpoint) -> DaemonResult<()> {
        let data = serde_json::to_vec(checkpoint)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?;
        self.db.put(cf::EXECUTION_STATE, self.run_id.as_bytes(), &data)
    }
}
