//! Shared gRPC service state and helpers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use metteur_shared::config::{AclConfig, Config};
use metteur_shared::llm::{Message, ReasoningEffort, Role, ToolCall};
use tokio::sync::{RwLock, watch};
use tonic::Status;
use uuid::Uuid;

use crate::error::DaemonError;
use crate::execution::RunStatus;
use crate::execution::interrupt::InterruptBus;
use crate::execution::react::{DEFAULT_MAX_ITERATIONS, ReactOptions};
use crate::llm::LlmClientFactory;
use crate::llm::MockStep;
use crate::observability::audit::AuditWriter;
use crate::registry::Registry;
use crate::sandbox::approval::ApprovalBroker;
use crate::storage::persistence::Db;
use crate::workspace::WorkspaceManager;

use super::super::proto::{AddonInfo, ExecutionEvent};

/// The control state of a running execution.
#[derive(Clone)]
pub(crate) struct RunningExecution {
    /// Bus for injecting interrupts into the execution.
    pub(crate) interrupt_bus: Option<InterruptBus>,
    /// Set when a pause has been requested (shared with the execution).
    pub(crate) pause_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Set when a cancel has been requested (shared with the execution).
    pub(crate) cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Sandbox approval channel shared with the execution.
    pub(crate) approvals: Option<Arc<ApprovalBroker>>,
}

/// The control state of a running chat session.
#[derive(Clone)]
pub(crate) struct ChatRun {
    /// Bus for injecting interrupts into the ReAct loop.
    pub(crate) interrupt_bus: Option<InterruptBus>,
    /// Set when an abort has been requested (shared with the chat task).
    pub(crate) cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Sandbox approval channel shared with the chat task.
    pub(crate) approvals: Option<Arc<ApprovalBroker>>,
    /// Whether the task may persist the session on completion. Cleared by
    /// `delete_chat_session` so a late finalize cannot resurrect a cleared
    /// session.
    pub(crate) persist: Arc<std::sync::atomic::AtomicBool>,
    /// The thread this chat turn runs on; lets deletion target the right one.
    pub(crate) session_id: Option<Uuid>,
}

/// Shared application state passed to the gRPC service.
pub struct AppState {
    /// The workspace manager.
    pub workspaces: WorkspaceManager,
    /// The resource registry.
    pub registry: Arc<Registry>,
    /// The global configuration.
    pub global_config: RwLock<Config>,
    /// The LLM client factory.
    pub llm_factory: LlmClientFactory,
    /// The global database (global audit), when enabled.
    pub global_db: Option<Db>,
    /// The global audit writer.
    pub global_audit: Option<AuditWriter>,
    /// Process-wide metrics.
    pub metrics: Arc<crate::observability::metrics::Metrics>,
    /// The MCP host, when enabled at startup.
    pub mcp_host: Option<Arc<crate::integration::mcp::McpHost>>,
    /// The addon host.
    pub addon_host: Option<Arc<crate::addon::AddonHost>>,
    /// The live ACL rules read by the interceptor on every request.
    pub acl_store: Arc<std::sync::RwLock<AclConfig>>,
    /// Workspaces with a currently running execution.
    pub(crate) running: RwLock<HashMap<PathBuf, RunningExecution>>,
    /// Workspaces with a currently running chat session.
    pub(crate) chats: RwLock<HashMap<PathBuf, ChatRun>>,
    /// Workspace-scoped blueprint function names per workspace root, so only
    /// the closing workspace's functions are retired from the shared registry.
    pub(crate) ws_functions: RwLock<HashMap<PathBuf, Vec<String>>>,
    /// Broadcast channel for global config changes.
    pub(crate) config_tx: watch::Sender<Config>,
}

impl AppState {
    /// Creates a new application state sharing the given registry.
    pub fn new(
        workspaces: WorkspaceManager,
        registry: Arc<Registry>,
        global_config: Config,
    ) -> Self {
        let acl_store = Arc::new(std::sync::RwLock::new(global_config.acl.clone()));
        let (config_tx, config_rx) = watch::channel(global_config.clone());
        spawn_config_reloader(config_rx, acl_store.clone());
        Self {
            workspaces,
            registry,
            global_config: RwLock::new(global_config),
            llm_factory: LlmClientFactory::new(),
            global_db: None,
            global_audit: None,
            metrics: Arc::new(crate::observability::metrics::Metrics::default()),
            mcp_host: None,
            addon_host: None,
            acl_store,
            running: RwLock::new(HashMap::new()),
            chats: RwLock::new(HashMap::new()),
            ws_functions: RwLock::new(HashMap::new()),
            config_tx,
        }
    }

