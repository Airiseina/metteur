//! Node kinds related to the blueprint function library.
//!
//! `CallFunction`, `FunctionEntry` and `FunctionExit` are handled directly by
//! the frame-aware interpreter, so the executors here only exist to make the
//! kinds valid for blueprint validation and the node-kind registry.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::{Node, PinId, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

/// A node calling a registered function (interpreter-handled).
pub struct CallFunctionExecutor;

#[async_trait]
impl NodeExecutor for CallFunctionExecutor {
    fn kind(&self) -> &str {
        "CallFunction"
    }

    async fn execute(
        &self,
        _node: &Node,
        _inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        Err(DaemonError::Execution("CallFunction is handled by the interpreter".to_string()))
    }
}

/// Entry node of a function body (interpreter-handled).
pub struct FunctionEntryExecutor;

#[async_trait]
impl NodeExecutor for FunctionEntryExecutor {
    fn kind(&self) -> &str {
        "FunctionEntry"
    }

    async fn execute(
        &self,
        _node: &Node,
        _inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        Err(DaemonError::Execution("FunctionEntry is handled by the interpreter".to_string()))
    }
}

/// Exit node of a function body (interpreter-handled).
pub struct FunctionExitExecutor;

#[async_trait]
impl NodeExecutor for FunctionExitExecutor {
    fn kind(&self) -> &str {
        "FunctionExit"
    }

    async fn execute(
        &self,
        _node: &Node,
        _inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        Err(DaemonError::Execution("FunctionExit is handled by the interpreter".to_string()))
    }
}
