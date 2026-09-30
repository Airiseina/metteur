//! The blueprint interpreter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use metteur_shared::config::Config;
use metteur_shared::{Blueprint, Node, NodeId, PinId, PinType, Value};
use parking_lot::RwLock as PLock;
use tokio::sync::RwLock;

use crate::error::{DaemonError, DaemonResult};
use crate::llm::LlmClientFactory;
use crate::observability::audit::AuditWriter;
use crate::registry::Registry;

use super::checkpoint::{CheckpointSink, ExecutionCheckpoint, RunStatus};
use super::context::{
    ExecutionContext, ExecutionState, ForEachState, Frame, FunctionBody, RetryMark, Scheduler,
};
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
    /// The agent's task list changed.
    Todos {
        node_id: NodeId,
        todos: Vec<metteur_shared::llm::TodoItem>,
    },
    /// A background command changed state.
    ///
    /// Emitted when a job is started and when its completion is observed (by a
    /// wait, or by the engine waking a parked turn); a job that finishes while
    /// nobody is looking is reported the next time it is observed.
    Job {
        node_id: NodeId,
        job_id: String,
        /// `started` or `finished`.
        state: String,
        /// One-line summary: job id, state, duration, size, command.
        summary: String,
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
    version_manager: Option<Arc<crate::storage::versioning::VersionManager>>,
    jobs: Option<Arc<crate::execution::JobManager>>,
    shared_blueprint: Option<SharedBlueprint>,
    circuit_failures: u32,
    /// Whether a cancelled run rolls its file mutations back before exiting.
    ///
    /// Command side effects stay outside the WAL either way; this only governs
    /// the file mutations the run recorded.
    rollback_on_cancel: bool,
    tree: super::tree::ExecTree,
    /// The run root of the tree, if the run started one.
    tree_root: Option<String>,
    /// Tree ids of entered function frames, innermost last.
    frame_trees: Vec<String>,
    /// Tree id of the node currently executing, if any.
    current_tree: Option<String>,
    /// Task list restored from a checkpoint, applied to the first context built
    /// for the resumed run (the list lives on the context, not the scheduler).
    resume_todos: Vec<metteur_shared::llm::TodoItem>,
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
            version_manager: None,
            jobs: None,
            shared_blueprint: None,
            circuit_failures: 0,
            rollback_on_cancel: true,
            tree: super::tree::ExecTree::new(),
            tree_root: None,
            frame_trees: Vec::new(),
            current_tree: None,
            resume_todos: Vec::new(),
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

    /// Resolves whether a cancelled run should roll its mutations back,
    /// preferring the live config over the value captured at build time.
    fn should_rollback_on_cancel(&self, ctx: &ExecutionContext) -> bool {
        ctx.config
            .as_ref()
            .and_then(|c| c.try_read().ok())
            .map(|cfg| cfg.execution.rollback_on_cancel)
            .unwrap_or(self.rollback_on_cancel)
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

    /// Attaches the workspace version manager, enabling snapshot tools.
    pub fn with_version_manager(
        mut self,
        version_manager: Arc<crate::storage::versioning::VersionManager>,
    ) -> Self {
        self.version_manager = Some(version_manager);
        self
    }

    /// Shares the workspace's background-command manager with the run.
    ///
    /// Job ids stay valid across runs of the workspace, and the manager can be
    /// asked to clean up everything a run started.
    pub fn with_jobs(mut self, jobs: Arc<crate::execution::JobManager>) -> Self {
        self.jobs = Some(jobs);
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
            attempt_counts: resume.attempt_counts,
            validation_marks: resume.validation_marks,
            foreach_stack: resume.foreach_stack.clone(),
        };
        self.scheduler = Scheduler::from_checkpoint(
            resume.executed,
            resume.pending,
            resume.triggered,
            resume.executed_order,
        );
        self.events.clear();
        self.blueprint_id = resume.blueprint_id;
        self.started_at = resume.started_at;
        // The task list lives on the execution context (not the scheduler
        // state), so it is carried into the resumed run separately.
        self.resume_todos = resume.todos.clone();
        // Carried over so a resumed run cannot reset the breaker counter and
        // slip past a replan threshold it had already reached.
        self.circuit_failures = resume.circuit_failures;
        self.tree = resume.exec_tree.clone();
        self.tree_root = resume.exec_tree.roots.first().cloned();
        self.frame_trees = resume.frame_trees.clone();
        self.current_tree = resume.current_tree.clone();

        let mut ctx = self.make_context(interrupts, pause_requested, cancel_requested);
        ctx.transaction_log = TransactionLog::from_entries(resume.transaction_log);
        // Checkpoints predating frame variables resume with an empty stack;
        // restore the root frame so VariableSet has somewhere to write.
        ctx.variables = if resume.variables.is_empty() {
            vec![HashMap::new()]
        } else {
            resume.variables
        };
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
        self.tree = super::tree::ExecTree::new();
        let root =
            self.tree.spawn(None, super::tree::TreeNodeKind::Run, bp.name.clone(), self.started_at);
        self.tree_root = Some(root);
        self.frame_trees.clear();
        self.current_tree = None;
        // A fresh run starts with an empty task list; a stale one would leak
        // into `make_context` (which seeds the list from this field).
        self.resume_todos.clear();
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
        ctx.version_manager = self.version_manager.clone();
        if let Some(jobs) = &self.jobs {
            ctx.jobs = Arc::clone(jobs);
        }
        ctx.todos = self.resume_todos.clone();
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
            executed_order: self.scheduler.order_list(),
            attempt_counts: self.state.attempt_counts.clone(),
            validation_marks: self.state.validation_marks.clone(),
            variables: ctx.variables.clone(),
            foreach_stack: self.state.foreach_stack.clone(),
            todos: ctx.todos.clone(),
            exec_tree: self.tree.clone(),
            frame_trees: self.frame_trees.clone(),
            current_tree: self.current_tree.clone(),
            circuit_failures: self.circuit_failures,
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
        if let Some(root) = self.tree_root.clone() {
            let (status, now) = match &result {
                Ok(()) => (super::tree::TreeNodeStatus::Done, now_millis()),
                Err(err) => (super::tree::TreeNodeStatus::Failed(err.to_string()), now_millis()),
            };
            self.tree.finish(&root, status, now);
        }
        // A cancelled run is not a failure: record it under its own status so a
        // client can tell an abandoned run from a broken one.
        let cancelled = matches!(&result, Err(DaemonError::Interrupted(_)));
        let rollback_on_cancel = self.should_rollback_on_cancel(ctx);
        if cancelled && rollback_on_cancel {
            // Undo the file mutations this run made, so cancelling leaves the
            // workspace as it was found. Command side effects are outside the
            // WAL and are not undone.
            match ctx.transaction_log.rollback_after(0) {
                Ok(undone) => ctx.audit(
                    "execution.cancel_rollback",
                    serde_json::json!({
                        "run_id": ctx.run_id.to_string(),
                        "undone": undone,
                    }),
                ),
                Err(err) => ctx.audit(
                    "execution.cancel_rollback_failed",
                    serde_json::json!({
                        "run_id": ctx.run_id.to_string(),
                        "error": err.to_string(),
                    }),
                ),
            }
        }
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
            Err(_) if cancelled => {
                ctx.audit(
                    "execution.cancelled",
                    serde_json::json!({
                        "run_id": ctx.run_id.to_string(),
                        "blueprint_id": bp_id.to_string(),
                        "rolled_back": rollback_on_cancel,
                    }),
                );
                RunStatus::Cancelled
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
            super::control::gate(ctx).await?;

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
                        // A drained function body first advances an unfinished
                        // ForEach loop of its own frame, if any.
                        if self.advance_foreach(&active_bp, ctx)? {
                            continue;
                        }
                        // The function body drained: pop the frame, map its exit
                        // values back to the caller, and continue.
                        let frame = self.state.call_stack.pop().expect("function frame on top");
                        // Discard the function body's variable frame with it.
                        ctx.variables.pop();
                        // Close the function body's tree node, rewinding the
                        // current position to the caller (CallFunction) node so
                        // `finish_function` closes that one too.
                        if let Some(fn_tree) = self.frame_trees.pop() {
                            self.tree.finish(
                                &fn_tree,
                                super::tree::TreeNodeStatus::Done,
                                now_millis(),
                            );
                            if self.current_tree.as_deref() == Some(fn_tree.as_str()) {
                                self.current_tree =
                                    self.tree.nodes.get(&fn_tree).and_then(|n| n.parent.clone());
                            }
                        }
                        self.finish_function(&frame, &active_bp, ctx).await?;
                        continue;
                    }
                    // A drained root frame advances an unfinished ForEach loop,
                    // if any; otherwise the run is complete.
                    if self.advance_foreach(&active_bp, ctx)? {
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
            self.tree_begin(&node);

            // A CallFunction node hands control to the called body.
            if node.kind == metteur_shared::CALL_FUNCTION_KIND {
                self.enter_function(node_id, &node, &inputs, ctx).await?;
                continue;
            }

            // A ForEach node is driven by the interpreter loop, not by its
            // executor: entering starts the iteration, draining the body
            // advances it (see the queue-empty branch above).
            if node.kind == "ForEach" {
                self.enter_foreach(node_id, &node, &inputs, &active_bp, ctx)?;
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
                self.tree_end(ctx, super::tree::TreeNodeStatus::Done);
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
            self.state.data_values.extend(outputs.iter().map(|(id, v)| (*id, v.clone())));
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
            self.tree_end(ctx, super::tree::TreeNodeStatus::Done);

            // Defer normal interrupts until the next Call LLM node.
            if let Some(bus) = &ctx.interrupts {
                for msg in bus.drain(InterruptPriority::Normal) {
                    ctx.pending_normal.push_back(msg);
                }
            }

            // Validation retry: a failed validator with retry budget rolls
            // back its segment and re-queues it before any successor can be
            // enqueued with stale data. It persists its own checkpoint.
            if self.maybe_retry(node_id, &node, &outputs, ctx)? {
                continue;
            }

            // Fire execution output edges and wake data consumers.
            self.fire_edges(&active_bp, node_id)?;

            // Circuit breaker: repeated validation failures trigger a
            // user-approved replan and re-run this node under the new plan.
            self.maybe_circuit_break(node_id, &node, &outputs, ctx).await?;

            // Persist only once the successors are queued. A checkpoint taken
            // before `fire_edges` would record this node as executed while its
            // successors are in neither `executed` nor `pending`; a crash in
            // that window would resume into a drained queue and report the run
            // as completed with the remaining branch silently skipped.
            self.write_checkpoint(ctx);
        }
        Ok(())
    }

    /// Handles validation retry for a failed validator node.
    ///
    /// Returns `true` when the node will re-run: the file mutations recorded
    /// since its last pass are rolled back (WAL before-images), the executed
    /// segment is re-queued in completion order, and the failure does not
    /// count toward the circuit breaker. Returns `false` when there is no
    /// retry budget left (or none configured) and the circuit breaker should
    /// see the failure. A passing validator refreshes its rollback mark.
    ///
    /// Runs before execution edges fire, so successors of the failed attempt
    /// are never enqueued with stale data.
    fn maybe_retry(
        &mut self,
        node_id: NodeId,
        node: &Node,
        outputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<bool> {
        // `LspCheck` is a deterministic validation node: a failing check
        // retries the segment exactly like a failing `Validator`.
        if !matches!(node.kind.as_str(), "Validator" | "LspCheck") {
            return Ok(false);
        }
        let failed = circuit_failed(node, outputs);
        let mut mark = *self.state.validation_marks.entry(node_id).or_default();
        // The completion order is not monotonic: ForEach iterations and fresh
        // function invocations un-mark and re-run nodes, which can leave a
        // stored `order_mark` past the end of the current order. A stale mark
        // would roll back a bounded log prefix yet re-queue an empty segment,
        // silently accepting the failure. Fall back to the whole frame.
        if mark.order_mark > self.active_scheduler().order_len() {
            mark = RetryMark {
                log_mark: 0,
                order_mark: 0,
            };
            self.state.validation_marks.insert(node_id, mark);
        }
        if !failed {
            let log_mark = ctx.transaction_log.mark();
            let order_mark = self.active_scheduler().order_len();
            self.state.validation_marks.insert(
                node_id,
                RetryMark {
                    log_mark,
                    order_mark,
                },
            );
            self.state.attempt_counts.remove(&node_id);
            return Ok(false);
        }

        let retry = node.data.get("retry");
        let max_attempts = retry
            .and_then(|r| r.get("max_attempts"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or_else(|| {
                ctx.config
                    .as_ref()
                    .and_then(|c| c.try_read().ok())
                    .map(|cfg| cfg.execution.validation_max_attempts)
            })
            .unwrap_or(1);
        let attempts = self.state.attempt_counts.entry(node_id).or_insert(0);
        if max_attempts <= 1 || *attempts >= max_attempts - 1 {
            return Ok(false);
        }
        *attempts += 1;
        let attempt = *attempts + 1;
        self.circuit_failures = 0;

        let rollback_enabled =
            retry.and_then(|r| r.get("rollback")).and_then(|v| v.as_bool()).unwrap_or(true);
        if rollback_enabled {
            let undone = ctx.transaction_log.rollback_after(mark.log_mark)?;
            ctx.audit(
                "execution.validation_rollback",
                serde_json::json!({
                    "run_id": ctx.run_id.to_string(),
                    "node_id": node_id.to_string(),
                    "undone": undone,
                }),
            );
            self.emit(ExecutionEvent::Message {
                node_id,
                message: format!(
                    "validation failed: rolled back {undone} file mutation(s), retrying attempt {attempt}/{max_attempts}"
                ),
            });
        } else {
            self.emit(ExecutionEvent::Message {
                node_id,
                message: format!("validation failed: retrying attempt {attempt}/{max_attempts}"),
            });
        }

        // Re-queue the executed segment (completion order) plus this
        // validator so the whole attempt re-runs on the restored files.
        let segment = self.active_scheduler().order_from(mark.order_mark);
        // A validator that passed inside the rolled-back segment loses that
        // pass; clamp its mark to this segment so a later failure of it
        // re-runs the full (superset) segment instead of a stale suffix.
        for (vid, m) in self.state.validation_marks.iter_mut() {
            if *vid != node_id && m.order_mark > mark.order_mark {
                *m = mark;
            }
        }
        {
            let sched = self.active_scheduler_mut();
            // Drain stale queued duplicates first: a node fed by both an exec
            // edge and a data edge sits in the queue twice, and un-marking
            // would otherwise resurrect the stale copy.
            sched.dequeue_all(&segment);
            for id in &segment {
                sched.unmark_executed(*id);
            }
            for id in &segment {
                sched.enqueue(*id);
            }
        }
        self.write_checkpoint(ctx);
        Ok(true)
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
        sched.dequeue_all(&[node_id]);
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
            Some(frame) if frame.function.is_some() => {
                &mut frame.function.as_mut().unwrap().scheduler
            }
            _ => &mut self.scheduler,
        }
    }

    /// Enters a ForEach loop over the `List` input.
    ///
    /// An empty list fires `Completed` immediately; otherwise the first item
    /// is exposed and the `Body` branch fires. Nested loops in the same frame
    /// are rejected (wrap the inner loop in a function).
    fn enter_foreach(
        &mut self,
        node_id: NodeId,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        blueprint: &Blueprint,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        let depth = self.state.call_stack.len();
        if self.state.foreach_stack.last().is_some_and(|top| top.depth == depth) {
            return Err(DaemonError::Execution(
                "nested ForEach loops in one frame are not supported; wrap the inner loop in a function".to_string(),
            ));
        }
        let items = match crate::execution::nodes::value_input(node, inputs, "List")?.clone() {
            Value::List(items) => items,
            other => {
                return Err(DaemonError::Execution(format!(
                    "ForEach List input must be a list, got {other:?}"
                )));
            }
        };
        // The run loop already emitted `started` and built the tree node.
        self.active_scheduler_mut().mark_executed(node_id);
        if items.is_empty() {
            self.emit(ExecutionEvent::NodeFinished {
                node_id,
            });
            ctx.audit(
                "node.finished",
                serde_json::json!({ "node_id": node_id.to_string(), "kind": node.kind }),
            );
            self.tree_end(ctx, super::tree::TreeNodeStatus::Done);
            self.write_checkpoint(ctx);
            self.fire_named_edge(blueprint, node_id, "Completed")?;
            return Ok(());
        }
        self.state.foreach_stack.push(ForEachState {
            node_id,
            items,
            index: 0,
            depth,
            count: 1,
        });
        self.publish_foreach_item(node, ctx)?;
        self.emit(ExecutionEvent::NodeFinished {
            node_id,
        });
        ctx.audit(
            "node.finished",
            serde_json::json!({ "node_id": node_id.to_string(), "kind": node.kind }),
        );
        self.tree_end(ctx, super::tree::TreeNodeStatus::Done);
        self.write_checkpoint(ctx);
        self.fire_named_edge(blueprint, node_id, "Body")
    }

    /// Advances the innermost loop of the current frame after its body drains.
    ///
    /// Returns `true` when the loop continues (next item exposed, `Body`
    /// fired) or completes (`Completed` fired); `false` when no loop of this
    /// frame is active.
    fn advance_foreach(
        &mut self,
        blueprint: &Blueprint,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<bool> {
        let depth = self.state.call_stack.len();
        let Some(top) = self.state.foreach_stack.last() else {
            return Ok(false);
        };
        if top.depth != depth {
            return Ok(false);
        }
        let node_id = top.node_id;
        if top.index + 1 >= top.items.len() {
            self.state.foreach_stack.pop();
            self.write_checkpoint(ctx);
            self.fire_named_edge(blueprint, node_id, "Completed")?;
            return Ok(true);
        }
        let max = ctx
            .config
            .as_ref()
            .and_then(|c| c.try_read().ok())
            .map(|cfg| cfg.execution.foreach_max_iterations)
            .unwrap_or(0);
        let top = self.state.foreach_stack.last_mut().expect("loop on top");
        top.index += 1;
        top.count += 1;
        if max != 0 && top.count > max {
            return Err(DaemonError::Execution(format!("ForEach loop exceeded {max} iterations")));
        }
        let node_id = top.node_id;
        self.unmark_exec_subgraph(blueprint, node_id, "Body")?;
        let node = blueprint
            .node(node_id)
            .cloned()
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;
        self.publish_foreach_item(&node, ctx)?;
        self.write_checkpoint(ctx);
        self.fire_named_edge(blueprint, node_id, "Body")?;
        Ok(true)
    }

    /// Un-marks the execution subgraph reachable from one named output pin.
    ///
    /// ForEach body iterations re-run their nodes every pass, so advancing
    /// the loop clears the completed marks (and stale queue entries) of the
    /// `Body` subgraph first.
    fn unmark_exec_subgraph(
        &mut self,
        blueprint: &Blueprint,
        node_id: NodeId,
        pin_name: &str,
    ) -> DaemonResult<()> {
        let start_pin = blueprint
            .node(node_id)
            .and_then(|n| n.pins.iter().find(|p| p.name == pin_name))
            .ok_or_else(|| DaemonError::Execution(format!("node missing pin {pin_name}")))?
            .id;
        let mut reached = vec![node_id];
        let mut stack = vec![node_id];
        while let Some(current) = stack.pop() {
            for edge in blueprint.outgoing_edges(current) {
                let source_pin = blueprint
                    .pin(edge.source_pin)
                    .ok_or_else(|| DaemonError::Execution("unknown source pin".to_string()))?;
                if source_pin.pin_type != PinType::ExecOutput {
                    continue;
                }
                if current == node_id && edge.source_pin != start_pin {
                    continue;
                }
                if !reached.contains(&edge.target_node) {
                    reached.push(edge.target_node);
                    stack.push(edge.target_node);
                }
            }
        }
        let sched = self.active_scheduler_mut();
        sched.dequeue_all(&reached);
        // The loop node itself stays completed; only its body re-runs.
        for id in reached.iter().filter(|id| **id != node_id) {
            sched.unmark_executed(*id);
        }
        Ok(())
    }

    /// Writes the current loop item and index into the ForEach node's outputs.
    fn publish_foreach_item(
        &mut self,
        node: &Node,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<()> {
        let top = self
            .state
            .foreach_stack
            .last()
            .ok_or_else(|| DaemonError::Execution("no active ForEach loop".to_string()))?;
        let item = top.items.get(top.index).cloned().unwrap_or(Value::Null);
        let index = top.index;
        let function = self.active_function();
        let mut outputs = Vec::new();
        for pin in &node.pins {
            if pin.pin_type != PinType::DataOutput {
                continue;
            }
            let value = if pin.name == "Iteration" {
                item.clone()
            } else if pin.name == "Index" {
                Value::Int(index as i64)
            } else {
                continue;
            };
            self.state.data_values.insert(pin.id, value.clone());
            outputs.push((pin.id, value));
        }
        self.emit(ExecutionEvent::NodeData {
            node_id: node.id,
            outputs,
            function,
        });
        ctx.audit(
            "foreach.item",
            serde_json::json!({ "node_id": node.id.to_string(), "index": index }),
        );
        Ok(())
    }

    /// Enqueues the targets of one named execution output pin.
    fn fire_named_edge(
        &mut self,
        blueprint: &Blueprint,
        node_id: NodeId,
        pin_name: &str,
    ) -> DaemonResult<()> {
        let mut targets = Vec::new();
        for edge in blueprint.outgoing_edges(node_id) {
            let source_pin = blueprint
                .pin(edge.source_pin)
                .ok_or_else(|| DaemonError::Execution("unknown source pin".to_string()))?;
            if source_pin.pin_type == PinType::ExecOutput && source_pin.name == pin_name {
                targets.push(edge.target_node);
            }
        }
        let sched = self.active_scheduler_mut();
        for target in targets {
            if !sched.is_executed(target) {
                sched.enqueue(target);
            }
        }
        Ok(())
    }

    /// Records the start of a node execution in the execution tree.
    fn tree_begin(&mut self, node: &Node) {
        let parent = self.frame_trees.last().map(String::as_str).or(self.tree_root.as_deref());
        let id = self.tree.spawn(
            parent,
            super::tree::TreeNodeKind::BlueprintNode(node.kind.clone()),
            node.kind.clone(),
            now_millis(),
        );
        self.current_tree = Some(id);
    }

    /// Drains executor-queued tree ops, then marks the node finished.
    fn tree_end(&mut self, ctx: &mut ExecutionContext, status: super::tree::TreeNodeStatus) {
        let ops = std::mem::take(&mut ctx.tree_ops);
        for op in ops {
            match op {
                super::tree::TreeOp::SpawnChild {
                    kind,
                    label,
                } => {
                    let parent = self.current_tree.as_deref();
                    let id = self.tree.spawn(parent, kind, label, now_millis());
                    self.current_tree = Some(id);
                }
                super::tree::TreeOp::FinishCurrent {
                    status,
                } => {
                    if let Some(id) = self.current_tree.clone() {
                        self.tree.finish(&id, status, now_millis());
                        self.current_tree = self.tree.nodes.get(&id).and_then(|n| n.parent.clone());
                    }
                }
                super::tree::TreeOp::AddTokens(n) => {
                    if let Some(id) = &self.current_tree {
                        self.tree.add_tokens(id, n);
                    }
                }
            }
        }
        if let Some(id) = self.current_tree.clone() {
            self.tree.finish(&id, status, now_millis());
            self.current_tree = self.tree.nodes.get(&id).and_then(|n| n.parent.clone());
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
        let name = caller_node.data.get("function").and_then(|v| v.as_str()).ok_or_else(|| {
            DaemonError::Execution("CallFunction requires data.function".to_string())
        })?;
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
            .ok_or_else(|| {
                DaemonError::Execution(format!("function '{name}' has no entry node"))
            })?;
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
                DaemonError::Execution(format!(
                    "CallFunction '{name}' input '{}' is not wired",
                    fp.name
                ))
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
        // A fresh variable frame: locals stay inside the function body.
        ctx.variables.push(HashMap::new());
        // A tree node for the function body, parented under the caller.
        let func_tree = self.tree.spawn(
            self.current_tree.as_deref(),
            super::tree::TreeNodeKind::Function(name.to_string()),
            name.to_string(),
            now_millis(),
        );
        self.frame_trees.push(func_tree);
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
            Some(fn_id) => {
                self.registry
                    .function_by_id(fn_id)
                    .ok_or_else(|| {
                        DaemonError::Execution(format!("function {fn_id} not in registry"))
                    })?
                    .body
            }
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
        // Close the CallFunction tree node the caller opened at start.
        self.tree_end(ctx, super::tree::TreeNodeStatus::Done);
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
    ///
    /// Inputs without an incoming edge fall back to the pin's default value,
    /// or to null when the pin is optional; genuinely missing inputs stay
    /// absent so the executor reports them.
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
            // Coerce toward the pin's declared type so a downstream executor
            // receives the type it declared. Validation already rejects
            // incompatible wirings, so a failure here means the graph changed
            // under a running plan (a replan) and is worth surfacing.
            let value = match blueprint.pin(edge.target_pin) {
                Some(target_pin) => match metteur_shared::coerce(&value, &target_pin.data_type) {
                    Some(coerced) => coerced,
                    None => {
                        return Err(DaemonError::Execution(format!(
                            "node {node_id} input '{}' expects {} but the incoming value is {value:?}",
                            target_pin.name, target_pin.data_type
                        )));
                    }
                },
                None => value,
            };
            inputs.insert(edge.target_pin, value);
        }
        if let Some(node) = blueprint.node(node_id) {
            for pin in node.pins.iter().filter(|p| p.pin_type == PinType::DataInput) {
                if inputs.contains_key(&pin.id) {
                    continue;
                }
                if let Some(default) = &pin.default {
                    inputs.insert(pin.id, crate::execution::nodes::json_to_value(default));
                } else if pin.optional {
                    inputs.insert(pin.id, Value::Null);
                }
            }
        }
        Ok(inputs)
    }

    /// Returns the target nodes of a node's execution output edges.
    ///
    /// `Branch` follows the `True`/`False` edge matching its `Result`;
    /// `Switch` follows the `Case_<value>` edge matching its `Result`,
    /// falling back to `Default`; `RequestApproval` follows `Approved` or
    /// `Denied` from its `Allowed` output. Any other node fans out to all of
    /// its execution output edges.
    fn follow_exec_edges(
        &self,
        blueprint: &Blueprint,
        node_id: NodeId,
    ) -> DaemonResult<Vec<NodeId>> {
        let node = blueprint
            .node(node_id)
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;

        let take = match node.kind.as_str() {
            "Branch" => {
                let result = self.branch_result(blueprint, node_id)?;
                Some(if result {
                    "True".to_string()
                } else {
                    "False".to_string()
                })
            }
            "Switch" => Some(self.switch_take(blueprint, node_id)?),
            "RequestApproval" => {
                let allowed = self.approval_result(blueprint, node_id)?;
                Some(if allowed {
                    "Approved".to_string()
                } else {
                    "Denied".to_string()
                })
            }
            _ => None,
        };

        let mut next = Vec::new();
        for edge in blueprint.outgoing_edges(node_id) {
            let source_pin = blueprint
                .pin(edge.source_pin)
                .ok_or_else(|| DaemonError::Execution("unknown source pin".to_string()))?;
            if source_pin.pin_type != PinType::ExecOutput {
                continue;
            }
            if let Some(take) = &take
                && source_pin.name != *take
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

    /// Returns the execution output pin taken by a `Switch` node.
    ///
    /// The `Result` value selects the `Case_<value>` pin; when no such pin is
    /// wired, `Default` is taken so unmatched cases are an empty branch rather
    /// than an error.
    fn switch_take(&self, blueprint: &Blueprint, node_id: NodeId) -> DaemonResult<String> {
        let node = blueprint
            .node(node_id)
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;
        let pin =
            node.pins.iter().find(|p| p.name == "Result").ok_or_else(|| {
                DaemonError::Execution("switch node missing Result pin".to_string())
            })?;
        let value = self.state.data_values.get(&pin.id).ok_or_else(|| {
            DaemonError::Execution("switch node missing Result value".to_string())
        })?;
        let candidate = format!("Case_{}", crate::execution::nodes::value_to_string(value));
        let wired: Vec<&str> = blueprint
            .outgoing_edges(node_id)
            .iter()
            .filter_map(|edge| blueprint.pin(edge.source_pin))
            .filter(|pin| pin.pin_type == PinType::ExecOutput)
            .map(|pin| pin.name.as_str())
            .collect();
        if wired.contains(&candidate.as_str()) {
            Ok(candidate)
        } else {
            Ok("Default".to_string())
        }
    }

    /// Reads the boolean outcome of a `RequestApproval` node.
    fn approval_result(&self, blueprint: &Blueprint, node_id: NodeId) -> DaemonResult<bool> {
        let node = blueprint
            .node(node_id)
            .ok_or_else(|| DaemonError::Execution(format!("unknown node {node_id}")))?;
        let pin = node.pins.iter().find(|p| p.name == "Allowed").ok_or_else(|| {
            DaemonError::Execution("approval node missing Allowed pin".to_string())
        })?;
        self.state.data_values.get(&pin.id).and_then(|v| v.as_bool()).ok_or_else(|| {
            DaemonError::Execution("approval node missing Allowed value".to_string())
        })
    }
}

/// Returns whether a validation node's output reports failure.
///
/// `LspCheck` participates here so that a failing deterministic language-server
/// check gets the same rollback/retry and circuit-breaker treatment as a
/// `Validator`: the edit that broke the code is rolled back and retried.
fn circuit_failed(node: &Node, outputs: &HashMap<PinId, Value>) -> bool {
    if !matches!(node.kind.as_str(), "Validator" | "Judge" | "LspCheck") {
        return false;
    }
    let result_pin = match node.kind.as_str() {
        "Validator" | "LspCheck" => "Passed",
        _ => "Success",
    };
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
    use std::path::PathBuf;
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
                        ..Default::default()
                    },
                    Pin {
                        id: start_a,
                        name: "A".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: start_b,
                        name: "B".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                        ..Default::default()
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
                        ..Default::default()
                    },
                    Pin {
                        id: add_exec_out,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecOutput,
                        data_type: DataType::Void,
                        ..Default::default()
                    },
                    Pin {
                        id: add_a,
                        name: "A".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: add_b,
                        name: "B".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: add_result,
                        name: "Result".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                        ..Default::default()
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
                        ..Default::default()
                    },
                    Pin {
                        id: judge_score,
                        name: "Score".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: judge_success,
                        name: "Success".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Bool,
                        ..Default::default()
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
        let (id, entry_node, add, exit_node) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (
            entry_exec,
            entry_a,
            entry_b,
            add_exin,
            add_exout,
            add_a,
            add_b,
            add_res,
            exit_exin,
            exit_res,
        ) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        FunctionEntry {
            id,
            name: "AddFunc".to_string(),
            description: "Adds two numbers".to_string(),
            signature: FunctionSignature {
                inputs: vec![
                    FnPin {
                        name: "A".to_string(),
                        data_type: DataType::Float,
                        description: None,
                        ..Default::default()
                    },
                    FnPin {
                        name: "B".to_string(),
                        data_type: DataType::Float,
                        description: None,
                        ..Default::default()
                    },
                ],
                outputs: vec![FnPin {
                    name: "Result".to_string(),
                    data_type: DataType::Float,
                    description: None,
                    ..Default::default()
                }],
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
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: entry_node,
                        source_pin: entry_exec,
                        target_node: add,
                        target_pin: add_exin,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: add,
                        source_pin: add_exout,
                        target_node: exit_node,
                        target_pin: exit_exin,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: entry_node,
                        source_pin: entry_a,
                        target_node: add,
                        target_pin: add_a,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: entry_node,
                        source_pin: entry_b,
                        target_node: add,
                        target_pin: add_b,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: add,
                        source_pin: add_res,
                        target_node: exit_node,
                        target_pin: exit_res,
                    },
                ],
                entry_node_id: entry_node,
            },
            source: metteur_shared::model::function::FunctionSource::Builtin,
        }
    }

    /// Builds Start -> CallFunction(AddFunc) with A=5, B=3 directly wired.
    fn call_function_blueprint(func_name: &str) -> Blueprint {
        let (start, caller) = (Uuid::new_v4(), Uuid::new_v4());
        let (start_ex, start_a, start_b, caller_exin, caller_exout, caller_a, caller_b, caller_res) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
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
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_ex,
                    target_node: caller,
                    target_pin: caller_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_a,
                    target_node: caller,
                    target_pin: caller_a,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_b,
                    target_node: caller,
                    target_pin: caller_b,
                },
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
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        let events = interpreter.run(&shared(blueprint.clone()), None).await.unwrap();

        // Caller result is 5 + 3 = 8 and is reported through a NodeData event.
        let data = events
            .iter()
            .find_map(|e| match e {
                ExecutionEvent::NodeData {
                    node_id,
                    outputs,
                    ..
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
        let (id, entry_node, caller, exit_node) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (entry_exec, caller_exin, caller_exout, exit_exin) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type: DataType::Void,
            ..Default::default()
        };
        let func = FunctionEntry {
            id,
            name: "Selfish".to_string(),
            description: String::new(),
            signature: metteur_shared::model::function::FunctionSignature::default(),
            body: Blueprint {
                id,
                name: "Selfish".to_string(),
                nodes: vec![
                    Node {
                        id: entry_node,
                        node_type: NodeType::Event,
                        kind: "FunctionEntry".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![pin(entry_exec, "Exec", PinType::ExecOutput)],
                        data: serde_json::Value::Null,
                    },
                    Node {
                        id: caller,
                        node_type: NodeType::Function,
                        kind: "CallFunction".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![
                            pin(caller_exin, "Exec", PinType::ExecInput),
                            pin(caller_exout, "Exec", PinType::ExecOutput),
                        ],
                        data: serde_json::json!({ "function": "Selfish" }),
                    },
                    Node {
                        id: exit_node,
                        node_type: NodeType::Event,
                        kind: "FunctionExit".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![pin(exit_exin, "Exec", PinType::ExecInput)],
                        data: serde_json::Value::Null,
                    },
                ],
                edges: vec![
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: entry_node,
                        source_pin: entry_exec,
                        target_node: caller,
                        target_pin: caller_exin,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: caller,
                        source_pin: caller_exout,
                        target_node: exit_node,
                        target_pin: exit_exin,
                    },
                ],
                entry_node_id: entry_node,
            },
            source: metteur_shared::model::function::FunctionSource::Builtin,
        };
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func.clone());
        let blueprint = call_function_blueprint("Selfish");
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
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
        let start_a = blueprint
            .nodes
            .iter()
            .find(|n| n.id == start)
            .unwrap()
            .pins
            .iter()
            .find(|p| p.name == "A")
            .unwrap()
            .id;
        let start_b = blueprint
            .nodes
            .iter()
            .find(|n| n.id == start)
            .unwrap()
            .pins
            .iter()
            .find(|p| p.name == "B")
            .unwrap()
            .id;
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
                Frame {
                    node_id: start,
                    pc: 0,
                    function: None,
                },
                Frame {
                    node_id: caller,
                    pc: 0,
                    function: Some(FunctionBody {
                        id: func.id,
                        scheduler: frame_sched,
                    }),
                },
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
            executed_order: vec![start, caller],
            attempt_counts: HashMap::new(),
            validation_marks: HashMap::new(),
            variables: vec![HashMap::new(), HashMap::new()],
            todos: Vec::new(),
            exec_tree: crate::execution::tree::ExecTree::new(),
            frame_trees: Vec::new(),
            foreach_stack: Vec::new(),
            current_tree: None,
            circuit_failures: 0,
            error: None,
        };
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
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
        use crate::sandbox::approval::{ApprovalBroker, Decision};
        use metteur_shared::config::{Config, ExecutionConfig};

        let broker = Arc::new(ApprovalBroker::new());
        let config = Arc::new(RwLock::new(Config {
            execution: ExecutionConfig {
                circuit_break_after: 1,
                validation_max_attempts: 1,
                foreach_max_iterations: 1000,
                ..Default::default()
            },
            ..Default::default()
        }));

        // Start(A=0, Expected=5) -> Validator(mode eq): A != Expected always.
        let (start, validator) = (Uuid::new_v4(), Uuid::new_v4());
        let (start_ex, start_a, start_exp, v_exin, v_actual, v_expected, v_passed) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
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
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_ex,
                    target_node: validator,
                    target_pin: v_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_a,
                    target_node: validator,
                    target_pin: v_actual,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: start_exp,
                    target_node: validator,
                    target_pin: v_expected,
                },
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
                            ..Default::default()
                        },
                        Pin {
                            id: a_exec_out,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecOutput,
                            data_type: DataType::Void,
                            ..Default::default()
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
                            ..Default::default()
                        },
                        Pin {
                            id: b_exec_out,
                            name: "Exec".to_string(),
                            pin_type: PinType::ExecOutput,
                            data_type: DataType::Void,
                            ..Default::default()
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
                        ..Default::default()
                    },
                    Pin {
                        id: exec_out,
                        name: "Exec".to_string(),
                        pin_type: PinType::ExecOutput,
                        data_type: DataType::Void,
                        ..Default::default()
                    },
                    Pin {
                        id: a,
                        name: "A".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: b,
                        name: "B".to_string(),
                        pin_type: PinType::DataInput,
                        data_type: DataType::Float,
                        ..Default::default()
                    },
                    Pin {
                        id: result,
                        name: "Result".to_string(),
                        pin_type: PinType::DataOutput,
                        data_type: DataType::Float,
                        ..Default::default()
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
                            ..Default::default()
                        },
                        Pin {
                            id: start_a,
                            name: "A".to_string(),
                            pin_type: PinType::DataOutput,
                            data_type: DataType::Float,
                            ..Default::default()
                        },
                        Pin {
                            id: start_b,
                            name: "B".to_string(),
                            pin_type: PinType::DataOutput,
                            data_type: DataType::Float,
                            ..Default::default()
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

    /// A sink that records every checkpoint a run produces, standing in for a
    /// crash that can be resumed from any of them.
    struct RecordSink {
        run_id: uuid::Uuid,
        checkpoints: std::sync::Mutex<Vec<ExecutionCheckpoint>>,
    }

    impl RecordSink {
        fn new() -> Self {
            Self {
                run_id: Uuid::new_v4(),
                checkpoints: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl CheckpointSink for RecordSink {
        fn run_id(&self) -> uuid::Uuid {
            self.run_id
        }

        fn write(&self, checkpoint: &ExecutionCheckpoint) -> DaemonResult<()> {
            self.checkpoints.lock().unwrap().push(checkpoint.clone());
            Ok(())
        }
    }

    /// A checkpoint taken after a node completes must already list that node's
    /// successors as pending.
    ///
    /// Writing the checkpoint before the execution edges fire recorded the
    /// just-finished node as executed while its successors were in neither
    /// `executed` nor `pending`. Resuming such a checkpoint drained an empty
    /// queue, reported the run as completed, and silently skipped the rest of
    /// the graph.
    #[tokio::test]
    async fn checkpoint_records_successors_of_finished_node() {
        let blueprint = build_blueprint();
        let sink = Arc::new(RecordSink::new());
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        interpreter.run(&shared(blueprint.clone()), None).await.unwrap();

        let checkpoints = sink.checkpoints.lock().unwrap();
        // Checkpoint 0 is the pre-run seed; checkpoint 1 follows Start.
        let after_start = checkpoints
            .get(1)
            .expect("a checkpoint after the first node must exist");
        assert!(
            !after_start.pending.is_empty(),
            "a checkpoint taken after a node finished must carry its successors, \
             otherwise a resume silently drops them (executed={:?}, pending=[])",
            after_start.executed,
        );
    }

    /// Resuming from a checkpoint the run itself produced must still execute
    /// every node that was left outstanding.
    #[tokio::test]
    async fn resume_from_live_checkpoint_finishes_outstanding_nodes() {
        let blueprint = build_blueprint();
        let sink = Arc::new(RecordSink::new());
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        interpreter.run(&shared(blueprint.clone()), None).await.unwrap();
        let captured = {
            let checkpoints = sink.checkpoints.lock().unwrap();
            let cp = checkpoints
                .get(1)
                .expect("a checkpoint after the first node must exist")
                .clone();
            assert!(
                !cp.pending.is_empty(),
                "resuming this checkpoint would skip the rest of the graph"
            );
            cp
        };
        let expected_pending: Vec<uuid::Uuid> = captured.pending.clone();

        let mut resumed = new_interpreter();
        let events = resumed
            .resume_with_control(
                &shared(blueprint),
                captured,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        // Every node the checkpoint left outstanding must actually run.
        for node_id in expected_pending {
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    ExecutionEvent::NodeStarted { node_id: n } if *n == node_id
                )),
                "node {node_id} was pending at checkpoint time but never ran after resume"
            );
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
            executed_order: vec![start],
            attempt_counts: HashMap::new(),
            validation_marks: HashMap::new(),
            variables: vec![HashMap::new()],
            todos: Vec::new(),
            exec_tree: crate::execution::tree::ExecTree::new(),
            frame_trees: Vec::new(),
            foreach_stack: Vec::new(),
            current_tree: None,
            circuit_failures: 0,
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
    async fn cancel_marks_run_cancelled() {
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
        // A user cancel is not a failure: it gets its own terminal status so a
        // client can tell an abandoned run from a broken one.
        assert_eq!(checkpoints.last().unwrap().status, RunStatus::Cancelled);
    }

    /// A paused run must stay cancellable. The pause loop parks between nodes,
    /// so without a cancel re-check on every tick a cancel issued while paused
    /// would never be observed and the run would hold its workspace slot
    /// forever.
    #[tokio::test]
    async fn cancel_while_paused_terminates() {
        let blueprint = Arc::new(build_blueprint());
        let pause = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The task owns the interpreter and the shared blueprint so the future
        // is 'static.
        let handle = {
            let blueprint = blueprint.clone();
            let pause = pause.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut interpreter = new_interpreter();
                let result = interpreter
                    .run_with_control(&shared((*blueprint).clone()), None, pause, cancel)
                    .await;
                (result, interpreter)
            })
        };
        // Let the loop reach the pause gate, then cancel.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("a paused run must observe a cancel request")
            .expect("run task must not panic");
        assert!(result.is_err());
    }

    /// A test tool that writes bad content on its first call and good content
    /// afterwards, exercising validation retry deterministically.
    /// With `fail_always` it never recovers, exercising retry exhaustion.
    struct FlakyWrite {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_always: bool,
    }

    #[async_trait::async_trait]
    impl crate::registry::Tool for FlakyWrite {
        fn name(&self) -> &str {
            "FlakyWrite"
        }
        fn description(&self) -> &str {
            "writes bad content once, then good content"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            })
        }
        async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
            let path = args
                .iter()
                .find_map(|v| match v {
                    Value::Json(obj) => {
                        obj.get("path").and_then(|p| p.as_str()).map(str::to_string)
                    }
                    _ => None,
                })
                .ok_or_else(|| DaemonError::Execution("FlakyWrite requires a path".to_string()))?;
            let attempt = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let content = if self.fail_always || attempt == 0 {
                "bad"
            } else {
                "good"
            };
            let resolved = ctx.workspace_root.join(&path);
            let old_content = std::fs::read(&resolved).ok();
            ctx.transaction_log.record_file_write(
                resolved.clone(),
                old_content,
                content.as_bytes().to_vec(),
            );
            std::fs::write(&resolved, content).map_err(DaemonError::Io)?;
            Ok(Value::Bool(true))
        }
    }

    /// Builds a blueprint: Start -> FlakyWrite -> ReadFile -> Validator -> End.
    fn build_retry_blueprint(file: &str, retry_data: serde_json::Value) -> (Blueprint, Uuid) {
        let start = Uuid::new_v4();
        let write = Uuid::new_v4();
        let read = Uuid::new_v4();
        let validator = Uuid::new_v4();
        let end = Uuid::new_v4();
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let tool_node = |id: Uuid,
                         tool_name: &str,
                         exec_in: Uuid,
                         exec_out: Uuid,
                         path_pin: Uuid,
                         result_pin: Uuid| Node {
            id,
            node_type: NodeType::Function,
            kind: "Tool".to_string(),
            position: (0.0, 0.0),
            pins: vec![
                pin(exec_in, "Exec", PinType::ExecInput, DataType::Void),
                pin(exec_out, "Exec", PinType::ExecOutput, DataType::Void),
                Pin {
                    default: Some(serde_json::json!(file)),
                    ..pin(path_pin, "path", PinType::DataInput, DataType::String)
                },
                pin(result_pin, "Result", PinType::DataOutput, DataType::String),
            ],
            data: serde_json::json!({ "tool_name": tool_name }),
        };
        let (s_ex, w_exin, w_exout, w_path, w_res) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (r_exin, r_exout, r_path, r_res, r_rawnum) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (v_exin, v_exout, v_actual, v_expected, v_passed) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let e_exin = Uuid::new_v4();
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "retry".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                tool_node(write, "FlakyWrite", w_exin, w_exout, w_path, w_res),
                // Raw content: this blueprint validates the file text itself,
                // so the read must not carry line-number prefixes.
                {
                    let mut node = tool_node(read, "ReadFile", r_exin, r_exout, r_path, r_res);
                    node.pins.push(Pin {
                        default: Some(serde_json::json!(false)),
                        ..Pin::data(
                            "line_numbers",
                            PinType::DataInput,
                            DataType::Bool,
                            r_rawnum,
                        )
                    });
                    node
                },
                Node {
                    id: validator,
                    node_type: NodeType::Function,
                    kind: "Validator".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(v_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(v_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(v_actual, "Actual", PinType::DataInput, DataType::String),
                        Pin {
                            default: Some(serde_json::json!("good")),
                            ..pin(v_expected, "Expected", PinType::DataInput, DataType::String)
                        },
                        pin(v_passed, "Passed", PinType::DataOutput, DataType::Bool),
                    ],
                    data: retry_data,
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: write,
                    target_pin: w_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write,
                    source_pin: w_exout,
                    target_node: read,
                    target_pin: r_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: read,
                    source_pin: r_exout,
                    target_node: validator,
                    target_pin: v_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: validator,
                    source_pin: v_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: read,
                    source_pin: r_res,
                    target_node: validator,
                    target_pin: v_actual,
                },
            ],
            entry_node_id: start,
        };
        (blueprint, validator)
    }

    fn messages_of(events: &[ExecutionEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                ExecutionEvent::Message {
                    message,
                    ..
                } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn validator_retry_rolls_back_and_reruns_segment() {
        let workspace = std::env::temp_dir().join(format!("metteur-retry-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (blueprint, validator) = build_retry_blueprint(
            "target.txt",
            serde_json::json!({ "mode": "eq", "retry": { "max_attempts": 3 } }),
        );

        let registry = Arc::new(Registry::with_builtins());
        registry
            .try_register_tool(Arc::new(FlakyWrite {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                fail_always: false,
            }))
            .unwrap();
        let sink = Arc::new(MemorySink::new());
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), workspace.clone())
                .with_checkpoint_sink(sink.clone());
        let events = interpreter.run(&shared(blueprint), None).await.unwrap();

        // The bad write was rolled back and the segment re-ran with good content.
        assert_eq!(std::fs::read_to_string(workspace.join("target.txt")).unwrap(), "good");
        let messages = messages_of(&events);
        assert!(messages.iter().any(|m| m.contains("rolled back 1 file mutation")));
        assert!(messages.iter().any(|m| m.contains("retrying attempt 2/3")));
        // Passing clears the validator's attempt state.
        assert!(interpreter.state.attempt_counts.is_empty());
        assert!(interpreter.state.validation_marks.contains_key(&validator));
        // Full segment re-ran: write + read + validator each completed twice.
        let started =
            events.iter().filter(|e| matches!(e, ExecutionEvent::NodeStarted { .. })).count();
        assert_eq!(started, 9);
    }

    #[tokio::test]
    async fn validator_retry_exhaustion_trips_circuit_breaker() {
        use crate::sandbox::approval::{ApprovalBroker, Decision};
        use metteur_shared::config::{Config, ExecutionConfig};

        let workspace = std::env::temp_dir().join(format!("metteur-retry-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (blueprint, _) = build_retry_blueprint(
            "target.txt",
            serde_json::json!({ "mode": "eq", "retry": { "max_attempts": 2 } }),
        );

        let broker = Arc::new(ApprovalBroker::new());
        let config = Arc::new(RwLock::new(Config {
            execution: ExecutionConfig {
                circuit_break_after: 1,
                validation_max_attempts: 1,
                foreach_max_iterations: 1000,
                ..Default::default()
            },
            ..Default::default()
        }));
        let registry = Arc::new(Registry::with_builtins());
        registry
            .try_register_tool(Arc::new(FlakyWrite {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                fail_always: true,
            }))
            .unwrap();
        let mut interpreter = Interpreter::new(registry, LlmClientFactory::new(), workspace)
            .with_config(config)
            .with_approvals(broker.clone());
        let blueprint_arc = Arc::new(PLock::new(blueprint));
        let task = tokio::spawn(async move { interpreter.run(&blueprint_arc, None).await });
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
    async fn retry_state_survives_checkpoint_resume() {
        let blueprint = build_blueprint();
        let mut checkpoint = checkpoint_after_start(&blueprint);
        let validator = Uuid::new_v4();
        checkpoint.attempt_counts = HashMap::from([(validator, 1)]);
        checkpoint.validation_marks = HashMap::from([(
            validator,
            RetryMark {
                log_mark: 0,
                order_mark: 1,
            },
        )]);
        checkpoint.executed_order = vec![blueprint.entry_node_id];

        let sink = Arc::new(MemorySink::new());
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        interpreter
            .resume_with_control(
                &shared(blueprint.clone()),
                checkpoint,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();
        // Retry bookkeeping survives the resume untouched.
        assert_eq!(interpreter.state.attempt_counts.get(&validator), Some(&1));
        assert_eq!(
            interpreter.state.validation_marks.get(&validator),
            Some(&RetryMark {
                log_mark: 0,
                order_mark: 1
            })
        );
        // The restored order prefix is intact and the run continued past it.
        let order = interpreter.scheduler.order_list();
        assert_eq!(order.first(), Some(&blueprint.entry_node_id));
        assert!(order.len() >= 3);
    }

    fn pin_with_default(
        p_id: Uuid,
        name: &str,
        pin_type: PinType,
        data_type: DataType,
        default: serde_json::Value,
    ) -> Pin {
        Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            default: Some(default),
            ..Default::default()
        }
    }

    /// Builds Start -> VariableSet(x) -> VariableGet(x) -> Validator(eq 1) -> End.
    fn variable_round_trip_blueprint() -> Blueprint {
        let (start, set, get, validator, end) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (s_ex, set_exin, set_exout, set_name, set_val, set_out) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (get_exin, get_exout, get_name, get_val) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (v_exin, v_exout, v_actual, v_expected, v_passed) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let e_exin = Uuid::new_v4();
        Blueprint {
            id: Uuid::new_v4(),
            name: "vars".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: set,
                    node_type: NodeType::Function,
                    kind: "VariableSet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(set_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(set_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin_with_default(
                            set_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("x"),
                        ),
                        pin_with_default(
                            set_val,
                            "Value",
                            PinType::DataInput,
                            DataType::Any,
                            serde_json::json!(1),
                        ),
                        pin(set_out, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: get,
                    node_type: NodeType::Pure,
                    kind: "VariableGet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(get_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(get_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin_with_default(
                            get_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("x"),
                        ),
                        pin(get_val, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: validator,
                    node_type: NodeType::Function,
                    kind: "Validator".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(v_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(v_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(v_actual, "Actual", PinType::DataInput, DataType::Any),
                        pin_with_default(
                            v_expected,
                            "Expected",
                            PinType::DataInput,
                            DataType::Any,
                            serde_json::json!(1),
                        ),
                        pin(v_passed, "Passed", PinType::DataOutput, DataType::Bool),
                    ],
                    data: serde_json::json!({ "mode": "eq" }),
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: set,
                    target_pin: set_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: set,
                    source_pin: set_exout,
                    target_node: get,
                    target_pin: get_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: get,
                    source_pin: get_exout,
                    target_node: validator,
                    target_pin: v_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: validator,
                    source_pin: v_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: get,
                    source_pin: get_val,
                    target_node: validator,
                    target_pin: v_actual,
                },
            ],
            entry_node_id: start,
        }
    }

    #[tokio::test]
    async fn variables_round_trip_within_frame() {
        let blueprint = variable_round_trip_blueprint();
        let mut interpreter = new_interpreter();
        interpreter.run(&shared(blueprint), None).await.unwrap();
    }

    #[tokio::test]
    async fn variable_get_undefined_fails() {
        let (start, get, end) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (s_ex, g_exin, g_name, g_val, e_exin) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "undef".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: get,
                    node_type: NodeType::Pure,
                    kind: "VariableGet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(g_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin_with_default(
                            g_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("missing"),
                        ),
                        pin(g_val, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![Edge {
                id: Uuid::new_v4(),
                source_node: start,
                source_pin: s_ex,
                target_node: get,
                target_pin: g_exin,
            }],
            entry_node_id: start,
        };
        let mut interpreter = new_interpreter();
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("undefined variable")
        ));
    }

    /// Builds Start(Case) -> Switch -> WriteFile(hit.txt) per branch -> End.
    fn switch_blueprint(case_value: &str) -> (Blueprint, PathBuf) {
        let workspace = std::env::temp_dir().join(format!("metteur-switch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (start, switch, write_a, write_b, write_d, end) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let write_node = |id: Uuid, content: &str| {
            let (exin, exout, path, text) =
                (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            Node {
                id,
                node_type: NodeType::Function,
                kind: "Tool".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    pin(exin, "Exec", PinType::ExecInput, DataType::Void),
                    pin(exout, "Exec", PinType::ExecOutput, DataType::Void),
                    pin_with_default(
                        path,
                        "path",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!("hit.txt"),
                    ),
                    pin_with_default(
                        text,
                        "content",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!(content),
                    ),
                    pin(Uuid::new_v4(), "Result", PinType::DataOutput, DataType::String),
                ],
                data: serde_json::json!({ "tool_name": "WriteFile" }),
            }
        };
        let (s_ex, s_case) = (Uuid::new_v4(), Uuid::new_v4());
        let (sw_exin, sw_case, sw_result, sw_a, sw_b, sw_d) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let e_exin = Uuid::new_v4();
        let write_a_node = write_node(write_a, "a");
        let write_b_node = write_node(write_b, "b");
        let write_d_node = write_node(write_d, "d");
        let a_exin = write_a_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput)
            .unwrap()
            .id;
        let b_exin = write_b_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput)
            .unwrap()
            .id;
        let d_exin = write_d_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput)
            .unwrap()
            .id;
        let a_exout = write_a_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
            .unwrap()
            .id;
        let b_exout = write_b_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
            .unwrap()
            .id;
        let d_exout = write_d_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
            .unwrap()
            .id;
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "switch".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(s_case, "Case", PinType::DataOutput, DataType::String),
                    ],
                    data: serde_json::json!({ "Case": case_value }),
                },
                Node {
                    id: switch,
                    node_type: NodeType::Control,
                    kind: "Switch".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(sw_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(sw_case, "Case", PinType::DataInput, DataType::String),
                        pin(sw_result, "Result", PinType::DataOutput, DataType::Any),
                        pin(sw_a, "Case_a", PinType::ExecOutput, DataType::Void),
                        pin(sw_b, "Case_b", PinType::ExecOutput, DataType::Void),
                        pin(sw_d, "Default", PinType::ExecOutput, DataType::Void),
                    ],
                    data: serde_json::json!({ "cases": ["a", "b"] }),
                },
                write_a_node,
                write_b_node,
                write_d_node,
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: switch,
                    target_pin: sw_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_case,
                    target_node: switch,
                    target_pin: sw_case,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: switch,
                    source_pin: sw_a,
                    target_node: write_a,
                    target_pin: a_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: switch,
                    source_pin: sw_b,
                    target_node: write_b,
                    target_pin: b_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: switch,
                    source_pin: sw_d,
                    target_node: write_d,
                    target_pin: d_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_a,
                    source_pin: a_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_b,
                    source_pin: b_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_d,
                    source_pin: d_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
            ],
            entry_node_id: start,
        };
        (blueprint, workspace)
    }

    #[tokio::test]
    async fn switch_routes_to_matching_case() {
        let (blueprint, workspace) = switch_blueprint("b");
        let mut interpreter = Interpreter::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            workspace.clone(),
        );
        interpreter.run(&shared(blueprint), None).await.unwrap();
        assert_eq!(std::fs::read_to_string(workspace.join("hit.txt")).unwrap(), "b");
    }

    #[tokio::test]
    async fn switch_falls_back_to_default() {
        let (blueprint, workspace) = switch_blueprint("zzz");
        let mut interpreter = Interpreter::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            workspace.clone(),
        );
        interpreter.run(&shared(blueprint), None).await.unwrap();
        assert_eq!(std::fs::read_to_string(workspace.join("hit.txt")).unwrap(), "d");
    }

    /// Builds Start -> Branch(Condition) -> WriteFile per outcome -> End.
    fn branch_blueprint(condition: bool) -> (Blueprint, PathBuf) {
        let workspace = std::env::temp_dir().join(format!("metteur-branch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (start, branch, write_t, write_f, end) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let write_node = |id: Uuid, content: &str| {
            let (exin, exout, path, text) =
                (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            Node {
                id,
                node_type: NodeType::Function,
                kind: "Tool".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    pin(exin, "Exec", PinType::ExecInput, DataType::Void),
                    pin(exout, "Exec", PinType::ExecOutput, DataType::Void),
                    pin_with_default(
                        path,
                        "path",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!("hit.txt"),
                    ),
                    pin_with_default(
                        text,
                        "content",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!(content),
                    ),
                    pin(Uuid::new_v4(), "Result", PinType::DataOutput, DataType::String),
                ],
                data: serde_json::json!({ "tool_name": "WriteFile" }),
            }
        };
        let (s_ex, b_exin, b_cond, b_res, b_true, b_false) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let e_exin = Uuid::new_v4();
        let write_t_node = write_node(write_t, "true");
        let write_f_node = write_node(write_f, "false");
        let t_exin = write_t_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput)
            .unwrap()
            .id;
        let f_exin = write_f_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput)
            .unwrap()
            .id;
        let t_exout = write_t_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
            .unwrap()
            .id;
        let f_exout = write_f_node
            .pins
            .iter()
            .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
            .unwrap()
            .id;
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "branch".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: branch,
                    node_type: NodeType::Control,
                    kind: "Branch".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(b_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin_with_default(
                            b_cond,
                            "Condition",
                            PinType::DataInput,
                            DataType::Bool,
                            serde_json::json!(condition),
                        ),
                        pin(b_res, "Result", PinType::DataOutput, DataType::Bool),
                        pin(b_true, "True", PinType::ExecOutput, DataType::Void),
                        pin(b_false, "False", PinType::ExecOutput, DataType::Void),
                    ],
                    data: serde_json::Value::Null,
                },
                write_t_node,
                write_f_node,
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: branch,
                    target_pin: b_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: branch,
                    source_pin: b_true,
                    target_node: write_t,
                    target_pin: t_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: branch,
                    source_pin: b_false,
                    target_node: write_f,
                    target_pin: f_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_t,
                    source_pin: t_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_f,
                    source_pin: f_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
            ],
            entry_node_id: start,
        };
        (blueprint, workspace)
    }

    #[tokio::test]
    async fn branch_routes_true_and_false() {
        for (condition, expected) in [(true, "true"), (false, "false")] {
            let (blueprint, workspace) = branch_blueprint(condition);
            let mut interpreter = Interpreter::new(
                Arc::new(Registry::with_builtins()),
                LlmClientFactory::new(),
                workspace.clone(),
            );
            interpreter.run(&shared(blueprint), None).await.unwrap();
            assert_eq!(std::fs::read_to_string(workspace.join("hit.txt")).unwrap(), expected);
        }
    }

    /// Builds Start -> ForEach([1,2,3]) -> Body: Set(last=Iteration) ->
    /// Completed -> Get(last) -> Validator(eq 3) -> End.
    fn foreach_blueprint(list: serde_json::Value) -> Blueprint {
        let (start, fe, set, get, validator, end) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (s_ex, fe_exin, fe_list, fe_iter, fe_idx, fe_body, fe_done) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (set_exin, set_exout, set_name, set_val, set_out) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (get_exin, get_exout, get_name, get_val) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (v_exin, v_exout, v_actual, v_expected, v_passed) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let e_exin = Uuid::new_v4();
        Blueprint {
            id: Uuid::new_v4(),
            name: "foreach".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: fe,
                    node_type: NodeType::Control,
                    kind: "ForEach".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(fe_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin_with_default(fe_list, "List", PinType::DataInput, DataType::Any, list),
                        pin(fe_iter, "Iteration", PinType::DataOutput, DataType::Any),
                        pin(fe_idx, "Index", PinType::DataOutput, DataType::Int),
                        pin(fe_body, "Body", PinType::ExecOutput, DataType::Void),
                        pin(fe_done, "Completed", PinType::ExecOutput, DataType::Void),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: set,
                    node_type: NodeType::Function,
                    kind: "VariableSet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(set_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(set_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin_with_default(
                            set_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("last"),
                        ),
                        pin(set_val, "Value", PinType::DataInput, DataType::Any),
                        pin(set_out, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: get,
                    node_type: NodeType::Pure,
                    kind: "VariableGet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(get_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(get_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin_with_default(
                            get_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("last"),
                        ),
                        pin(get_val, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: validator,
                    node_type: NodeType::Function,
                    kind: "Validator".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(v_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(v_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(v_actual, "Actual", PinType::DataInput, DataType::Any),
                        pin_with_default(
                            v_expected,
                            "Expected",
                            PinType::DataInput,
                            DataType::Any,
                            serde_json::json!(3),
                        ),
                        pin(v_passed, "Passed", PinType::DataOutput, DataType::Bool),
                    ],
                    data: serde_json::json!({ "mode": "eq" }),
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: fe,
                    target_pin: fe_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: fe,
                    source_pin: fe_body,
                    target_node: set,
                    target_pin: set_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: fe,
                    source_pin: fe_done,
                    target_node: get,
                    target_pin: get_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: get,
                    source_pin: get_exout,
                    target_node: validator,
                    target_pin: v_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: validator,
                    source_pin: v_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: fe,
                    source_pin: fe_iter,
                    target_node: set,
                    target_pin: set_val,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: get,
                    source_pin: get_val,
                    target_node: validator,
                    target_pin: v_actual,
                },
            ],
            entry_node_id: start,
        }
    }

    #[tokio::test]
    async fn foreach_iterates_in_order() {
        let blueprint = foreach_blueprint(serde_json::json!([1, 2, 3]));
        let mut interpreter = new_interpreter();
        interpreter.run(&shared(blueprint), None).await.unwrap();
    }

    #[tokio::test]
    async fn foreach_empty_list_skips_body() {
        // The body would set `flag`; reading it after an empty run must fail.
        let blueprint = foreach_blueprint(serde_json::json!([]));
        let mut interpreter = new_interpreter();
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("undefined variable")
        ));
    }

    #[tokio::test]
    async fn foreach_respects_iteration_limit() {
        use metteur_shared::config::{Config, ExecutionConfig};
        let blueprint = foreach_blueprint(serde_json::json!([1, 2, 3]));
        let config = Arc::new(RwLock::new(Config {
            execution: ExecutionConfig {
                circuit_break_after: 0,
                validation_max_attempts: 1,
                foreach_max_iterations: 2,
                ..Default::default()
            },
            ..Default::default()
        }));
        let mut interpreter = new_interpreter().with_config(config);
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("exceeded 2 iterations")
        ));
    }

    /// Builds Start -> RequestApproval -> WriteFile per outcome -> End.
    fn approval_blueprint() -> (Blueprint, PathBuf) {
        let workspace = std::env::temp_dir().join(format!("metteur-approval-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let (start, approval, write_a, write_d, end) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let write_node = |id: Uuid, content: &str| {
            let (exin, exout, path, text) =
                (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            Node {
                id,
                node_type: NodeType::Function,
                kind: "Tool".to_string(),
                position: (0.0, 0.0),
                pins: vec![
                    pin(exin, "Exec", PinType::ExecInput, DataType::Void),
                    pin(exout, "Exec", PinType::ExecOutput, DataType::Void),
                    pin_with_default(
                        path,
                        "path",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!("hit.txt"),
                    ),
                    pin_with_default(
                        text,
                        "content",
                        PinType::DataInput,
                        DataType::String,
                        serde_json::json!(content),
                    ),
                    pin(Uuid::new_v4(), "Result", PinType::DataOutput, DataType::String),
                ],
                data: serde_json::json!({ "tool_name": "WriteFile" }),
            }
        };
        let (s_ex, a_exin, a_msg, a_allowed, a_ok, a_no) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let e_exin = Uuid::new_v4();
        let write_a_node = write_node(write_a, "approved");
        let write_d_node = write_node(write_d, "denied");
        let a_in = |n: &Node| {
            n.pins.iter().find(|p| p.name == "Exec" && p.pin_type == PinType::ExecInput).unwrap().id
        };
        let a_out = |n: &Node| {
            n.pins
                .iter()
                .find(|p| p.name == "Exec" && p.pin_type == PinType::ExecOutput)
                .unwrap()
                .id
        };
        let (wa_in, wa_out, wd_in, wd_out) =
            (a_in(&write_a_node), a_out(&write_a_node), a_in(&write_d_node), a_out(&write_d_node));
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "approval".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: approval,
                    node_type: NodeType::Control,
                    kind: "RequestApproval".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(a_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin_with_default(
                            a_msg,
                            "Message",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("proceed?"),
                        ),
                        pin(a_allowed, "Allowed", PinType::DataOutput, DataType::Bool),
                        pin(a_ok, "Approved", PinType::ExecOutput, DataType::Void),
                        pin(a_no, "Denied", PinType::ExecOutput, DataType::Void),
                    ],
                    data: serde_json::Value::Null,
                },
                write_a_node,
                write_d_node,
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: approval,
                    target_pin: a_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: approval,
                    source_pin: a_ok,
                    target_node: write_a,
                    target_pin: wa_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: approval,
                    source_pin: a_no,
                    target_node: write_d,
                    target_pin: wd_in,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_a,
                    source_pin: wa_out,
                    target_node: end,
                    target_pin: e_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: write_d,
                    source_pin: wd_out,
                    target_node: end,
                    target_pin: e_exin,
                },
            ],
            entry_node_id: start,
        };
        (blueprint, workspace)
    }

    #[tokio::test]
    async fn request_approval_routes_both_outcomes() {
        use crate::sandbox::approval::{ApprovalBroker, Decision};
        for (decision, expected) in [(Decision::Allow, "approved"), (Decision::Deny, "denied")] {
            let (blueprint, workspace) = approval_blueprint();
            let broker = Arc::new(ApprovalBroker::new());
            let mut interpreter = Interpreter::new(
                Arc::new(Registry::with_builtins()),
                LlmClientFactory::new(),
                workspace.clone(),
            )
            .with_approvals(broker.clone());
            let blueprint_arc = Arc::new(PLock::new(blueprint));
            let task = tokio::spawn(async move { interpreter.run(&blueprint_arc, None).await });
            loop {
                let ids = broker.pending_ids();
                if !ids.is_empty() {
                    broker.respond(&ids[0], decision);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            task.await.unwrap().unwrap();
            assert_eq!(std::fs::read_to_string(workspace.join("hit.txt")).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn function_locals_do_not_leak_to_caller() {
        use metteur_shared::model::function::FunctionSignature;

        // Function body: Entry -> VariableSet(tmp) -> Exit.
        let func_id = Uuid::new_v4();
        let (entry_node, set, exit_node) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (en_ex, set_exin, set_exout, set_name, set_val, set_out, ex_exin) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let func = FunctionEntry {
            id: func_id,
            name: "SetTmp".to_string(),
            description: String::new(),
            signature: FunctionSignature {
                inputs: vec![],
                outputs: vec![],
            },
            body: Blueprint {
                id: func_id,
                name: "SetTmp".to_string(),
                nodes: vec![
                    Node {
                        id: entry_node,
                        node_type: NodeType::Event,
                        kind: "FunctionEntry".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![pin(en_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                        data: serde_json::Value::Null,
                    },
                    Node {
                        id: set,
                        node_type: NodeType::Function,
                        kind: "VariableSet".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![
                            pin(set_exin, "Exec", PinType::ExecInput, DataType::Void),
                            pin(set_exout, "Exec", PinType::ExecOutput, DataType::Void),
                            pin_with_default(
                                set_name,
                                "Name",
                                PinType::DataInput,
                                DataType::String,
                                serde_json::json!("tmp"),
                            ),
                            pin_with_default(
                                set_val,
                                "Value",
                                PinType::DataInput,
                                DataType::Any,
                                serde_json::json!(7),
                            ),
                            pin(set_out, "Value", PinType::DataOutput, DataType::Any),
                        ],
                        data: serde_json::Value::Null,
                    },
                    Node {
                        id: exit_node,
                        node_type: NodeType::Event,
                        kind: "FunctionExit".to_string(),
                        position: (0.0, 0.0),
                        pins: vec![pin(ex_exin, "Exec", PinType::ExecInput, DataType::Void)],
                        data: serde_json::Value::Null,
                    },
                ],
                edges: vec![
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: entry_node,
                        source_pin: en_ex,
                        target_node: set,
                        target_pin: set_exin,
                    },
                    Edge {
                        id: Uuid::new_v4(),
                        source_node: set,
                        source_pin: set_exout,
                        target_node: exit_node,
                        target_pin: ex_exin,
                    },
                ],
                entry_node_id: entry_node,
            },
            source: metteur_shared::model::function::FunctionSource::Builtin,
        };
        // Caller: Start -> Call(SetTmp) -> VariableGet(tmp) -> End.
        let (start, caller, get, end) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (s_ex, c_exin, c_exout, g_exin, g_name, g_val, e_exin) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "leak".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: caller,
                    node_type: NodeType::Function,
                    kind: "CallFunction".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(c_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(c_exout, "Exec", PinType::ExecOutput, DataType::Void),
                    ],
                    data: serde_json::json!({ "function": "SetTmp" }),
                },
                Node {
                    id: get,
                    node_type: NodeType::Pure,
                    kind: "VariableGet".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(g_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin_with_default(
                            g_name,
                            "Name",
                            PinType::DataInput,
                            DataType::String,
                            serde_json::json!("tmp"),
                        ),
                        pin(g_val, "Value", PinType::DataOutput, DataType::Any),
                    ],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: caller,
                    target_pin: c_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: caller,
                    source_pin: c_exout,
                    target_node: get,
                    target_pin: g_exin,
                },
            ],
            entry_node_id: start,
        };
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func);
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        let result = interpreter.run(&shared(blueprint), None).await;
        assert!(matches!(
            result,
            Err(DaemonError::Execution(msg)) if msg.contains("undefined variable")
        ));
    }

    #[tokio::test]
    async fn execution_tree_tracks_nodes_and_tokens() {
        let (start, llm, end) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (s_ex, l_exin, l_exout, e_exin) =
            (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "tree-test".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: llm,
                    node_type: NodeType::Function,
                    kind: "CallLLM".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![
                        pin(l_exin, "Exec", PinType::ExecInput, DataType::Void),
                        pin(l_exout, "Exec", PinType::ExecOutput, DataType::Void),
                        pin(Uuid::new_v4(), "Result", PinType::DataOutput, DataType::String),
                        pin(Uuid::new_v4(), "Context", PinType::DataOutput, DataType::Context),
                    ],
                    data: serde_json::json!({
                        "provider": "mock",
                        "mock_text": "ok",
                        "model": "mock",
                    }),
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![
                Edge {
                    id: Uuid::new_v4(),
                    source_node: start,
                    source_pin: s_ex,
                    target_node: llm,
                    target_pin: l_exin,
                },
                Edge {
                    id: Uuid::new_v4(),
                    source_node: llm,
                    source_pin: l_exout,
                    target_node: end,
                    target_pin: e_exin,
                },
            ],
            entry_node_id: start,
        };
        let mut interpreter = new_interpreter();
        interpreter.run(&shared(blueprint), None).await.unwrap();
        // The tree has a run root plus one node per executed blueprint node.
        assert_eq!(interpreter.tree.roots.len(), 1);
        let run = interpreter.tree.nodes.get(&interpreter.tree.roots[0]).expect("run root");
        assert!(matches!(run.kind, crate::execution::tree::TreeNodeKind::Run));
        assert_eq!(run.children.len(), 3);
        let done = interpreter
            .tree
            .nodes
            .values()
            .filter(|n| matches!(n.status, crate::execution::tree::TreeNodeStatus::Done))
            .count();
        assert_eq!(done, interpreter.tree.nodes.len());
    }

    #[tokio::test]
    async fn function_call_closes_function_and_caller_tree_nodes() {
        let func = add_function();
        let registry = Arc::new(Registry::with_builtins());
        registry.register_function(func);
        let blueprint = call_function_blueprint("AddFunc");
        let mut interpreter =
            Interpreter::new(registry, LlmClientFactory::new(), std::env::temp_dir());
        interpreter.run(&shared(blueprint), None).await.unwrap();
        // No tree node may remain Running after the run completes; the run
        // root, its blueprint nodes and every entered function frame are Done.
        let running = interpreter
            .tree
            .nodes
            .values()
            .filter(|n| matches!(n.status, crate::execution::tree::TreeNodeStatus::Running))
            .count();
        assert_eq!(running, 0, "all tree nodes must close: {:#?}", interpreter.tree.nodes);
        // The CallFunction node carries the entered function as a child.
        let call_node = interpreter.tree.nodes.values().find(|n| n.label == "CallFunction");
        assert!(call_node.is_some(), "CallFunction node must exist in the tree");
        let root = interpreter.tree.nodes.get(&interpreter.tree.roots[0]).expect("run root");
        assert!(matches!(root.kind, crate::execution::tree::TreeNodeKind::Run));
    }

    #[tokio::test]
    async fn execution_tree_survives_checkpoint_round_trip() {
        let (start, end) = (Uuid::new_v4(), Uuid::new_v4());
        let pin = |p_id: Uuid, name: &str, pin_type: PinType, data_type: DataType| Pin {
            id: p_id,
            name: name.to_string(),
            pin_type,
            data_type,
            ..Default::default()
        };
        let (s_ex, e_exin) = (Uuid::new_v4(), Uuid::new_v4());
        let blueprint = Blueprint {
            id: Uuid::new_v4(),
            name: "tree-cp".to_string(),
            nodes: vec![
                Node {
                    id: start,
                    node_type: NodeType::Event,
                    kind: "Start".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(s_ex, "Exec", PinType::ExecOutput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
                Node {
                    id: end,
                    node_type: NodeType::Event,
                    kind: "End".to_string(),
                    position: (0.0, 0.0),
                    pins: vec![pin(e_exin, "Exec", PinType::ExecInput, DataType::Void)],
                    data: serde_json::Value::Null,
                },
            ],
            edges: vec![Edge {
                id: Uuid::new_v4(),
                source_node: start,
                source_pin: s_ex,
                target_node: end,
                target_pin: e_exin,
            }],
            entry_node_id: start,
        };
        let sink = Arc::new(MemorySink::new());
        let mut interpreter = new_interpreter().with_checkpoint_sink(sink.clone());
        interpreter.run(&shared(blueprint.clone()), None).await.unwrap();
        let last = sink.checkpoints.lock().unwrap().last().unwrap().clone();
        assert!(!last.exec_tree.nodes.is_empty());
        // A fresh run of the same blueprint also populates a tree root.
        let mut interpreter = new_interpreter();
        interpreter.run(&shared(blueprint), None).await.unwrap();
        assert_eq!(interpreter.tree.roots.len(), 1);
    }
}