    /// Attaches the global database for global audit logging.
    pub fn with_global_db(mut self, db: Db) -> Self {
        self.global_db = Some(db.clone());
        // Global blueprint functions live only in this database; load them so
        // they survive a daemon restart and are callable/listed again.
        if let Err(err) = self
            .registry
            .load_functions(&db, metteur_shared::model::function::FunctionSource::Global)
        {
            tracing::warn!("failed to load global functions: {err}");
        }
        self.global_audit = Some(AuditWriter::new(db));
        self
    }

    /// Replaces the shared metrics instance (so hosts can pre-register).
    pub fn with_metrics(mut self, metrics: Arc<crate::observability::metrics::Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Attaches the MCP host.
    ///
    /// Syncing is explicit: [`AppState::resync_mcp`] is called from every place
    /// that can change the server set (global or workspace config writes, and
    /// workspace open/close). Driving it from the global `config_tx` alone
    /// would drop workspace-declared servers, and running both paths would
    /// race on the same host.
    pub fn with_mcp_host(mut self, host: Arc<crate::integration::mcp::McpHost>) -> Self {
        self.mcp_host = Some(host);
        self
    }

    /// Builds the effective MCP configuration for the whole daemon.
    ///
    /// Server processes are daemon-wide, so the effective set is the union of
    /// the global servers and those declared by every open workspace, with a
    /// workspace definition winning on an alias collision (the more specific
    /// scope). Section-level timeouts keep the global value, since one host
    /// carries only one.
    pub async fn merged_mcp_config(&self) -> metteur_shared::config::McpConfig {
        let global = self.global_config.read().await.mcp.clone();
        let mut workspace_configs = Vec::new();
        for ws in self.workspaces.list().await {
            workspace_configs.push(ws.config.read().await.mcp.clone());
        }
        crate::integration::mcp::merge_servers(&global, &workspace_configs)
    }

    /// Pushes the effective MCP configuration to the host.
    ///
    /// Called after anything that can change the server set: a global or
    /// workspace config write, and workspace open/close. `McpHost::sync` is
    /// incremental (unchanged servers keep their connection, vanished ones are
    /// shut down), so this stays cheap when nothing relevant changed.
    pub async fn resync_mcp(&self) {
        let Some(host) = &self.mcp_host else {
            return;
        };
        let config = self.merged_mcp_config().await;
        host.sync(&config).await;
    }

    /// Attaches the addon host and loads the global addon directory.
    pub async fn with_addon_host(mut self, host: Arc<crate::addon::AddonHost>) -> Self {
        host.rescan().await;
        self.addon_host = Some(host);
        self
    }
}

/// Applies global config changes to the shared ACL store.
fn spawn_config_reloader(
    mut rx: watch::Receiver<Config>,
    acl_store: Arc<std::sync::RwLock<AclConfig>>,
) {
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let config = rx.borrow().clone();
            if let Ok(mut acl) = acl_store.write() {
                *acl = config.acl;
            }
        }
    });
}

/// Converts an engine event into its protobuf representation.
fn proto_event(event: crate::execution::ExecutionEvent) -> ExecutionEvent {
    use crate::execution::ExecutionEvent as E;
    match event {
        E::NodeStarted {
            node_id,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "started".to_string(),
            message: String::new(),
            detail_json: String::new(),
        },
        E::NodeFinished {
            node_id,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "finished".to_string(),
            message: String::new(),
            detail_json: String::new(),
        },
        E::Message {
            node_id,
            message,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "message".to_string(),
            message,
            detail_json: String::new(),
        },
        E::ApprovalRequested {
            node_id,
            request_id,
            detail,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "approval_request".to_string(),
            message: request_id,
            detail_json: detail,
        },
        E::Todos {
            node_id,
            todos,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "todos".to_string(),
            message: String::new(),
            detail_json: serde_json::json!({ "todos": todos }).to_string(),
        },
        E::Job {
            node_id,
            job_id,
            state,
            summary,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "job".to_string(),
            message: summary.clone(),
            detail_json: serde_json::json!({ "job_id": job_id, "state": state }).to_string(),
        },
        E::ContextUsage {
            node_id,
            regions,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "context".to_string(),
            message: String::new(),
            detail_json: serde_json::to_string(&serde_json::json!({ "regions": regions }))
                .unwrap_or_default(),
        },
        E::NodeData {
            node_id,
            outputs,
            function,
        } => ExecutionEvent {
            node_id: node_id.to_string(),
            kind: "node_data".to_string(),
            message: String::new(),
            detail_json: serde_json::to_string(&serde_json::json!({
                "outputs": outputs
                    .iter()
                    .map(|(id, value)| (
                        id.to_string(),
                        crate::execution::nodes::value_to_json(value),
                    ))
                    .collect::<serde_json::Map<_, _>>(),
                "function": function.map(|f| f.to_string()),
            }))
            .unwrap_or_default(),
        },
    }
}

