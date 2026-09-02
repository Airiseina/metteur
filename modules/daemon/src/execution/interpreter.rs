//! The blueprint interpreter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use metteur_shared::config::Config;
use metteur_shared::{Blueprint, Node, NodeId, PinId, PinType, Value};
use parking_lot::RwLock as PLock;
use tokio::sync::RwLock;

use crate::observability::audit::AuditWriter;
use crate::error::{DaemonError, DaemonResult};
use crate::llm::LlmClientFactory;
use crate::registry::Registry;

use super::checkpoint::{CheckpointSink, ExecutionCheckpoint, RunStatus};
use super::context::{ExecutionContext, ExecutionState, Frame, FunctionBody, Scheduler};
use super::interrupt::{InterruptBus, InterruptPriority};
use super::transaction::TransactionLog;

/// Shared handle to the executing root blueprint.
///
/// The ReplanBlueprint tool rewrites the plan through this handle while the
/// run reads a fresh snapshot at every node boundary (hot apply).
pub type SharedBlueprint = Arc<PLock<Blueprint>>;

/// Maximum nesting depth of function body frames.
const MAX_FUNCTION_DEPTH: u32 = 8;

/// An event emitted during blueprint execution.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ExecutionEvent {
    /// A node started executing.
    NodeStarted {
        node_id: NodeId,
    },
    /// A node finished executing.
    NodeFinished {
        node_id: NodeId,
    },
    /// A node finished and reported its produced data output values.
    NodeData {
        node_id: NodeId,
        outputs: Vec<(PinId, Value)>,
        /// The function whose body this node belongs to, if any.
        function: Option<uuid::Uuid>,
    },
    /// A node produced a message.
    Message {
        node_id: NodeId,
        message: String,
    },
    /// The sandbox requests user approval for an operation.
    ApprovalRequested {
        node_id: NodeId,
        request_id: String,
        detail: String,
    },
    /// A CallLLM node finished and reported its context composition.
    ContextUsage {
        node_id: NodeId,
        regions: Vec<metteur_shared::llm::ContextRegion>,
    },
}

/// Executes a blueprint against the given registry.
pub struct Interpreter {
    registry: Arc<Registry>,
    llm_factory: LlmClientFactory,
    workspace_root: std::path::PathBuf,
    state: ExecutionState,
    scheduler: Scheduler,
    events: Vec<ExecutionEvent>,
    checkpoint: Option<Arc<dyn CheckpointSink>>,
    audit: Option<AuditWriter>,
    config: Option<Arc<RwLock<Config>>>,
    user: String,
    blueprint_id: uuid::Uuid,
    started_at: u64,
    /// Live event sink; when set, events stream out instead of buffering.
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<ExecutionEvent>>,
    approvals: Option<Arc<crate::sandbox::approval::ApprovalBroker>>,
    metrics: Option<Arc<crate::observability::metrics::Metrics>>,
    transaction_log: Option<super::transaction::TransactionLog>,
    workspace_db: Option<crate::storage::persistence::Db>,
    global_db: Option<crate::storage::persistence::Db>,
    lsp: Option<Arc<crate::integration::lsp::LspManager>>,
    addon_fragments: Vec<metteur_shared::llm::SystemFragment>,
    shared_blueprint: Option<SharedBlueprint>,
    circuit_failures: u32,
}

impl Interpreter {
    /// Creates a new interpreter for the given registry.
    pub fn new(
        registry: Arc<Registry>,
        llm_factory: LlmClientFactory,
        workspace_root: std::path::PathBuf,
    ) -> Self {
        Self {
            registry,
            llm_factory,
            workspace_root,
            state: ExecutionState::default(),
            scheduler: Scheduler::default(),
            events: Vec::new(),
            checkpoint: None,
            audit: None,
            config: None,
            user: "local".to_string(),
            blueprint_id: uuid::Uuid::nil(),
            started_at: 0,
            event_tx: None,
            approvals: None,
            metrics: None,
            transaction_log: None,
            workspace_db: None,
            global_db: None,
            lsp: None,
            addon_fragments: Vec::new(),
            shared_blueprint: None,
            circuit_failures: 0,
        }
    }

    /// Enables checkpoint persistence through the given sink.
    pub fn with_checkpoint_sink(mut self, sink: Arc<dyn CheckpointSink>) -> Self {
        self.checkpoint = Some(sink);
        self
    }

    /// Attaches a workspace audit writer.
    pub fn with_audit(mut self, audit: AuditWriter) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Attaches the merged workspace configuration.
    pub fn with_config(mut self, config: Arc<RwLock<Config>>) -> Self {
        self.config = Some(config);
        self
    }