/// Shared setup for streaming execution RPCs.
///
/// Registers the per-workspace run slot, spawns the interpreter with live
/// event forwarding and returns a receiver stream of protobuf events. The
/// run slot is released by the forwarding task once the stream completes.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_execution(
    state: &Arc<AppState>,
    ws_key: PathBuf,
    ws_db: Db,
    ws_config: Arc<RwLock<Config>>,
    workspace_root: PathBuf,
    registry: Arc<Registry>,
    llm_factory: LlmClientFactory,
    audit_writer: AuditWriter,
    subject: String,
    sink: Arc<dyn crate::execution::CheckpointSink>,
    blueprint: metteur_shared::Blueprint,
    resume: Option<crate::execution::ExecutionCheckpoint>,
    interrupt_bus: InterruptBus,
    pause_flag: Arc<std::sync::atomic::AtomicBool>,
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    lsp: Option<Arc<crate::integration::lsp::LspManager>>,
    addon_fragments: Vec<metteur_shared::llm::SystemFragment>,
    version_manager: Option<Arc<crate::storage::versioning::VersionManager>>,
    jobs: Arc<crate::execution::JobManager>,
) -> Result<tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>, Status> {
    let workspace = state
        .workspaces
        .get(&ws_key)
        .await
        .ok_or_else(|| Status::not_found("workspace not open"))?;
    let _admission = workspace.activity_gate.lock().await;
    if state.chats.read().await.contains_key(&ws_key) {
        return Err(Status::failed_precondition("workspace already has a running chat"));
    }
    let broker = Arc::new(ApprovalBroker::new());
    {
        // Check and register under one write lock so two concurrent requests
        // cannot both claim the workspace's single execution slot.
        let mut running = state.running.write().await;
        if running.contains_key(&ws_key) {
            return Err(Status::failed_precondition("workspace already has a running execution"));
        }
        running.insert(
            ws_key.clone(),
            RunningExecution {
                interrupt_bus: Some(interrupt_bus.clone()),
                pause_requested: pause_flag.clone(),
                cancel_requested: cancel_flag.clone(),
                approvals: Some(broker.clone()),
            },
        );
    }
    state.metrics.executions_running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let global_db = state.global_db.clone();
    let run_metrics = state.metrics.clone();
    let run_id = sink.run_id();
    let shared_blueprint: crate::execution::SharedBlueprint =
        Arc::new(parking_lot::RwLock::new(blueprint));
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::execution::ExecutionEvent>();
    let (err_tx, err_rx) = tokio::sync::oneshot::channel();

    let interp_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut interpreter =
            crate::execution::Interpreter::new(registry, llm_factory, workspace_root)
                .with_checkpoint_sink(sink)
                .with_audit(audit_writer)
                .with_config(ws_config)
                .with_user(subject)
                .with_event_tx(interp_tx)
                .with_approvals(broker)
                .with_metrics(run_metrics)
                .with_workspace_db(ws_db);
        if let Some(lsp) = lsp {
            interpreter = interpreter.with_lsp(lsp);
        }
        interpreter = interpreter.with_addon_fragments(addon_fragments);
        if let Some(db) = global_db {
            interpreter = interpreter.with_global_db(db);
        }
        if let Some(version_manager) = version_manager {
            interpreter = interpreter.with_version_manager(version_manager);
        }
        let run_jobs = Arc::clone(&jobs);
        interpreter = interpreter.with_jobs(jobs);
        let result = match resume {
            Some(checkpoint) => {
                interpreter
                    .resume_with_control(
                        &shared_blueprint,
                        checkpoint,
                        Some(interrupt_bus),
                        pause_flag,
                        cancel_flag,
                    )
                    .await
            }
            None => {
                interpreter
                    .run_with_control(
                        &shared_blueprint,
                        Some(interrupt_bus),
                        pause_flag,
                        cancel_flag,
                    )
                    .await
            }
        };
        drop(interpreter);
        // Background commands belong to the run that started them: whatever the
        // outcome, they must not outlive it as orphan processes.
        let killed = run_jobs.kill_owned_by(run_id);
        if killed > 0 {
            tracing::info!("run {run_id} ended; terminated {killed} background job(s)");
        }
        let _ = err_tx.send(result.map(|_| ()));
    });
    drop(event_tx);

    let (out_tx, out_rx) = tokio::sync::mpsc::channel(64);
    let state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            if out_tx.send(Ok(proto_event(event))).await.is_err() {
                break;
            }
        }
        match err_rx.await {
            Ok(Err(err)) => {
                let _ = out_tx.send(Err(to_status(err))).await;
                state.metrics.executions_failed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(Ok(())) => {
                state
                    .metrics
                    .executions_completed
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(_) => {}
        }
        state.metrics.executions_running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        state.running.write().await.remove(&ws_key);
    });

    Ok(tokio_stream::wrappers::ReceiverStream::new(out_rx))
}

/// Converts a daemon error into a tonic status.
pub(crate) fn to_status(err: DaemonError) -> Status {
    Status::new(err.to_status(), err.to_string())
}

/// Maps a filesystem error to a tonic status, keeping the common kinds readable.
pub(crate) fn io_status(err: std::io::Error) -> Status {
    match err.kind() {
        std::io::ErrorKind::NotFound => Status::not_found(err.to_string()),
        std::io::ErrorKind::PermissionDenied => Status::permission_denied(err.to_string()),
        _ => Status::internal(format!("io error: {err}")),
    }
}

/// Resolves a workspace-relative path against the workspace root.
///
/// Absolute paths, `..` traversal and any path under the `/.metteur` metadata
/// directory are rejected. The check is lexical: Windows `canonicalize` output
/// (with its `\\?\` prefix) is not stable across calls, so containment cannot
/// rely on it.
pub(crate) fn resolve_ws_path(
    root: &std::path::Path,
    relative: &str,
) -> Result<std::path::PathBuf, Status> {
    use std::path::Component;
    if relative.is_empty() {
        return Ok(root.to_path_buf());
    }
    let rel = std::path::Path::new(relative);
    let invalid = rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        || rel.starts_with(crate::workspace::METADATA_DIR);
    if invalid {
        return Err(Status::invalid_argument("workspace paths must be relative, outside metadata"));
    }
    Ok(root.join(rel))
}

/// Builds ReAct options for a chat turn from the client-provided JSON.
pub(crate) fn chat_options(options_json: &str) -> ReactOptions {
    let data: serde_json::Value =
        serde_json::from_str(options_json).unwrap_or(serde_json::Value::Null);
    let string = |key: &str| {
        data.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
    };
    ReactOptions {
        provider: string("provider").unwrap_or_else(|| "openai-chat".to_string()),
        model: string("model"),
        temperature: data.get("temperature").and_then(|v| v.as_f64()),
        top_p: data.get("top_p").and_then(|v| v.as_f64()),
        max_tokens: data.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as u32),
        reasoning_effort: string("reasoning_effort").and_then(|s| match s.as_str() {
            "none" => Some(ReasoningEffort::None),
            "low" => Some(ReasoningEffort::Low),
            "medium" => Some(ReasoningEffort::Medium),
            "high" => Some(ReasoningEffort::High),
            _ => None,
        }),
        max_iterations: data
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_ITERATIONS),
        mock_text: string("mock_text"),
        mock_delay_ms: data.get("mock_delay_ms").and_then(|v| v.as_u64()),
        mock_steps: parse_mock_steps(&data),
        ..ReactOptions::default()
    }
}

/// Parses scripted mock steps out of the chat options.
///
/// Only the `mock` provider reads this. It exists so a tool-calling turn can be
/// driven through the chat RPC in a test: a single scripted text answer cannot
/// produce one, and the transcript's tool handling is exactly what needs
/// covering.
fn parse_mock_steps(data: &serde_json::Value) -> Option<Vec<MockStep>> {
    let steps = data.get("mock_steps")?.as_array()?;
    let parsed: Vec<MockStep> = steps.iter().filter_map(parse_mock_step).collect();
    (!parsed.is_empty()).then_some(parsed)
}