    /// Attaches the authenticated subject performing the execution.
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// Streams execution events through `tx` as they occur instead of
    /// buffering them until the run completes.
    pub fn with_event_tx(mut self, tx: tokio::sync::mpsc::UnboundedSender<ExecutionEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Attaches the sandbox approval broker shared with the control RPCs.
    pub fn with_approvals(
        mut self,
        approvals: Arc<crate::sandbox::approval::ApprovalBroker>,
    ) -> Self {
        self.approvals = Some(approvals);
        self
    }

    /// Attaches process-wide metrics.
    pub fn with_metrics(mut self, metrics: Arc<crate::observability::metrics::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Shares an existing transaction log so nested runs record into the
    /// parent log and outer rollbacks cover their mutations.
    pub fn with_transaction_log(mut self, log: super::transaction::TransactionLog) -> Self {
        self.transaction_log = Some(log);
        self
    }

    /// Attaches the workspace database for persistent sandbox grants.
    pub fn with_workspace_db(mut self, db: crate::storage::persistence::Db) -> Self {
        self.workspace_db = Some(db);
        self
    }

    /// Attaches the global database for global sandbox grants.
    pub fn with_global_db(mut self, db: crate::storage::persistence::Db) -> Self {
        self.global_db = Some(db);
        self
    }

    /// Attaches the workspace language-server manager.
    pub fn with_lsp(mut self, lsp: Arc<crate::integration::lsp::LspManager>) -> Self {
        self.lsp = Some(lsp);
        self
    }

    /// Attaches addon prompt fragments for fresh CallLLM contexts.
    pub fn with_addon_fragments(
        mut self,
        fragments: Vec<metteur_shared::llm::SystemFragment>,
    ) -> Self {
        self.addon_fragments = fragments;
        self
    }

    /// Emits an event to the live sink or buffers it when no sink is set.
    fn emit(&mut self, event: ExecutionEvent) {
        match &self.event_tx {
            Some(tx) => {
                let _ = tx.send(event);
            }
            None => self.events.push(event),
        }
    }

    /// Executes the blueprint from its entry node.
    ///
    /// The interpreter state is reset on each call, so an instance may be
    /// reused across executions. `interrupts` optionally provides a channel
    /// for injecting temporary messages during execution.
    pub async fn run(
        &mut self,
        blueprint: &SharedBlueprint,
        interrupts: Option<InterruptBus>,
    ) -> DaemonResult<Vec<ExecutionEvent>> {
        self.run_with_control(
            blueprint,
            interrupts,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
    }

    /// Executes the blueprint with shared pause/cancel control flags.
    ///
    /// The flags are shared with the gRPC control RPCs so that pause and
    /// cancel requests take effect during execution. `blueprint` is shared so
    /// a replan can hot-apply data-level edits to the remainder of the run.
    pub async fn run_with_control(
        &mut self,
        blueprint: &SharedBlueprint,
        interrupts: Option<InterruptBus>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    ) -> DaemonResult<Vec<ExecutionEvent>> {
        // Reject blueprints whose execution graph contains a cycle.
        if has_exec_cycle(&blueprint.read()) {
            return Err(DaemonError::Execution(
                "execution cycle detected in blueprint".to_string(),
            ));
        }

        self.shared_blueprint = Some(blueprint.clone());
        self.reset_run(blueprint);
        let mut ctx = self.make_context(interrupts, pause_requested, cancel_requested);
        self.write_checkpoint(&ctx);
        self.execute(blueprint, &mut ctx).await
    }

    /// Resumes an interrupted run from its checkpoint.
    ///
    /// The scheduling state, produced data values and transaction log are
    /// restored and execution continues with checkpoints written to the same
    /// run id. The node that was in flight when the run was interrupted is
    /// re-executed.
    pub async fn resume_with_control(
        &mut self,
        blueprint: &SharedBlueprint,
        resume: ExecutionCheckpoint,
        interrupts: Option<InterruptBus>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    ) -> DaemonResult<Vec<ExecutionEvent>> {
        if !resume.status.resumable() {
            return Err(DaemonError::Interrupted(format!(
                "run {} is not resumable",
                resume.run_id
            )));
        }
        self.shared_blueprint = Some(blueprint.clone());
        self.state = ExecutionState {
            blueprint_id: resume.blueprint_id,
            call_stack: resume.call_stack,
            data_values: resume.data_values,
            paused_at: None,
        };
        self.scheduler =
            Scheduler::from_checkpoint(resume.executed, resume.pending, resume.triggered);
        self.events.clear();
        self.blueprint_id = resume.blueprint_id;
        self.started_at = resume.started_at;
        self.circuit_failures = 0;

        let mut ctx = self.make_context(interrupts, pause_requested, cancel_requested);
        ctx.transaction_log = TransactionLog::from_entries(resume.transaction_log);
        self.write_checkpoint(&ctx);
        self.execute(blueprint, &mut ctx).await
    }

    /// Resets the interpreter for a fresh run of `blueprint`.
    fn reset_run(&mut self, blueprint: &SharedBlueprint) {
        let bp = blueprint.read();
        self.state = ExecutionState::default();
        self.state.blueprint_id = bp.id;
        self.state.call_stack.push(Frame {
            node_id: bp.entry_node_id,
            pc: 0,
            function: None,
        });
        self.events.clear();
        self.scheduler = Scheduler::default();
        self.scheduler.seed(bp.entry_node_id);
        self.blueprint_id = bp.id;
        self.started_at = now_millis();
        self.circuit_failures = 0;
    }

    /// Creates the per-run execution context.
    fn make_context(
        &self,
        interrupts: Option<InterruptBus>,
        pause_requested: Arc<std::sync::atomic::AtomicBool>,
        cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    ) -> ExecutionContext {
        let run_id = self.checkpoint.as_ref().map(|sink| sink.run_id()).unwrap_or_default();
        let mut ctx = ExecutionContext::new(
            self.registry.clone(),
            self.llm_factory.clone(),
            self.workspace_root.clone(),
        )
        .with_run(run_id, self.started_at);
        if let Some(audit) = &self.audit {
            ctx.audit = Some(audit.clone());
        }
        if let Some(config) = &self.config {
            ctx.config = Some(config.clone());
        }
        ctx.user = self.user.clone();
        ctx.interrupts = interrupts;
        ctx.pause_requested = pause_requested;
        ctx.cancel_requested = cancel_requested;
        ctx.events = self.event_tx.clone();
        ctx.approvals = self.approvals.clone();
        ctx.metrics = self.metrics.clone();
        if let Some(log) = &self.transaction_log {
            ctx.transaction_log = log.clone();
        }
        ctx.workspace_db = self.workspace_db.clone();
        ctx.global_db = self.global_db.clone();
        ctx.lsp = self.lsp.clone();
        ctx.addon_fragments = self.addon_fragments.clone();
        ctx.blueprint = self.shared_blueprint.clone();
        ctx
    }

    /// Persists a checkpoint of the current run state, if a sink is set.
    fn write_checkpoint(&self, ctx: &ExecutionContext) {
        let Some(sink) = &self.checkpoint else {
            return;
        };
        self.persist(sink, ctx, RunStatus::Running, None);
    }

    /// Persists the terminal checkpoint for a finished run.
    fn write_terminal_checkpoint(
        &self,
        ctx: &ExecutionContext,
        status: RunStatus,
        error: Option<String>,
    ) {
        let Some(sink) = &self.checkpoint else {
            return;
        };
        self.persist(sink, ctx, status, error);
    }

    /// Serializes and writes a checkpoint through the sink.
    fn persist(
        &self,
        sink: &Arc<dyn CheckpointSink>,
        ctx: &ExecutionContext,
        status: RunStatus,
        error: Option<String>,
    ) {
        let checkpoint = ExecutionCheckpoint {
            run_id: sink.run_id(),
            blueprint_id: self.blueprint_id,
            status,
            started_at: self.started_at,
            updated_at: now_millis(),
            call_stack: self.state.call_stack.clone(),
            data_values: self.state.data_values.clone(),
            executed: self.scheduler.executed_list(),
            pending: self.scheduler.pending_list(),
            triggered: self.scheduler.triggered_list(),
            transaction_log: ctx.transaction_log.entries().to_vec(),
            error,
        };
        if let Err(err) = sink.write(&checkpoint) {
            tracing::error!("failed to write checkpoint: {err}");
        }
    }

    /// Runs the worklist loop until the queue is drained.
    ///
    /// A checkpoint is written after each completed node and one final
    /// checkpoint with the terminal status when the run finishes or fails.
    async fn execute(
        &mut self,
        blueprint: &SharedBlueprint,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<Vec<ExecutionEvent>> {
        let bp_id = blueprint.read().id;
        ctx.audit(
            "execution.start",
            serde_json::json!({
                "run_id": ctx.run_id.to_string(),
                "blueprint_id": bp_id.to_string(),
            }),
        );
        if let Some(sink) = &self.checkpoint {
            tracing::info!("run {} started", sink.run_id());
        }

        let result = self.run_loop(blueprint, ctx).await;
        let status = match &result {
            Ok(()) => {
                ctx.audit(
                    "execution.end",
                    serde_json::json!({
                        "run_id": ctx.run_id.to_string(),
                        "blueprint_id": bp_id.to_string(),
                    }),
                );
                RunStatus::Completed
            }
            Err(err) => {
                ctx.audit(
                    "execution.failed",
                    serde_json::json!({
                        "run_id": ctx.run_id.to_string(),
                        "error": err.to_string(),
                    }),
                );
                RunStatus::Failed
            }
        };
        self.write_terminal_checkpoint(ctx, status, result.as_ref().err().map(|e| e.to_string()));
        result.map(|()| self.events.clone())
    }

    /// The main execution loop.
    ///
    /// Execution uses a worklist-based dataflow scheduler: a node runs once it
    /// has been triggered by an execution edge and all of its data inputs are
    /// available. This allows diamond-shaped blueprints where a node consumes
    /// data produced by multiple branches.
    ///
    /// The root blueprint is re-read at every node boundary so a replan can
    /// hot-apply data-level edits mid-run. No blueprint lock is held across
    /// `.await`, so a replan write lock can never deadlock against a run.
    ///
    /// Function frames: a `CallFunction` node pushes a frame carrying its own
    /// scheduler; when that frame's queue drains the exit node values are
    /// mapped back onto the caller's output pins and the frame is popped.
    async fn run_loop(
        &mut self,
        root: &SharedBlueprint,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        loop {
            // Honor cancellation and pause requests between nodes.
            if ctx.cancel_requested.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(DaemonError::Interrupted("cancelled by user".to_string()));
            }
            while ctx.pause_requested.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            // Resolve the active frame: the innermost entered function body,
            // or the root blueprint when no function is on the stack.
            let (active_bp, is_function) = match self.active_function() {
                Some(fn_id) => {
                    let entry = self.registry.function_by_id(fn_id).ok_or_else(|| {
                        DaemonError::Execution(format!("function {fn_id} not in registry"))
                    })?;
                    (entry.body, true)
                }
                None => (root.read().clone(), false),
            };

            let node_id = match self.active_scheduler_mut().pop() {
                Some(id) => id,
                None => {
                    if is_function {
                        // The function body drained: pop the frame, map its exit
                        // values back to the caller, and continue.
                        let frame = self.state.call_stack.pop().expect("function frame on top");
                        self.finish_function(&frame, &active_bp, ctx).await?;
                        continue;
                    }
                    break;
                }
            };

            // Skip nodes that already ran (e.g. re-queued by data producers).
            if self.active_scheduler().is_executed(node_id) {
                continue;
            }
            // Snapshot the node and its inputs; no blueprint lock is held
            // across the executor's `.await`.
            let (node, inputs) = {
                // Wait until all data inputs are available; the node is
                // re-enqueued when a producer fires an edge or produces data.
                if !self.data_inputs_ready(&active_bp, node_id) {
                    continue;
                }
                let node = active_bp
                    .node(node_id)
                    .cloned()
                    .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;
                let inputs = self.gather_inputs(&active_bp, node_id)?;
                (node, inputs)
            };

            self.emit(ExecutionEvent::NodeStarted {
                node_id,
            });
            ctx.current_node = node_id;
            ctx.audit(
                "node.started",
                serde_json::json!({ "node_id": node_id.to_string(), "kind": node.kind }),
            );

            // A CallFunction node hands control to the called body.
            if node.kind == metteur_shared::CALL_FUNCTION_KIND {
                self.enter_function(node_id, &node, &inputs, ctx).await?;
                continue;
            }

            // Entry/exit nodes of a function body execute implicitly: their
            // values are already wired by enter_function / finish_function.
            if (node.kind == metteur_shared::FUNCTION_ENTRY_KIND && is_function)
                || (node.kind == metteur_shared::FUNCTION_EXIT_KIND && is_function)
            {
                self.active_scheduler_mut().mark_executed(node_id);
                self.emit(ExecutionEvent::NodeFinished {
                    node_id,
                });
                ctx.audit(
                    "node.finished",
                    serde_json::json!({ "node_id": node_id.to_string(), "kind": node.kind }),
                );
                self.write_checkpoint(ctx);
                // Drive the body forward from the entry node (exit nodes
                // typically have no successors, which is harmless).
                self.fire_edges(&active_bp, node_id)?;
                continue;
            }

            if let Some(sink) = &self.checkpoint {
                tracing::info!("run {} executing node {node_id}", sink.run_id());
            }

            let executor = self.registry.node_executor(&node.kind).ok_or_else(|| {
                DaemonError::Execution(format!("no executor for node kind '{}'", node.kind))
            })?;
            let outputs = executor.execute(&node, &inputs, ctx).await?;
            self.state
                .data_values
                .extend(outputs.iter().map(|(id, v)| (*id, v.clone())));
            let function = self.active_function();
            self.emit(ExecutionEvent::NodeData {
                node_id,
                outputs: outputs.iter().map(|(id, v)| (*id, v.clone())).collect(),
                function,
            });
            self.active_scheduler_mut().mark_executed(node_id);
            if let Some(root_frame) = self.state.call_stack.first_mut()
                && root_frame.function.is_none()
            {
                root_frame.node_id = node_id;
            }
            self.emit(ExecutionEvent::NodeFinished {
                node_id,
            });
            ctx.audit(
                "node.finished",
                serde_json::json!({ "node_id": node_id.to_string(), "kind": node.kind }),
            );

            self.write_checkpoint(ctx);

            // Defer normal interrupts until the next Call LLM node.
            if let Some(bus) = &ctx.interrupts {
                for msg in bus.drain(InterruptPriority::Normal) {
                    ctx.pending_normal.push_back(msg);
                }
            }

            // Fire execution output edges and wake data consumers.
            self.fire_edges(&active_bp, node_id)?;

            // Circuit breaker: repeated validation failures trigger a
            // user-approved replan and re-run this node under the new plan.
            self.maybe_circuit_break(node_id, &node, &outputs, ctx).await?;
        }
        Ok(())
    }

    /// Counts consecutive validator/judge failures and, past the configured
    /// threshold, trips the circuit breaker (replan + rerun).
    async fn maybe_circuit_break(
        &mut self,
        node_id: NodeId,
        node: &Node,
        outputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        let failed = circuit_failed(node, outputs);
        self.circuit_failures = if failed {
            self.circuit_failures + 1
        } else {
            0
        };
        let threshold = ctx
            .config
            .as_ref()
            .and_then(|c| c.try_read().ok())
            .map(|cfg| cfg.execution.circuit_break_after)
            .unwrap_or(0);
        if threshold == 0 || self.circuit_failures < threshold {
            return Ok(());
        }
        crate::replan::trip_and_replan(ctx, node_id, self.circuit_failures).await?;
        self.circuit_failures = 0;
        let sched = self.active_scheduler_mut();
        sched.unmark_executed(node_id);
        sched.enqueue(node_id);
        Ok(())
    }

    /// Returns the id of the innermost function currently on the stack.
    fn active_function(&self) -> Option<uuid::Uuid> {
        match self.state.call_stack.last() {
            Some(frame) => frame.function.as_ref().map(|b| b.id),
            None => None,
        }
    }

    /// Returns a reference to the scheduler of the active frame.
    fn active_scheduler(&self) -> &Scheduler {
        match self.state.call_stack.last() {
            Some(frame) if frame.function.is_some() => &frame.function.as_ref().unwrap().scheduler,
            _ => &self.scheduler,
        }
    }

    /// Returns a mutable reference to the scheduler of the active frame.
    fn active_scheduler_mut(&mut self) -> &mut Scheduler {
        match self.state.call_stack.last_mut() {
            Some(frame) if frame.function.is_some() => &mut frame.function.as_mut().unwrap().scheduler,
            _ => &mut self.scheduler,
        }
    }

    /// Number of function frames currently on the stack.
    fn function_depth(&self) -> u32 {
        self.state.call_stack.iter().filter(|f| f.function.is_some()).count() as u32
    }

    /// Enters the function named by `node.data.function`, binding signature
    /// inputs to the caller's data input values and pushing a new frame.
    async fn enter_function(
        &mut self,
        caller_id: NodeId,
        caller_node: &Node,
        inputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        let name = caller_node
            .data
            .get("function")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DaemonError::Execution("CallFunction requires data.function".to_string()))?;
        let entry = self.registry.function(name).ok_or_else(|| {
            DaemonError::Execution(format!("function '{name}' is not registered"))
        })?;
        if self.function_depth() >= MAX_FUNCTION_DEPTH {
            return Err(DaemonError::Execution(
                "function nesting depth limit exceeded".to_string(),
            ));
        }
        if has_exec_cycle(&entry.body) {
            return Err(DaemonError::Execution(format!(
                "function '{name}' contains an execution cycle"
            )));
        }
        let entry_node = entry
            .body
            .nodes
            .iter()
            .find(|n| n.kind == metteur_shared::FUNCTION_ENTRY_KIND)
            .cloned()
            .ok_or_else(|| DaemonError::Execution(format!("function '{name}' has no entry node")))?;
        // Bind each signature input to the caller's matching data pin.
        for fp in &entry.signature.inputs {
            let caller_pin = caller_node
                .pins
                .iter()
                .find(|p| p.name == fp.name && p.pin_type == PinType::DataInput)
                .ok_or_else(|| {
                    DaemonError::Execution(format!(
                        "CallFunction '{name}' is missing input pin '{}'",
                        fp.name
                    ))
                })?;
            let value = inputs.get(&caller_pin.id).cloned().ok_or_else(|| {
                DaemonError::Execution(format!("CallFunction '{name}' input '{}' is not wired", fp.name))
            })?;
            let entry_pin = entry_node
                .pins
                .iter()
                .find(|p| p.name == fp.name && p.pin_type == PinType::DataOutput)
                .ok_or_else(|| {
                    DaemonError::Execution(format!(
                        "function '{name}' entry lacks output pin '{}'",
                        fp.name
                    ))
                })?;
            self.state.data_values.insert(entry_pin.id, value);
        }

        let mut sched = Scheduler::default();
        sched.seed(entry_node.id);
        self.active_scheduler_mut().mark_executed(caller_id);
        self.state.call_stack.push(Frame {
            node_id: caller_id,
            pc: 0,
            function: Some(FunctionBody {
                id: entry.id,
                scheduler: sched,
            }),
        });
        ctx.audit(
            "function.enter",
            serde_json::json!({
                "caller": caller_id.to_string(),
                "function": name,
                "depth": self.function_depth(),
            }),
        );
        Ok(())
    }

    /// Pops a drained function frame, mapping its exit data inputs onto the
    /// caller's data output pins and driving the caller's execution edges.
    async fn finish_function(
        &mut self,
        frame: &Frame,
        body: &Blueprint,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        let caller_id = frame.node_id;
        let exit_node = body
            .nodes
            .iter()
            .find(|n| n.kind == metteur_shared::FUNCTION_EXIT_KIND)
            .cloned()
            .ok_or_else(|| DaemonError::Execution("function body has no exit node".to_string()))?;
        let exit_inputs = self.gather_inputs(body, exit_node.id)?;

        // Resolve the caller node in the outer blueprint.
        let outer_bp = match self.active_function() {
            Some(fn_id) => self
                .registry
                .function_by_id(fn_id)
                .ok_or_else(|| DaemonError::Execution(format!("function {fn_id} not in registry")))?
                .body,
            None => {
                let root = self
                    .shared_blueprint
                    .clone()
                    .ok_or_else(|| DaemonError::Execution("no root blueprint".to_string()))?;
                root.read().clone()
            }
        };
        let caller = outer_bp
            .node(caller_id)
            .cloned()
            .ok_or_else(|| DaemonError::Execution(format!("caller node {caller_id} not found")))?;

        // Map exit data inputs to the caller's data output pins by name.
        let mut caller_outputs: Vec<(PinId, Value)> = Vec::new();
        for pin in &exit_node.pins {
            if pin.pin_type != PinType::DataInput {
                continue;
            }
            let value = exit_inputs.get(&pin.id).cloned().ok_or_else(|| {
                DaemonError::Execution(format!("missing function output '{}'", pin.name))
            })?;
            let out_pin = caller
                .pins
                .iter()
                .find(|p| p.name == pin.name && p.pin_type == PinType::DataOutput)
                .ok_or_else(|| {
                    DaemonError::Execution(format!(
                        "CallFunction caller lacks output pin '{}'",
                        pin.name
                    ))
                })?;
            self.state.data_values.insert(out_pin.id, value.clone());
            caller_outputs.push((out_pin.id, value));
        }

        self.emit(ExecutionEvent::NodeFinished {
            node_id: caller_id,
        });
        self.emit(ExecutionEvent::NodeData {
            node_id: caller_id,
            outputs: caller_outputs,
            function: None,
        });
        ctx.audit(
            "node.finished",
            serde_json::json!({ "node_id": caller_id.to_string(), "kind": "CallFunction" }),
        );
        self.write_checkpoint(ctx);
        self.fire_edges(&outer_bp, caller_id)
    }

    /// Follows a node's execution output edges and wakes data consumers in
    /// the active frame.
    fn fire_edges(&mut self, blueprint: &Blueprint, node_id: NodeId) -> DaemonResult<()> {
        let next_nodes = self.follow_exec_edges(blueprint, node_id)?;
        let ready_consumers: Vec<NodeId> = self
            .data_targets(blueprint, node_id)
            .into_iter()
            .filter(|t| self.data_inputs_ready(blueprint, *t))
            .collect();
        let sched = self.active_scheduler_mut();
        for next in next_nodes {
            if !sched.is_executed(next) {
                sched.enqueue(next);
            }
        }
        for target in ready_consumers {
            if sched.is_triggered(target) && !sched.is_executed(target) {
                sched.enqueue(target);
            }
        }
        Ok(())
    }

    /// Returns whether all data inputs of `node_id` have produced values.
    fn data_inputs_ready(&self, blueprint: &Blueprint, node_id: NodeId) -> bool {
        blueprint.incoming_edges(node_id).iter().all(|edge| {
            let source_pin = match blueprint.pin(edge.source_pin) {
                Some(pin) => pin,
                None => return false,
            };
            if source_pin.pin_type != PinType::DataOutput {
                return true;
            }
            self.state.data_values.contains_key(&edge.source_pin)
        })
    }

    /// Returns the nodes that consume data produced by `node_id`.
    fn data_targets(&self, blueprint: &Blueprint, node_id: NodeId) -> Vec<NodeId> {
        let mut targets = Vec::new();
        for edge in blueprint.outgoing_edges(node_id) {
            let source_pin = match blueprint.pin(edge.source_pin) {
                Some(pin) => pin,
                None => continue,
            };
            if source_pin.pin_type == PinType::DataOutput {
                targets.push(edge.target_node);
            }
        }
        targets
    }

    /// Gathers the data input values for a node from its incoming data edges.
    fn gather_inputs(
        &self,
        blueprint: &Blueprint,
        node_id: NodeId,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut inputs = HashMap::new();
        for edge in blueprint.incoming_edges(node_id) {
            let source_pin = blueprint
                .pin(edge.source_pin)
                .ok_or_else(|| DaemonError::Execution("unknown source pin".to_string()))?;
            if source_pin.pin_type != PinType::DataOutput {
                continue;
            }
            let value = self.state.data_values.get(&edge.source_pin).cloned().ok_or_else(|| {
                DaemonError::Execution(format!(
                    "missing data value for pin {} while gathering inputs of node {node_id}",
                    edge.source_pin
                ))
            })?;
            inputs.insert(edge.target_pin, value);
        }
        Ok(inputs)
    }

    /// Returns the target nodes of a node's execution output edges.
    ///
    /// For a `Branch` node, only the output edge matching the branch condition
    /// is followed (output pins named `True` or `False`).
    fn follow_exec_edges(
        &self,
        blueprint: &Blueprint,
        node_id: NodeId,
    ) -> DaemonResult<Vec<NodeId>> {
        let node = blueprint
            .node(node_id)
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;

        let branch_take = if node.kind == "Branch" {
            let result = self.branch_result(blueprint, node_id)?;
            Some(if result {
                "True"
            } else {
                "False"
            })
        } else {
            None
        };

        let mut next = Vec::new();
        for edge in blueprint.outgoing_edges(node_id) {
            let source_pin = blueprint
                .pin(edge.source_pin)
                .ok_or_else(|| DaemonError::Execution("unknown source pin".to_string()))?;
            if source_pin.pin_type != PinType::ExecOutput {
                continue;
            }
            if let Some(take) = branch_take
                && source_pin.name != take
            {
                continue;
            }
            next.push(edge.target_node);
        }
        Ok(next)
    }

    /// Reads the boolean result of a `Branch` node from its `Result` pin.
    fn branch_result(&self, blueprint: &Blueprint, node_id: NodeId) -> DaemonResult<bool> {
        let node = blueprint
            .node(node_id)
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;
        let pin =
            node.pins.iter().find(|p| p.name == "Result").ok_or_else(|| {
                DaemonError::Execution("branch node missing Result pin".to_string())
            })?;
        self.state
            .data_values
            .get(&pin.id)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| DaemonError::Execution("branch node missing Result value".to_string()))
    }
}

/// Returns whether a validator/judge node output reports failure.
fn circuit_failed(node: &Node, outputs: &HashMap<PinId, Value>) -> bool {
    if !matches!(node.kind.as_str(), "Validator" | "Judge") {
        return false;
    }
    let result_pin = if node.kind == "Validator" { "Passed" } else { "Success" };
    outputs.iter().any(|(id, v)| {
        node.pins.iter().any(|p| p.id == *id && p.name == result_pin) && v.as_bool() == Some(false)
    })
}

/// Returns whether the blueprint's execution graph contains a cycle.
///
/// The graph is built from execution output edges; a cycle means execution
/// could never terminate.
fn has_exec_cycle(blueprint: &Blueprint) -> bool {
    // 0 = unvisited, 1 = in progress, 2 = done.
    let mut state: HashMap<NodeId, u8> = HashMap::new();
    for node in &blueprint.nodes {
        if visit_exec(node.id, blueprint, &mut state) {
            return true;
        }
    }
    false
}

/// Depth-first visit of the execution graph; returns true on a cycle.
fn visit_exec(node_id: NodeId, blueprint: &Blueprint, state: &mut HashMap<NodeId, u8>) -> bool {
    match state.get(&node_id) {
        Some(1) => return true,
        Some(2) => return false,
        _ => {}
    }
    state.insert(node_id, 1);
    for edge in blueprint.outgoing_edges(node_id) {
        let source_pin = match blueprint.pin(edge.source_pin) {
            Some(pin) => pin,
            None => continue,
        };
        if source_pin.pin_type != PinType::ExecOutput {
            continue;
        }
        if visit_exec(edge.target_node, blueprint, state) {
            return true;
        }
    }
    state.insert(node_id, 2);
    false
}

/// Returns the current time in milliseconds since the Unix epoch.
fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_shared::model::function::FunctionEntry;
    use metteur_shared::{DataType, Edge, Node, NodeType, Pin, PinType};
    use uuid::Uuid;