/// Parses one scripted step: `{"text": "..."}` or `{"tool_calls": [...]}`.
fn parse_mock_step(step: &serde_json::Value) -> Option<MockStep> {
    if let Some(text) = step.get("text").and_then(|value| value.as_str()) {
        return Some(MockStep::Text(text.to_string()));
    }
    let calls: Vec<ToolCall> = step
        .get("tool_calls")?
        .as_array()?
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            let name = call.get("name")?.as_str()?.to_string();
            Some(ToolCall {
                id: format!("mock-call-{index}"),
                name,
                arguments: call.get("arguments").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    (!calls.is_empty()).then_some(MockStep::Tools(calls))
}

/// Converts the client-supplied conversation history into context messages.
///
/// Tool messages are skipped: their results belong to prior assistant turns
/// and cannot be reproduced without their tool call ids.
pub(crate) fn history_messages(history_json: &str, out: &mut Vec<Message>) {
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(history_json) else {
        return;
    };
    for entry in entries {
        let Some(role) = entry.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(content) = entry.get("content").and_then(|v| v.as_str()) else {
            continue;
        };
        match role {
            "user" => out.push(Message::text(Role::User, content.to_string())),
            "assistant" => out.push(Message::text(Role::Assistant, content.to_string())),
            _ => {}
        }
    }
}

/// Resolves an optional workspace path argument.
///
/// Empty means global scope. The path itself is not validated here: scope
/// directories are created on demand and closed workspaces stay addressable.
pub(crate) fn optional_workspace(raw: &str) -> Result<Option<std::path::PathBuf>, Status> {
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

/// Converts an addon snapshot into its protobuf form.
pub(crate) fn addon_info_to_proto(info: crate::addon::AddonInfoData) -> AddonInfo {
    AddonInfo {
        id: info.id,
        version: info.version,
        name: info.name,
        description: info.description,
        enabled: info.enabled,
        scope: info.scope,
        required_permissions: info.required_permissions,
        granted_permissions: info.granted_permissions,
        tool_count: info.tool_count,
        fragment_count: info.fragment_count,
    }
}

/// Records an entry in the global audit log, if one is attached.
pub(crate) fn record_global_audit(
    state: &AppState,
    subject: &str,
    operation: &str,
    detail: serde_json::Value,
) {
    if let Some(writer) = &state.global_audit {
        let _ = writer.record(subject, operation, detail);
    }
}

/// Converts a run status to its wire string.
pub(crate) fn status_str(status: RunStatus) -> String {
    match status {
        RunStatus::Running => "Running",
        RunStatus::Suspended => "Suspended",
        RunStatus::Completed => "Completed",
        RunStatus::Failed => "Failed",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("metteur-service-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn resolve_path_allows_internal_and_root() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let resolved = resolve_ws_path(&root, "a.txt").unwrap();
        assert_eq!(resolved, root.join("a.txt"));
        assert_eq!(resolve_ws_path(&root, "").unwrap(), root);
    }

    #[test]
    fn resolve_path_rejects_absolute_metadata_and_escape() {
        let root = temp_root();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        let abs = resolve_ws_path(&root, "C:/Windows/System32");
        assert!(abs.is_err());

        let meta = resolve_ws_path(&root, ".metteur/db");
        assert!(meta.is_err());

        let escape = resolve_ws_path(&root, "../outside.txt");
        assert!(escape.is_err());
    }

    #[test]
    fn chat_options_parses_overrides() {
        let opts = chat_options(
            r#"{"model":"claude-4","temperature":0.2,"max_iterations":3,"reasoning_effort":"high"}"#,
        );
        assert_eq!(opts.model.as_deref(), Some("claude-4"));
        assert_eq!(opts.temperature, Some(0.2));
        assert_eq!(opts.max_iterations, 3);
        assert_eq!(opts.reasoning_effort, Some(ReasoningEffort::High));

        let defaults = chat_options("{}");
        assert_eq!(defaults.model, None);
        assert_eq!(defaults.max_iterations, DEFAULT_MAX_ITERATIONS);
    }

    #[test]
    fn history_messages_maps_roles_and_skips_tools() {
        let mut out = Vec::new();
        history_messages(
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"tool","content":"result"}]"#,
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role, Role::User);
        assert_eq!(out[1].role, Role::Assistant);
    }
}