    fn new_interpreter() -> Interpreter {
        Interpreter::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            std::env::temp_dir(),
        )
    }

    /// Wraps a blueprint in the shared handle used by the interpreter.
    fn shared(bp: Blueprint) -> Arc<PLock<Blueprint>> {
        Arc::new(PLock::new(bp))
    }

    /// Builds a blueprint: Start -> Add(A=2, B=3) -> Judge(Score=Result).
    fn build_blueprint() -> Blueprint {
        let start = Uuid::new_v4();
        let add = Uuid::new_v4();
        let judge = Uuid::new_v4();

        let start_exec_out = Uuid::new_v4();
        let start_a = Uuid::new_v4();
        let start_b = Uuid::new_v4();
        let add_exec_in = Uuid::new_v4();
        let add_exec_out = Uuid::new_v4();
        let add_a = Uuid::new_v4();
        let add_b = Uuid::new_v4();
        let add_result = Uuid::new_v4();
        let judge_exec_in = Uuid::new_v4();
        let judge_score = Uuid::new_v4();
        let judge_success = Uuid::new_v4();

        let nodes = vec![
            Node {
                id: start,
                node_type: NodeType::Event,
                kind: "Start".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    Pin {
                        id: start_exec_out,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecOutput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: start_a,
                        name: "A".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: start_b,
                        name: "B".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                    },
                ],
                data: serde_json::json!({ "A": 2, "B": 3 }),
            },
            Node {
                id: add,
                node_type: NodeType::Pure,
                kind: "Add".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    Pin {
                        id: add_exec_in,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecInput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: add_exec_out,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecOutput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: add_a,
                        name: "A".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: add_b,
                        name: "B".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: add_result,
                        name: "Result".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                    },
                ],
                data: serde_json::Value::Null,
            },
            Node {
                id: judge,
                node_type: NodeType::Function,
                kind: "Judge".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    Pin {
                        id: judge_exec_in,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecInput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: judge_score,
                        name: "Score".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: judge_success,
                        name: "Success".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Bool,
                    },
                ],
                data: serde_json::Value::Null,
            },
        ];

        let edges = vec![
            Edge {
                id: Uuid::new_v4(),
                source_node: start,
                source_pin: start_exec_out,
                target_node: add,
                target_pin: add_exec_in,
            },
            Edge {
                id: Uuid::new_v4(),
                source_node: add,
                source_pin: add_exec_out,
                target_node: judge,
                target_pin: judge_exec_in,
            },
            Edge {
                id: Uuid::new_v4(),
                source_node: add,
                source_pin: add_result,
                target_node: judge,
                target_pin: judge_score,
            },
            Edge {
                id: Uuid::new_v4(),
                source_node: start,
                source_pin: start_a,
                target_node: add,
                target_pin: add_a,
            },
            Edge {
                id: Uuid::new_v4(),
                source_node: start,
                source_pin: start_b,
                target_node: add,
                target_pin: add_b,
            },
        ];

        Blueprint {
            id: Uuid::new_v4(),
            name: "test".to_string(),
            nodes,
            edges,
            entry_node_id: start,
        }
    }

    #[tokio::test]
    async fn executes_arithmetic_chain() {
        let blueprint = build_blueprint();
        let mut interpreter = new_interpreter();
        let events = interpreter.run(&shared(blueprint), None).await.unwrap();
        // Start, Add, Judge each emit started + finished + node_data.
        assert_eq!(events.len(), 9);
        assert!(matches!(events[0], ExecutionEvent::NodeStarted { .. }));
    }

    #[tokio::test]
    async fn run_resets_state_on_reuse() {
        let blueprint = build_blueprint();
        let mut interpreter = new_interpreter();
        let first = interpreter.run(&shared(blueprint.clone()), None).await.unwrap();
        let second = interpreter.run(&shared(blueprint), None).await.unwrap();
        assert_eq!(first.len(), second.len());
        assert_eq!(first.len(), 9);
    }

    /// Builds a pure `AddFunc` function: entry(A, B) -> Add -> exit(Result).
    fn add_function() -> FunctionEntry {
        use metteur_shared::model::function::{FnPin, FunctionEntry, FunctionSignature};
        let (id, entry_node, add, exit_node) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (entry_exec, entry_a, entry_b, add_exin, add_exout, add_a, add_b, add_res, exit_exin, exit_res) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
        };
        FunctionEntry {
            id,
            name: "AddFunc".to_string(),
            description: "Adds two numbers".to_string(),
            signature: FunctionSignature {
                inputs: vec![
                    FnPin { name: "A".to_string(), data_type: DataType::Float, description: None },
                    FnPin { name: "B".to_string(), data_type: DataType::Float, description: None },
                ],
                outputs: vec![
                    FnPin { name: "Result".to_string(), data_type: DataType::Float, description: None },
                ],
            },
            body: Blueprint {
                id,
                name: "AddFunc".to_string(),
                nodes: vec![
                    Node {
                        id: entry_node,
                        node_type: NodeType::Event,
                        kind: "FunctionEntry".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![
                            pin(entry_exec, "Exec", PinType::ExecOutput, DataType::Void),
                            pin(entry_a, "A", PinType::DataOutput, DataType::Float),
                            pin(entry_b, "B", PinType::DataOutput, DataType::Float),
                        ],
                        data: serde_json::Value::Null,
                    },
                    Node {
                        id: add,
                        node_type: NodeType::Pure,
                        kind: "Add".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![
                            pin(add_exin, "Exec", PinType::ExecInput, DataType::Void),
                            pin(add_exout, "Exec", PinType::ExecOutput, DataType::Void),
                            pin(add_a, "A", PinType::DataInput, DataType::Float),
                            pin(add_b, "B", PinType::DataInput, DataType::Float),
                            pin(add_res, "Result", PinType::DataOutput, DataType::Float),
                        ],
                        data: serde_json::Value::Null,
                    },
                    Node {
                        id: exit_node,
                        node_type: NodeType::Event,
                        kind: "FunctionExit".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![
                            pin(exit_exin, "Exec", PinType::ExecInput, DataType::Void),
                            pin(exit_res, "Result", PinType::DataInput, DataType::Float),
                        ],
                        data: serde_json::Value::Null,
                    },
                ],
                edges: vec![
                    Edge { id: Uuid::new_v4(), source_node: entry_node, source_pin: entry_exec, target_node: add, target_pin: add_exin },
                    Edge { id: Uuid::new_v4(), source_node: add, source_pin: add_exout, target_node: exit_node, target_pin: exit_exin },
                    Edge { id: Uuid::new_v4(), source_node: entry_node, source_pin: entry_a, target_node: add, target_pin: add_a },
                    Edge { id: Uuid::new_v4(), source_node: entry_node, source_pin: entry_b, target_node: add, target_pin: add_b },
                    Edge { id: Uuid::new_v4(), source_node: add, source_pin: add_res, target_node: exit_node, target_pin: exit_res },
                ],
                entry_node_id: entry_node,
            },
            source: metteur_shared::model::function::FunctionSource::Builtin,
        }
    }

    /// Builds Start -> CallFunction(AddFunc) with A=5, B=3 directly wired.
    fn call_function_blueprint(func_name: &str) -> Blueprint {
        let (start, caller) = (Uuid::new_v4(), Uuid::new_v4());
        let (start_ex, start_a, start_b, caller_exin, caller_exout, caller_a, caller_b, caller_res) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
             id: p_id,
             name: name.to_string(),
             pin_type,
             data_type,
         };
         Blueprint {
             id: Uuid::new_v4(),
             name: "call-fn".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(start_ex, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(start_a, "A", PinType::DataOutput, DataType::Float),
                        pin(start_b, "B", PinType::DataOutput, DataType::Float),
                    ],
                    data: serde_json::json!({ "A": 5, "B": 3 }),
                },
                Node {
                    id: caller,
                    node_type: NodeType::Function,
                    kind: "CallFunction".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(caller_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(caller_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(caller_a, "A", PinType::DataInput, DataType::Float),
                        pin(caller_b, "B", PinType::DataInput, DataType::Float),
                        pin(caller_res, "Result", PinType::DataOutput, DataType::Float),
                    ],
                    data: serde_json::json!({ "function": func_name }),
                },
            ],
            edges: vec![
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_ex, target_node: caller, target_pin: caller_exin },
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_a, target_node: caller, target_pin: caller_a },
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_b, target_node: caller, target_pin: caller_b },
            ],
            entry_node_id: start,
        }
    }

    #[tokio::test]
    async fn executes_function_call_via_frames() {
        let func = add_function();
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func.clone());
        let blueprint = call_function_blueprint("AddFunc");
        let mut interpreter = Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        let events = interpreter.run(&shared(blueprint.clone()), None).await.unwrap();

        // Caller result is 5 + 3 = 8 and is reported through a NodeData event.
        let data = events
            .iter()
            .find_map(|e| match e {
                ExecutionEvent::NodeData {
                    node_id, outputs, ..
                } if blueprint
                    .node(*node_id)
                    .map(|n| n.kind == "CallFunction")
                    .unwrap_or(false) =>
                {
                    Some((*node_id, outputs.clone()))
                }
                _ => None,
            })
            .expect("caller produces node data");
        let caller = blueprint.nodes.iter().find(|n| n.kind == "CallFunction").unwrap();
        let result_pin = caller.pins.iter().find(|p| p.name == "Result").unwrap().id;
        let value = data.1.iter().find(|(id, _)| *id == result_pin).map(|(_, v)| v).unwrap();
        assert_eq!(value.as_float(), Some(8.0));

        // The inner Add node reports its origin function for audit grouping.
        let func_id = func.id;
        assert!(events.iter().any(|e| matches!(
            e,
            ExecutionEvent::NodeData { function: Some(id), .. } if *id == func_id
        )));
    }

    #[tokio::test]
    async fn function_recursion_is_depth_limited() {
        // Selfish: FunctionEntry -> CallFunction(Selfish) -> FunctionExit.
        let (id, entry_node, caller, exit_node) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (entry_exec, caller_exin, caller_exout, exit_exin) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType| Pin { id: p_id, name: name.to_string(), pin_type, data_type: DataType::Void };
        let func = FunctionEntry {
            id,
            name: "Selfish".to_string(),
            description: String::new(),
            signature: metteur_shared::model::function::FunctionSignature::default(),
            body: Blueprint {
                id,
                name: "Selfish".to_string(),
                nodes: vec![
                    Node { id: entry_node, node_type: NodeType::Event, kind: "FunctionEntry".to_string(), position: (0.0, 0.0), pins: vec![pin(entry_exec, "Exec", PinType::ExecOutput)], data: serde_json::Value::Null },
                    Node { id: caller, node_type: NodeType::Function, kind: "CallFunction".to_string(), position: (0.0, 0.0), pins: vec![pin(caller_exin, "Exec", PinType::ExecInput), pin(caller_exout, "Exec", PinType::ExecOutput)], data: serde_json::json!({ "function": "Selfish" }) },
                    Node { id: exit_node, node_type: NodeType::Event, kind: "FunctionExit".to_string(), position: (0.0, 0.0), pins: vec![pin(exit_exin, "Exec", PinType::ExecInput)], data: serde_json::Value::Null },
                ],
                edges: vec![
                    Edge { id: Uuid::new_v4(), source_node: entry_node, source_pin: entry_exec, target_node: caller, target_pin: caller_exin },
                    Edge { id: Uuid::new_v4(), source_node: caller, source_pin: caller_exout, target_node: exit_node, target_pin: exit_exin },
                ],
                entry_node_id: entry_node,
            },
            source: metteur_shared::model::function::FunctionSource::Builtin,
        };
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func.clone());
        let blueprint = call_function_blueprint("Selfish");
        let mut interpreter = Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("depth limit exceeded")
        ));
    }

    #[tokio::test]
    async fn resumes_with_function_frame_from_checkpoint() {
        let func = add_function();
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func.clone());
        let blueprint = call_function_blueprint("AddFunc");
        let start = blueprint.entry_node_id;
        let caller = blueprint.nodes.iter().find(|n| n.kind == "CallFunction").unwrap().id;
        let start_a = blueprint.nodes.iter().find(|n| n.id == start).unwrap().pins.iter().find(|p| p.name == "A").unwrap().id;
        let start_b = blueprint.nodes.iter().find(|n| n.id == start).unwrap().pins.iter().find(|p| p.name == "B").unwrap().id;
        let entry_node = func.body.entry_node_id;
        let entry_node_obj = func.body.nodes.iter().find(|n| n.id == entry_node).unwrap();
        let entry_a = entry_node_obj.pins.iter().find(|p| p.name == "A").unwrap().id;
        let entry_b = entry_node_obj.pins.iter().find(|p| p.name == "B").unwrap().id;

        let mut frame_sched = Scheduler::default();
        frame_sched.seed(entry_node);
        let checkpoint = ExecutionCheckpoint {
            run_id: Uuid::new_v4(),
            blueprint_id: blueprint.id,
            status: RunStatus::Running,
            started_at: 0,
            updated_at: 0,
            call_stack: vec![
                Frame { node_id: start, pc: 0, function: None },
                Frame { node_id: caller, pc: 0, function: Some(FunctionBody { id: func.id, scheduler: frame_sched }) },
            ],
            data_values: HashMap::from([
                (start_a, Value::Int(5)),
                (start_b, Value::Int(3)),
                (entry_a, Value::Int(5)),
                (entry_b, Value::Int(3)),
            ]),
            executed: vec![start, caller],
            pending: vec![caller],
            triggered: vec![start, caller],
            transaction_log: Vec::new(),
            error: None,
        };
        let mut interpreter = Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        let events = interpreter
            .resume_with_control(
                &shared(blueprint),
                checkpoint,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        // Function entry/exit (2 each) + inner Add (3) + caller completion (2).
        assert_eq!(events.len(), 9);
    }

    #[tokio::test]
    async fn circuit_breaker_aborts_when_user_denies() {
        use metteur_shared::config::{Config, ExecutionConfig};
        use crate::sandbox::approval::{ApprovalBroker, Decision};

        let broker = Arc::new(ApprovalBroker::new());
        let config = Arc::new(RwLock::new(Config {
            execution: ExecutionConfig {
                circuit_break_after: 1,
            },
            ..Default::default()
        }));

        // Start(A=0, Expected=5) -> Validator(mode eq): A != Expected always.
        let (start, validator) = (Uuid::new_v4(), Uuid::new_v4());
        let (start_ex, start_a, start_exp, v_exin, v_actual, v_expected, v_passed) = (
            Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(),
            Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
        };
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "breaker".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(start_ex, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(start_a, "A", PinType::DataOutput, DataType::Float),
                        pin(start_exp, "Expected", PinType::DataOutput, DataType::Float),
                    ],
                    data: serde_json::json!({ "A": 0, "Expected": 5 }),
                },
                Node {
                    id: validator,
                    node_type: NodeType::Function,
                    kind: "Validator".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(v_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(v_actual, "Actual", PinType::DataInput, DataType::Float),
                        pin(v_expected, "Expected", PinType::DataInput, DataType::Float),
                        pin(v_passed, "Passed", PinType::DataOutput, DataType::Bool),
                    ],
                    data: serde_json::json!({ "mode": "eq" }),
                },
            ],
            edges: vec![
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_ex, target_node: validator, target_pin: v_exin },
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_a, target_node: validator, target_pin: v_actual },
                Edge { id: Uuid::new_v4(), source_node: start, source_pin: start_exp, target_node: validator, target_pin: v_expected },
            ],
            entry_node_id: start,
        };

        let mut interpreter = Interpreter::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            std::env::temp_dir(),
        )
        .with_config(config)
        .with_approvals(broker.clone());
        let blueprint_arc = Arc::new(PLock::new(blueprint));
        let task = tokio::spawn(async move { interpreter.run(&blueprint_arc, None).await });
        // Deny the circuit-tripped approval as soon as it appears.
        loop {
            let ids = broker.pending_ids();
            if !ids.is_empty() {
                broker.respond(&ids[0], Decision::Deny);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let result = task.await.unwrap();
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("aborted by user")
        ));
    }

    #[tokio::test]
    async fn detects_execution_cycle() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let a_exec_out = Uuid::new_v4();
        let a_exec_in = Uuid::new_v4();
        let b_exec_out = Uuid::new_v4();
        let b_exec_in = Uuid::new_v4();

        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "cycle".to_string(),
            nodes: vec![
                Node {
                    id: a,
                    node_type: NodeType::Function,
                    kind: "CallLLM".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        Pin {
                            id: a_exec_in,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecInput,
                            data_type: DataType::Void,
                        },
                        Pin {
                            id: a_exec_out,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecOutput,
                            data_type: DataType::Void,
                        },
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: b,
                    node_type: NodeType::Function,
                    kind: "CallLLM".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        Pin {
                            id: b_exec_in,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecInput,
                            data_type: DataType::Void,
                        },
                        Pin {
                            id: b_exec_out,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecOutput,
                            data_type: DataType::Void,
                        },
                    ],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: a,
                    source_pin: a_exec_out,
                    target_node: b,
                    target_pin: b_exec_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: b,
                    source_pin: b_exec_out,
                    target_node: a,
                    target_pin: a_exec_in,
                },
            ],
            entry_node_id: a,
        };

        let mut interpreter = new_interpreter();
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn executes_diamond_blueprint() {
        // Start branches to Add1 and Add2, which merge into Add3. Add3
        // consumes data produced by both branches, so it must wait for both.
        let start = Uuid::new_v4();
        let add1 = Uuid::new_v4();
        let add2 = Uuid::new_v4();
        let add3 = Uuid::new_v4();

        let start_exec = Uuid::new_v4();
        let start_a = Uuid::new_v4();
        let start_b = Uuid::new_v4();

        let add1_exec_in = Uuid::new_v4();
        let add1_exec_out = Uuid::new_v4();
        let add1_a = Uuid::new_v4();
        let add1_b = Uuid::new_v4();
        let add1_result = Uuid::new_v4();

        let add2_exec_in = Uuid::new_v4();
        let add2_exec_out = Uuid::new_v4();
        let add2_a = Uuid::new_v4();
        let add2_b = Uuid::new_v4();
        let add2_result = Uuid::new_v4();

        let add3_exec_in = Uuid::new_v4();
        let add3_exec_out = Uuid::new_v4();
        let add3_a = Uuid::new_v4();
        let add3_b = Uuid::new_v4();
        let add3_result = Uuid::new_v4();

        let add_node =
            |id: Uuid, exec_in: Uuid, exec_out: Uuid, a: Uuid, b: Uuid, result: Uuid| Node {
                id,
                node_type: NodeType::Pure,
                kind: "Add".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    Pin {
                        id: exec_in,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecInput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: exec_out,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecOutput,
                        data_type: DataType::Void,
                    },
                    Pin {
                        id: a,
                        name: "A".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: b,
                        name: "B".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                    },
                    Pin {
                        id: result,
                        name: "Result".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                    },
                ],
                data: serde_json::Value::Null,
            };

        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "diamond".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        Pin {
                            id: start_exec,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecOutput,
                            data_type: DataType::Void,
                        },
                        Pin {
                            id: start_a,
                            name: "A".to_string(),
                            pin_type: PinType::DataOutput,
                            data_type: DataType::Float,
                        },
                        Pin {
                            id: start_b,
                            name: "B".to_string(),
                            pin_type: PinType::DataOutput,
                            data_type: DataType::Float,
                        },
                    ],
                    data: serde_json::json!({ "A": 2, "B": 3 }),
                },
                add_node(add1, add1_exec_in, add1_exec_out, add1_a, add1_b, add1_result),
                add_node(add2, add2_exec_in, add2_exec_out, add2_a, add2_b, add2_result),
                add_node(add3, add3_exec_in, add3_exec_out, add3_a, add3_b, add3_result),
            ],
            edges: vec![
                // Exec: Start -> Add1, Start -> Add2, Add1 -> Add3, Add2 -> Add3.
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_exec,
                    target_node: add1,
                    target_pin: add1_exec_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_exec,
                    target_node: add2,
                    target_pin: add2_exec_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: add1,
                    source_pin: add1_exec_out,
                    target_node: add3,
                    target_pin: add3_exec_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: add2,
                    source_pin: add2_exec_out,
                    target_node: add3,
                    target_pin: add3_exec_in,
                },
                // Data: Start -> Add1/Add2, Add1/Add2 -> Add3.
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_a,
                    target_node: add1,
                    target_pin: add1_a,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_b,
                    target_node: add1,
                    target_pin: add1_b,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_a,
                    target_node: add2,
                    target_pin: add2_a,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_b,
                    target_node: add2,
                    target_pin: add2_b,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: add1,
                    source_pin: add1_result,
                    target_node: add3,
                    target_pin: add3_a,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: add2,
                    source_pin: add2_result,
                    target_node: add3,
                    target_pin: add3_b,
                },
            ],
            entry_node_id: start,
        };

        let mut interpreter = new_interpreter();
        let events = interpreter.run(&shared(blueprint), None).await.unwrap();
        // Start, Add1, Add2, Add3 each emit started + finished + node_data.
        assert_eq!(events.len(), 12);
        // Add3 must have executed, which requires both branches to have run.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ExecutionEvent::NodeFinished { node_id } if *node_id == add3))
        );
    }

    /// An in-memory checkpoint sink for tests.
    #[derive(Default)]
    struct MemorySink {
        run_id: uuid::Uuid,
        checkpoints: std::sync::Mutex<Vec<ExecutionCheckpoint>>,
    }

    impl MemorySink {
        fn new() -> Self {
            Self {
                run_id: Uuid::new_v4(),
                checkpoints: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl CheckpointSink for MemorySink {
        fn run_id(&self) -> uuid::Uuid {
            self.run_id
        }

        fn write(&self, checkpoint: &ExecutionCheckpoint) -> DaemonResult<()> {
            self.checkpoints.lock().unwrap().push(checkpoint.clone());
            Ok(())
        }
    }

    /// Builds a checkpoint representing the run state right after Start ran.
    fn checkpoint_after_start(blueprint: &Blueprint) -> ExecutionCheckpoint {
        let start = blueprint.entry_node_id;
        let start_node = blueprint.node(start).unwrap();
        let start_a = start_node.pins.iter().find(|p| p.name == "A").unwrap().id;
        let start_b = start_node.pins.iter().find(|p| p.name == "B").unwrap().id;
        let add = blueprint.nodes.iter().find(|n| n.kind == "Add").unwrap().id;
        ExecutionCheckpoint {
            run_id: Uuid::new_v4(),
            blueprint_id: blueprint.id,
            status: RunStatus::Running,
            started_at: 0,
            updated_at: 0,
            call_stack: Vec::new(),
            data_values: HashMap::from([(start_a, Value::Int(2)), (start_b, Value::Int(3))]),
            executed: vec![start],
            pending: vec![add],
            triggered: vec![start, add],
            transaction_log: Vec::new(),
            error: None,
        }
    }

    #[tokio::test]
    async fn resume_continues_from_checkpoint() {
        let blueprint = build_blueprint();
        let checkpoint = checkpoint_after_start(&blueprint);
        let mut interpreter = new_interpreter();
        let events = interpreter
            .resume_with_control(
                &shared(blueprint),
                checkpoint,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        // Add and Judge are the remaining nodes (started + finished + data).
        assert_eq!(events.len(), 6);
    }

    #[tokio::test]
    async fn resume_rejects_non_resumable_run() {
        let blueprint = build_blueprint();
        let mut checkpoint = checkpoint_after_start(&blueprint);
        checkpoint.status = RunStatus::Completed;
        let mut interpreter = new_interpreter();
        let result = interpreter
            .resume_with_control(
                &shared(blueprint),
                checkpoint,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn writes_checkpoints_through_sink() {
        let blueprint = build_blueprint();
        let sink = Arc::new(MemorySink::new());
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        interpreter.run(&shared(blueprint), None).await.unwrap();
        let checkpoints = sink.checkpoints.lock().unwrap();
        // Initial + one per node (3) + terminal.
        assert_eq!(checkpoints.len(), 5);
        assert_eq!(checkpoints.last().unwrap().status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn cancel_marks_run_failed() {
        let blueprint = build_blueprint();
        let sink = Arc::new(MemorySink::new());
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        let result = interpreter
            .run_with_control(
                &shared(blueprint),
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                cancel,
            )
            .await;
        assert!(result.is_err());
        let checkpoints = sink.checkpoints.lock().unwrap();
        assert_eq!(checkpoints.last().unwrap().status, RunStatus::Failed);
    }
}
