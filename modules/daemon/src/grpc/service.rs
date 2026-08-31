//! gRPC service implementation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use metteur_shared::config::{AclConfig, Config};
use tokio::sync::{RwLock, watch};
use tonic::{Request, Response, Status};

use crate::audit::AuditWriter;
use crate::error::DaemonError;
use crate::execution::context::ExecutionContext;
use crate::execution::interrupt::{Interrupt, InterruptBus, InterruptPriority};
use crate::execution::react::{DEFAULT_MAX_ITERATIONS, ReactEvent, ReactOptions, run_react_streaming};
use crate::execution::{DbCheckpointSink, RunStatus};
use crate::llm::LlmClientFactory;
use crate::persistence::Db;
use crate::registry::Registry;
use crate::sandbox::approval::ApprovalBroker;
use crate::workspace::WorkspaceManager;
use metteur_shared::llm::{ContextManager, Message, ReasoningEffort, Role};
use metteur_shared::model::function::{FnPin, FunctionEntry, FunctionSource};

use super::acl::subject_from_request;
use super::proto::daemon_server::Daemon;
use super::proto::{
    self, AbortChatRequest, AddonInfo, AddonList, ApprovalDecisionRequest,
    AuditEntry as ProtoAuditEntry, AuditLogList, Blueprint, CancelRequest, ChatEvent,
    CloseWorkspaceRequest, Config as ProtoConfig, ContinueExecutionRequest, CreateDirRequest,
    CreateSnapshotRequest, DeleteFunctionRequest, Empty, ExecuteBlueprintRequest, ExecutionEvent,
    ExecutionInfo, ExecutionList, FileEntry, FileHistory, FileHistoryEntry, FileInfo, FileList,
    FnPin as ProtoFnPin, FunctionInfo as ProtoFunctionInfo, FunctionList, GetConfigRequest,
    CompileDslRequest, DecompileBlueprintRequest, DecompileDslResponse,
    GetExecutionUsageRequest, GetFileHistoryRequest, InstallAddonRequest, InterruptRequest,
    ListAddonsRequest, ListAuditLogRequest, ListExecutionsRequest, ListFilesRequest,
    ListFunctionsRequest, ListSnapshotsRequest, LoadBlueprintRequest, LoadFunctionRequest,
    LoadFunctionResponse, McpServerInfo, McpServerList, NodeKindList, OpenWorkspaceRequest,
    PauseRequest, ReadFileRequest, ReadFileResponse, RemoveFileRequest, RenameFileRequest,
    ResumeRequest, RollbackRequest, SaveBlueprintRequest, SaveFunctionRequest,
    SaveFunctionResponse, SendChatRequest, SetAddonEnabledRequest, SetConfigRequest, SnapshotInfo,
    SnapshotList, StatFileRequest, ToolInfo, ToolList, UninstallAddonRequest, UsageSummary,
    WatchEvent, WatchWorkspaceRequest, WriteFileRequest, WorkspaceInfo, WorkspaceList,
};

/// The control state of a running execution.
#[derive(Clone)]
struct RunningExecution {
    /// Bus for injecting interrupts into the execution.
    interrupt_bus: Option<InterruptBus>,
    /// Set when a pause has been requested (shared with the execution).
    pause_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Set when a cancel has been requested (shared with the execution).
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Sandbox approval channel shared with the execution.
    approvals: Option<Arc<ApprovalBroker>>,
}

/// The control state of a running chat session.
#[derive(Clone)]
struct ChatRun {
    /// Bus for injecting interrupts into the ReAct loop.
    interrupt_bus: Option<InterruptBus>,
    /// Set when an abort has been requested (shared with the chat task).
    cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Sandbox approval channel shared with the chat task.
    approvals: Option<Arc<ApprovalBroker>>,
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
    pub metrics: Arc<crate::metrics::Metrics>,
    /// The MCP host, when enabled at startup.
    pub mcp_host: Option<Arc<crate::mcp::McpHost>>,
    /// The addon host.
    pub addon_host: Option<Arc<crate::addon::AddonHost>>,
    /// The live ACL rules read by the interceptor on every request.
    pub acl_store: Arc<std::sync::RwLock<AclConfig>>,
    /// Workspaces with a currently running execution.
    running: RwLock<HashMap<PathBuf, RunningExecution>>,
    /// Workspaces with a currently running chat session.
    chats: RwLock<HashMap<PathBuf, ChatRun>>,
    /// Broadcast channel for global config changes.
    config_tx: watch::Sender<Config>,
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
            metrics: Arc::new(crate::metrics::Metrics::default()),
            mcp_host: None,
            addon_host: None,
            acl_store,
            running: RwLock::new(HashMap::new()),
            chats: RwLock::new(HashMap::new()),
            config_tx,
        }
    }

    /// Attaches the global database for global audit logging.
    pub fn with_global_db(mut self, db: Db) -> Self {
        self.global_audit = Some(AuditWriter::new(db.clone()));
        self.global_db = Some(db);
        self
    }

    /// Replaces the shared metrics instance (so hosts can pre-register).
    pub fn with_metrics(mut self, metrics: Arc<crate::metrics::Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Attaches the MCP host and starts its config-driven resync task.
    pub fn with_mcp_host(mut self, host: Arc<crate::mcp::McpHost>) -> Self {
        let mut config_rx = self.config_tx.subscribe();
        let task_host = host.clone();
        tokio::spawn(async move {
            while config_rx.changed().await.is_ok() {
                let mcp = config_rx.borrow().mcp.clone();
                task_host.sync(&mcp).await;
            }
        });
        self.mcp_host = Some(host);
        self
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
        E::ContextUsage {
        node_id,
        regions,
    } => ExecutionEvent {
        node_id: node_id.to_string(),
        kind: "context".to_string(),
        message: String::new(),
        detail_json: serde_json::to_string(
            &serde_json::json!({ "regions": regions }),
        )
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
async fn spawn_execution(
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
    lsp: Option<Arc<crate::lsp::LspManager>>,
    addon_fragments: Vec<metteur_shared::llm::SystemFragment>,
) -> Result<tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>, Status> {
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

/// The tonic service implementing the `Daemon` RPCs.
pub struct DaemonService {
    state: Arc<AppState>,
}

impl DaemonService {
    /// Creates a new service backed by the given state.
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
        }
    }

    /// Returns the approval broker of the active run (execution or chat) for a
    /// workspace, if any.
    async fn approval_broker_for(&self, ws_key: &std::path::Path) -> Option<Arc<ApprovalBroker>> {
        if let Some(entry) = self.state.running.read().await.get(ws_key) {
            return entry.approvals.clone();
        }
        self.state.chats.read().await.get(ws_key).and_then(|c| c.approvals.clone())
    }
}

/// Converts a daemon error into a tonic status.
fn to_status(err: DaemonError) -> Status {
    Status::new(err.to_status(), err.to_string())
}

/// Maps a filesystem error to a tonic status, keeping the common kinds readable.
fn io_status(err: std::io::Error) -> Status {
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
fn resolve_ws_path(root: &std::path::Path, relative: &str) -> Result<std::path::PathBuf, Status> {
    use std::path::Component;
    if relative.is_empty() {
        return Ok(root.to_path_buf());
    }
    let rel = std::path::Path::new(relative);
    let invalid = rel.is_absolute()
        || rel.components().any(|c| {
            matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_))
        })
        || rel.starts_with(crate::workspace::METADATA_DIR);
    if invalid {
        return Err(Status::invalid_argument("workspace paths must be relative, outside metadata"));
    }
    Ok(root.join(rel))
}

/// Builds ReAct options for a chat turn from the client-provided JSON.
fn chat_options(options_json: &str) -> ReactOptions {
    let data: serde_json::Value =
        serde_json::from_str(options_json).unwrap_or(serde_json::Value::Null);
    let string = |key: &str| {
        data.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
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
        ..ReactOptions::default()
    }
}

/// Converts the client-supplied conversation history into context messages.
///
/// Tool messages are skipped: their results belong to prior assistant turns
/// and cannot be reproduced without their tool call ids.
fn history_messages(history_json: &str, out: &mut Vec<Message>) {
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(history_json) else {
        return;
    };
    for entry in entries {
        let Some(role) = entry.get("role").and_then(|v| v.as_str()) else { continue };
        let Some(content) = entry.get("content").and_then(|v| v.as_str()) else { continue };
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
fn optional_workspace(raw: &str) -> Result<Option<std::path::PathBuf>, Status> {
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(raw)))
}

/// Converts an addon snapshot into its protobuf form.
fn addon_info_to_proto(info: crate::addon::AddonInfoData) -> AddonInfo {
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
fn record_global_audit(
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
fn status_str(status: RunStatus) -> String {
    match status {
        RunStatus::Running => "Running",
        RunStatus::Suspended => "Suspended",
        RunStatus::Completed => "Completed",
        RunStatus::Failed => "Failed",
    }
    .to_string()
}

#[tonic::async_trait]
impl Daemon for DaemonService {
    async fn open_workspace(
        &self,
        request: Request<OpenWorkspaceRequest>,
    ) -> Result<Response<WorkspaceInfo>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws = self.state.workspaces.open(&PathBuf::from(req.path)).await.map_err(to_status)?;
        // Register this workspace's function library into the shared registry.
        if let Err(err) = self.state.registry.load_functions(&ws.db, FunctionSource::Workspace) {
            tracing::warn!("failed to load workspace functions: {err}");
        }
        record_global_audit(
            &self.state,
            &subject,
            "workspace.open",
            serde_json::json!({ "path": ws.root().to_string_lossy() }),
        );
        self.state.metrics.workspaces_active.store(
            self.state.workspaces.list().await.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(Response::new(WorkspaceInfo {
            path: ws.root().to_string_lossy().to_string(),
            locked: true,
        }))
    }

    async fn close_workspace(
        &self,
        request: Request<CloseWorkspaceRequest>,
    ) -> Result<Response<Empty>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        self.state.workspaces.close(&PathBuf::from(req.path)).await.map_err(to_status)?;
        // Retire workspace-scoped functions from the shared registry.
        let names: Vec<String> = self
            .state
            .registry
            .functions()
            .into_iter()
            .filter(|f| f.source == FunctionSource::Workspace)
            .map(|f| f.name)
            .collect();
        for name in names {
            self.state.registry.unregister_function(&name);
        }
        record_global_audit(&self.state, &subject, "workspace.close", serde_json::json!({}));
        self.state.metrics.workspaces_active.store(
            self.state.workspaces.list().await.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(Response::new(Empty {}))
    }

    async fn list_workspaces(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<WorkspaceList>, Status> {
        let workspaces = self.state.workspaces.list().await;
        let infos = workspaces
            .into_iter()
            .map(|ws| WorkspaceInfo {
                path: ws.root().to_string_lossy().to_string(),
                locked: true,
            })
            .collect();
        Ok(Response::new(WorkspaceList {
            workspaces: infos,
        }))
    }

    async fn save_blueprint(
        &self,
        request: Request<SaveBlueprintRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let proto_blueprint =
            req.blueprint.ok_or_else(|| Status::invalid_argument("blueprint is required"))?;
        let blueprint = proto_to_blueprint(&proto_blueprint).map_err(to_status)?;
        let data = serde_json::to_vec(&blueprint).map_err(|e| Status::internal(e.to_string()))?;
        ws.db
            .put(crate::persistence::cf::BLUEPRINTS, blueprint.id.as_bytes(), &data)
            .map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn load_blueprint(
        &self,
        request: Request<LoadBlueprintRequest>,
    ) -> Result<Response<Blueprint>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let id = uuid::Uuid::parse_str(&req.blueprint_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let data = ws
            .db
            .get(crate::persistence::cf::BLUEPRINTS, id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(blueprint_to_proto(&blueprint)))
    }

    type ExecuteBlueprintStream =
        tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>;

    async fn execute_blueprint(
        &self,
        request: Request<ExecuteBlueprintRequest>,
    ) -> Result<Response<Self::ExecuteBlueprintStream>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let id = uuid::Uuid::parse_str(&req.blueprint_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let data = ws
            .db
            .get(crate::persistence::cf::BLUEPRINTS, id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        let run_id = uuid::Uuid::new_v4();
        let ws_key = ws.root().to_path_buf();

        let interrupt_bus = InterruptBus::new();
        let pause_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };

        let stream = spawn_execution(
            &self.state,
            ws_key,
            ws.db.clone(),
            ws.config.clone(),
            ws.root().to_path_buf(),
            self.state.registry.clone(),
            self.state.llm_factory.clone(),
            AuditWriter::new(ws.db.clone()),
            subject,
            Arc::new(DbCheckpointSink::new(ws.db.clone(), run_id)),
            blueprint,
            None,
            interrupt_bus,
            pause_flag,
            cancel_flag,
            ws.lsp_manager.clone(),
            addon_fragments,
        )
        .await?;

        Ok(Response::new(stream))
    }

    async fn cancel_execution(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(broker) = &entry.approvals {
            broker.deny_all();
        }
        Ok(Response::new(Empty {}))
    }

    async fn pause_execution(
        &self,
        request: Request<PauseRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.pause_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Response::new(Empty {}))
    }

    async fn resume_execution(
        &self,
        request: Request<ResumeRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.pause_requested.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(Response::new(Empty {}))
    }

    async fn send_interrupt(
        &self,
        request: Request<InterruptRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let priority = match req.priority.as_str() {
            "Urgent" => InterruptPriority::Urgent,
            "Emergency" => InterruptPriority::Emergency,
            _ => InterruptPriority::Normal,
        };
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        if let Some(bus) = &entry.interrupt_bus {
            bus.send(Interrupt {
                priority,
                message: req.message,
            });
        }
        Ok(Response::new(Empty {}))
    }

    async fn list_tools(&self, _request: Request<Empty>) -> Result<Response<ToolList>, Status> {
        let tools = self
            .state
            .registry
            .tools()
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
            })
            .collect();
        Ok(Response::new(ToolList {
            tools,
        }))
    }

    async fn get_config(
        &self,
        request: Request<GetConfigRequest>,
    ) -> Result<Response<ProtoConfig>, Status> {
        let req = request.into_inner();
        let config = if req.workspace_path.is_empty() {
            self.state.global_config.read().await.clone()
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            ws.config.read().await.clone()
        };
        let json = serde_json::to_string(&config).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ProtoConfig {
            config_json: json,
        }))
    }

    async fn set_config(
        &self,
        request: Request<SetConfigRequest>,
    ) -> Result<Response<Empty>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let config: Config = serde_json::from_str(&req.config_json)
            .map_err(|e| Status::invalid_argument(format!("invalid config: {e}")))?;

        if req.workspace_path.is_empty() {
            // Persist to the global config file.
            let dir = crate::config::global_config_dir().map_err(to_status)?;
            std::fs::create_dir_all(&dir).map_err(|e| to_status(DaemonError::Io(e)))?;
            let path = dir.join(crate::config::CONFIG_FILE);
            let toml = toml::to_string(&config).map_err(|e| Status::internal(e.to_string()))?;
            std::fs::write(&path, toml).map_err(|e| to_status(DaemonError::Io(e)))?;
            *self.state.global_config.write().await = config.clone();
            // Apply to the shared ACL store immediately and broadcast the
            // change to subscribers.
            if let Ok(mut acl) = self.state.acl_store.write() {
                *acl = config.acl.clone();
            }
            let _ = self.state.config_tx.send(config.clone());
            record_global_audit(
                &self.state,
                &subject,
                "config.set",
                serde_json::json!({ "scope": "global" }),
            );
        } else {
            // Persist to the workspace config file and update the workspace.
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            let path =
                ws.root().join(crate::workspace::METADATA_DIR).join(crate::config::CONFIG_FILE);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| to_status(DaemonError::Io(e)))?;
            }
            let toml = toml::to_string(&config).map_err(|e| Status::internal(e.to_string()))?;
            std::fs::write(&path, toml).map_err(|e| to_status(DaemonError::Io(e)))?;
            // Reload the merged config for the workspace.
            let global_config_path =
                self.state.workspaces.global_config_path().map_err(to_status)?;
            let merged = crate::config::load_merged_config(&global_config_path, ws.root())
                .map_err(to_status)?;
            *ws.config.write().await = merged;
            let writer = AuditWriter::new(ws.db.clone());
            let _ =
                writer.record(&subject, "config.set", serde_json::json!({ "scope": "workspace" }));
        }
        Ok(Response::new(Empty {}))
    }

    async fn create_snapshot(
        &self,
        request: Request<CreateSnapshotRequest>,
    ) -> Result<Response<SnapshotInfo>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let snapshot = ws
            .version_manager
            .create_snapshot_with(
                &req.description,
                if req.alias.is_empty() {
                    None
                } else {
                    Some(req.alias.as_str())
                },
            )
            .map_err(to_status)?;
        Ok(Response::new(SnapshotInfo {
            id: snapshot.id.to_string(),
            description: snapshot.description,
            created_at: snapshot.created_at as i64,
            alias: snapshot.alias.unwrap_or_default(),
        }))
    }

    async fn list_snapshots(
        &self,
        request: Request<ListSnapshotsRequest>,
    ) -> Result<Response<SnapshotList>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let snapshots = ws.version_manager.list_snapshots().map_err(to_status)?;
        let infos = snapshots
            .into_iter()
            .map(|s| SnapshotInfo {
                id: s.id.to_string(),
                description: s.description,
                created_at: s.created_at as i64,
                alias: s.alias.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(SnapshotList {
            snapshots: infos,
        }))
    }

    async fn rollback(&self, request: Request<RollbackRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        if !req.alias.is_empty() {
            ws.version_manager.rollback_by_alias(&req.alias).map_err(to_status)?;
        } else {
            let id = uuid::Uuid::parse_str(&req.snapshot_id)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
            ws.version_manager.rollback(id).map_err(to_status)?;
        }
        Ok(Response::new(Empty {}))
    }

    async fn list_executions(
        &self,
        request: Request<ListExecutionsRequest>,
    ) -> Result<Response<ExecutionList>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let checkpoints = DbCheckpointSink::list(&ws.db).map_err(to_status)?;
        let running = self.state.running.read().await;
        let active = running.contains_key(&ws.root);
        let executions = checkpoints
            .into_iter()
            .map(|cp| {
                let status = if cp.status == RunStatus::Running && !active {
                    "Suspended".to_string()
                } else {
                    status_str(cp.status)
                };
                ExecutionInfo {
                    run_id: cp.run_id.to_string(),
                    blueprint_id: cp.blueprint_id.to_string(),
                    status,
                    started_at: cp.started_at as i64,
                    updated_at: cp.updated_at as i64,
                    executed_nodes: cp.executed.len() as i32,
                    data_json: serde_json::to_string(&cp).unwrap_or_default(),
                }
            })
            .collect();
        Ok(Response::new(ExecutionList {
            executions,
        }))
    }

    type ContinueExecutionStream =
        tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>;

    async fn continue_execution(
        &self,
        request: Request<ContinueExecutionRequest>,
    ) -> Result<Response<Self::ContinueExecutionStream>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let run_id = uuid::Uuid::parse_str(&req.run_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let checkpoint = DbCheckpointSink::load(&ws.db, run_id)
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("execution not found"))?;
        if !checkpoint.status.resumable() {
            return Err(Status::failed_precondition("execution is not resumable"));
        }
        let data = ws
            .db
            .get(crate::persistence::cf::BLUEPRINTS, checkpoint.blueprint_id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        let ws_key = ws.root().to_path_buf();

        let interrupt_bus = InterruptBus::new();
        let pause_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };

        let stream = spawn_execution(
            &self.state,
            ws_key,
            ws.db.clone(),
            ws.config.clone(),
            ws.root().to_path_buf(),
            self.state.registry.clone(),
            self.state.llm_factory.clone(),
            AuditWriter::new(ws.db.clone()),
            subject,
            Arc::new(DbCheckpointSink::new(ws.db.clone(), run_id)),
            blueprint,
            Some(checkpoint),
            interrupt_bus,
            pause_flag,
            cancel_flag,
            ws.lsp_manager.clone(),
            addon_fragments,
        )
        .await?;

        Ok(Response::new(stream))
    }

    async fn get_file_history(
        &self,
        request: Request<GetFileHistoryRequest>,
    ) -> Result<Response<FileHistory>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let history = ws.version_manager.file_history(&req.path).map_err(to_status)?;
        let entries = history
            .into_iter()
            .map(|entry| FileHistoryEntry {
                snapshot_id: entry.snapshot.id.to_string(),
                description: entry.snapshot.description,
                created_at: entry.snapshot.created_at as i64,
                status: match entry.status {
                    crate::versioning::FileChangeStatus::Added => "Added".to_string(),
                    crate::versioning::FileChangeStatus::Modified => "Modified".to_string(),
                    crate::versioning::FileChangeStatus::Deleted => "Deleted".to_string(),
                    crate::versioning::FileChangeStatus::Unchanged => "Unchanged".to_string(),
                },
                content_hash: entry.hash.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(FileHistory {
            entries,
        }))
    }

    async fn list_audit_log(
        &self,
        request: Request<ListAuditLogRequest>,
    ) -> Result<Response<AuditLogList>, Status> {
        let req = request.into_inner();
        let entries = if req.workspace_path.is_empty() {
            let writer = self
                .state
                .global_audit
                .clone()
                .ok_or_else(|| Status::not_found("global audit is not enabled"))?;
            writer.list().map_err(to_status)?
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            AuditWriter::new(ws.db.clone()).list().map_err(to_status)?
        };
        let entries = entries
            .into_iter()
            .map(|entry| ProtoAuditEntry {
                timestamp: entry.timestamp as i64,
                user_id: entry.user_id,
                operation: entry.operation,
                detail_json: entry.detail.to_string(),
            })
            .collect();
        Ok(Response::new(AuditLogList {
            entries,
        }))
    }

    async fn respond_approval(
        &self,
        request: Request<ApprovalDecisionRequest>,
    ) -> Result<Response<Empty>, Status> {
        use crate::sandbox::approval::{Scope, parse_decision};
        use crate::sandbox::grant::GrantStore;

        let req = request.into_inner();
        let Some((decision, scope)) = parse_decision(&req.decision) else {
            return Err(Status::invalid_argument(format!("invalid decision '{}'", req.decision)));
        };
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let broker = self
            .approval_broker_for(ws.root())
            .await
            .ok_or_else(|| Status::not_found("no running execution or chat"))?;
        // Run-scoped grants are recorded by the awaiting authorization itself;
        // this call only needs to deliver the decision for those.
        let answered = broker.respond(&req.request_id, decision);

        let Some(answered) = answered else {
            return Err(Status::failed_precondition(
                "approval request already answered or unknown",
            ));
        };

        if matches!(scope, Scope::Workspace | Scope::Global) {
            let grants = GrantStore::new(Some(ws.db.clone()), self.state.global_db.clone());
            grants
                .store(scope, answered.command_hash, decision, &answered.command)
                .map_err(to_status)?;
        }
        Ok(Response::new(Empty {}))
    }

    async fn list_node_kinds(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<NodeKindList>, Status> {
        let mut kinds = self.state.registry.node_kinds();
        kinds.sort();
        Ok(Response::new(NodeKindList {
            kinds,
        }))
    }

    async fn save_function(
        &self,
        request: Request<SaveFunctionRequest>,
    ) -> Result<Response<SaveFunctionResponse>, Status> {
        let req = request.into_inner();
        let info = req.info.ok_or_else(|| Status::invalid_argument("info is required"))?;
        let body_proto = req.body.ok_or_else(|| Status::invalid_argument("body is required"))?;
        let body = proto_to_blueprint(&body_proto).map_err(to_status)?;
        let mut entry = proto_to_function(&info, body).map_err(to_status)?;
        crate::registry::library::validate(&entry).map_err(Status::invalid_argument)?;
        entry.signature = FunctionEntry::derive_signature(&entry.body)
            .map_err(Status::invalid_argument)?;

        if req.workspace_path.is_empty() {
            let db = self
                .state
                .global_db
                .clone()
                .ok_or_else(|| Status::unavailable("global database is not enabled"))?;
            entry.source = FunctionSource::Global;
            crate::registry::library::save(&db, &entry).map_err(to_status)?;
            self.state.registry.register_function(entry.clone());
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            entry.source = FunctionSource::Workspace;
            crate::registry::library::save(&ws.db, &entry).map_err(to_status)?;
            self.state.registry.register_function(entry.clone());
        }
        Ok(Response::new(SaveFunctionResponse {
            info: Some(function_to_proto(&entry)),
        }))
    }

    async fn list_functions(
        &self,
        request: Request<ListFunctionsRequest>,
    ) -> Result<Response<FunctionList>, Status> {
        let req = request.into_inner();
        let functions = self.state.registry.functions();
        let filtered: Vec<ProtoFunctionInfo> = functions
            .into_iter()
            .filter(|f| {
                if req.workspace_path.is_empty() {
                    f.source != FunctionSource::Workspace
                } else {
                    f.source == FunctionSource::Workspace || f.source == FunctionSource::Builtin
                }
            })
            .map(|f| function_to_proto(&f))
            .collect();
        Ok(Response::new(FunctionList {
            functions: filtered,
        }))
    }

    async fn load_function(
        &self,
        request: Request<LoadFunctionRequest>,
    ) -> Result<Response<LoadFunctionResponse>, Status> {
        let req = request.into_inner();
        let entry = if req.workspace_path.is_empty() {
            self.state
                .registry
                .function(&req.name)
                .filter(|f| f.source != FunctionSource::Workspace)
        } else {
            self.state.registry.function(&req.name)
        }
        .ok_or_else(|| Status::not_found(format!("function '{}' not found", req.name)))?;
        Ok(Response::new(LoadFunctionResponse {
            info: Some(function_to_proto(&entry)),
            body: Some(blueprint_to_proto(&entry.body)),
        }))
    }

    async fn delete_function(
        &self,
        request: Request<DeleteFunctionRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        if req.workspace_path.is_empty() {
            let db = self
                .state
                .global_db
                .clone()
                .ok_or_else(|| Status::unavailable("global database is not enabled"))?;
            crate::registry::library::delete(&db, &req.name).map_err(to_status)?;
            if let Some(entry) = self.state.registry.function(&req.name)
                && entry.source == FunctionSource::Global
            {
                self.state.registry.unregister_function(&req.name);
            }
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            crate::registry::library::delete(&ws.db, &req.name).map_err(to_status)?;
            if let Some(entry) = self.state.registry.function(&req.name)
                && entry.source == FunctionSource::Workspace
            {
                self.state.registry.unregister_function(&req.name);
            }
        }
        Ok(Response::new(Empty {}))
    }

    async fn compile_dsl(
        &self,
        request: Request<CompileDslRequest>,
    ) -> Result<Response<Blueprint>, Status> {
        let source = request.into_inner().source;
        let blueprint = metteur_shared::dsl::compile(&source)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        Ok(Response::new(blueprint_to_proto(&blueprint)))
    }

    async fn decompile_blueprint(
        &self,
        request: Request<DecompileBlueprintRequest>,
    ) -> Result<Response<DecompileDslResponse>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let id = uuid::Uuid::parse_str(&req.blueprint_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let data = ws
            .db
            .get(crate::persistence::cf::BLUEPRINTS, id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(DecompileDslResponse {
            source: metteur_shared::dsl::decompile(&blueprint),
        }))
    }

    async fn get_execution_usage(
        &self,
        request: Request<GetExecutionUsageRequest>,
    ) -> Result<Response<UsageSummary>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let config = ws.config.read().await;
        let summary =
            crate::billing::run_usage(&ws.db, &config.billing, &req.run_id).map_err(to_status)?;
        drop(config);
        Ok(Response::new(UsageSummary {
            currency: summary.currency,
            total_cost_micros: summary.total_cost_micros as u64,
            models: summary
                .models
                .into_iter()
                .map(|m| proto::ModelUsage {
                    model: m.model,
                    calls: m.calls,
                    input_tokens: m.input_tokens,
                    output_tokens: m.output_tokens,
                    reasoning_tokens: m.reasoning_tokens,
                    cost_micros: m.cost_micros,
                })
                .collect(),
        }))
    }

    async fn list_mcp_servers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<McpServerList>, Status> {
        let servers = match &self.state.mcp_host {
            Some(host) => host
                .statuses()
                .into_iter()
                .map(|status| McpServerInfo {
                    name: status.alias,
                    status: match status.state {
                        crate::mcp::StatusKind::Connected => "Connected".to_string(),
                        crate::mcp::StatusKind::Disabled => "Disabled".to_string(),
                        crate::mcp::StatusKind::Failed => "Failed".to_string(),
                    },
                    tool_count: status.tool_count,
                    error: status.error,
                })
                .collect(),
            None => Vec::new(),
        };
        Ok(Response::new(McpServerList {
            servers,
        }))
    }

    async fn install_addon(
        &self,
        request: Request<InstallAddonRequest>,
    ) -> Result<Response<AddonInfo>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        let info = host
            .install(
                std::path::Path::new(&req.package_path),
                workspace.as_deref(),
                &req.granted_permissions,
            )
            .await
            .map_err(to_status)?;
        Ok(Response::new(addon_info_to_proto(info)))
    }

    async fn list_addons(
        &self,
        _request: Request<ListAddonsRequest>,
    ) -> Result<Response<AddonList>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let roots: Vec<PathBuf> =
            self.state.workspaces.list().await.iter().map(|ws| ws.root().to_path_buf()).collect();
        let addons = host.list(&roots).await.into_iter().map(addon_info_to_proto).collect();
        Ok(Response::new(AddonList {
            addons,
        }))
    }

    async fn uninstall_addon(
        &self,
        request: Request<UninstallAddonRequest>,
    ) -> Result<Response<Empty>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        host.uninstall(&req.id, workspace.as_deref()).await.map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn set_addon_enabled(
        &self,
        request: Request<SetAddonEnabledRequest>,
    ) -> Result<Response<Empty>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        host.set_enabled(&req.id, req.enabled, workspace.as_deref()).await.map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn list_files(
        &self,
        request: Request<ListFilesRequest>,
    ) -> Result<Response<FileList>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let dir = resolve_ws_path(ws.root(), &req.dir)?;
        let meta = std::fs::metadata(&dir).map_err(io_status)?;
        if !meta.is_dir() {
            return Err(Status::invalid_argument("not a directory"));
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(io_status)? {
            let entry = entry.map_err(io_status)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let rel = entry
                .path()
                .strip_prefix(ws.root())
                .map_err(|_| Status::internal("entry outside workspace"))?
                .to_string_lossy()
                .to_string();
            // The workspace metadata directory stays invisible to clients.
            if std::path::Path::new(&rel).starts_with(crate::workspace::METADATA_DIR) {
                continue;
            }
            let is_dir = entry.file_type().map_err(io_status)?.is_dir();
            entries.push(FileEntry { name, path: rel, is_dir });
        }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(Response::new(FileList { entries }))
    }

    async fn read_file(
        &self,
        request: Request<ReadFileRequest>,
    ) -> Result<Response<ReadFileResponse>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let path = resolve_ws_path(ws.root(), &req.path)?;
        let bytes = std::fs::read(&path).map_err(io_status)?;
        let content = String::from_utf8(bytes)
            .map_err(|_| Status::invalid_argument("file is not valid utf-8"))?;
        Ok(Response::new(ReadFileResponse { content }))
    }

    async fn write_file(
        &self,
        request: Request<WriteFileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let path = resolve_ws_path(ws.root(), &req.path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_status)?;
        }
        std::fs::write(&path, req.content.as_bytes()).map_err(io_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn stat_file(
        &self,
        request: Request<StatFileRequest>,
    ) -> Result<Response<FileInfo>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let path = resolve_ws_path(ws.root(), &req.path)?;
        let meta = std::fs::metadata(&path).map_err(io_status)?;
        Ok(Response::new(FileInfo {
            path: req.path,
            is_dir: meta.is_dir(),
            len: meta.len() as i64,
        }))
    }

    async fn create_dir(
        &self,
        request: Request<CreateDirRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let path = resolve_ws_path(ws.root(), &req.path)?;
        std::fs::create_dir_all(&path).map_err(io_status)?;
        Ok(Response::new(Empty {}))
    }

    async fn remove_file(
        &self,
        request: Request<RemoveFileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let path = resolve_ws_path(ws.root(), &req.path)?;
        let meta = std::fs::symlink_metadata(&path).map_err(io_status)?;
        if meta.is_dir() {
            std::fs::remove_dir_all(&path).map_err(io_status)?;
        } else {
            std::fs::remove_file(&path).map_err(io_status)?;
        }
        Ok(Response::new(Empty {}))
    }

    async fn rename_file(
        &self,
        request: Request<RenameFileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let from = resolve_ws_path(ws.root(), &req.from)?;
        let to = resolve_ws_path(ws.root(), &req.to)?;
        std::fs::rename(&from, &to).map_err(io_status)?;
        Ok(Response::new(Empty {}))
    }

    type WatchWorkspaceStream =
        tokio_stream::wrappers::ReceiverStream<Result<WatchEvent, Status>>;

    /// Streams live file changes (created / modified / removed) for a workspace.
    ///
    /// The stream stays open until the client disconnects or the workspace is
    /// closed; events are dropped with no back-pressure when the client lags.
    async fn watch_workspace(
        &self,
        request: Request<WatchWorkspaceRequest>,
    ) -> Result<Response<Self::WatchWorkspaceStream>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let watcher = ws
            .watcher()
            .ok_or_else(|| Status::unavailable("workspace file watcher unavailable"))?;
        let mut rx = watcher.subscribe();
        let root = ws.root().to_path_buf();
        let (tx, rx_stream) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let rel = ev.path.strip_prefix(&root).unwrap_or(&ev.path);
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let kind = match ev.kind {
                            crate::versioning::watcher::WatchKind::Created => "created",
                            crate::versioning::watcher::WatchKind::Modified => "modified",
                            crate::versioning::watcher::WatchKind::Removed => "removed",
                        };
                        if tx
                            .send(Ok(WatchEvent {
                                path: rel_str,
                                kind: kind.to_string(),
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    // Backlog overflow: forward the newest events only.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx_stream)))
    }

    type SendChatStream =
        tokio_stream::wrappers::UnboundedReceiverStream<Result<ChatEvent, Status>>;

    async fn send_chat(
        &self,
        request: Request<SendChatRequest>,
    ) -> Result<Response<Self::SendChatStream>, Status> {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let run_id = uuid::Uuid::new_v4();

        // A workspace hosts either one execution or one chat at a time.
        {
            let running = self.state.running.read().await;
            if running.contains_key(&ws_key) {
                return Err(Status::failed_precondition("workspace has a running execution"));
            }
        }
        let mut chats = self.state.chats.write().await;
        if chats.contains_key(&ws_key) {
            return Err(Status::failed_precondition("workspace already has an active chat"));
        }
        let interrupt_bus = InterruptBus::new();
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let broker = Arc::new(ApprovalBroker::new());
        chats.insert(
            ws_key.clone(),
            ChatRun {
                interrupt_bus: Some(interrupt_bus.clone()),
                cancel_requested: cancel_flag.clone(),
                approvals: Some(broker.clone()),
            },
        );
        drop(chats);

        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatEvent, Status>>();
        let state = self.state.clone();
        let registry = self.state.registry.clone();
        let llm_factory = self.state.llm_factory.clone();
        let root = ws.root().to_path_buf();
        let ws_db = ws.db.clone();
        let ws_config = ws.config.clone();
        let lsp = ws.lsp_manager.clone();
        let user = subject.clone();
        let message = req.message.clone();
        let history_json = req.history_json.clone();
        let options_json = req.options_json.clone();

        tokio::spawn(async move {
            let mut ctx = ExecutionContext::new(registry, llm_factory, root.clone())
                .with_run(run_id, chrono::Utc::now().timestamp_millis() as u64)
                .with_user(user)
                .with_config(ws_config)
                .with_audit(AuditWriter::new(ws_db.clone()));
            ctx.interrupts = Some(interrupt_bus);
            ctx.cancel_requested = cancel_flag.clone();
            ctx.approvals = Some(broker);
            ctx.workspace_db = Some(ws_db.clone());
            ctx.lsp = lsp;
            if let Some(global_db) = state.global_db.clone() {
                ctx.global_db = Some(global_db);
            }

            // Compose the context: addon fragments, prior history, then the turn.
            let mut context = ContextManager {
                system_fragments: addon_fragments.clone(),
                ..ContextManager::default()
            };
            ctx.addon_fragments = addon_fragments;
            history_messages(&history_json, &mut context.messages);
            context.push_message(Message::text(Role::User, message));

            let opts = chat_options(&options_json);
            // Clone the sender so the closure owns its half; the original stays
            // available for the terminal `done`/`error` event.
            let stream_tx = event_tx.clone();
            let mut on_event = move |ev: ReactEvent| {
                let event = match ev {
                    ReactEvent::Assistant { text } => ChatEvent {
                        kind: "assistant".to_string(),
                        content: text,
                        detail_json: String::new(),
                    },
                    ReactEvent::Tool { name, content } => ChatEvent {
                        kind: "tool".to_string(),
                        content,
                        detail_json: serde_json::json!({ "name": name }).to_string(),
                    },
                };
                if stream_tx.send(Ok(event)).is_err() {
                    // The reader went away; stop the loop at the next iteration.
                    cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            };
            let outcome = run_react_streaming(&mut ctx, context, &opts, &mut on_event).await;
            let terminal = match outcome {
                Ok(outcome) => Ok(ChatEvent {
                    kind: "done".to_string(),
                    content: String::new(),
                    detail_json: serde_json::json!({
                        "usage": {
                            "input_tokens": outcome.usage.input_tokens,
                            "output_tokens": outcome.usage.output_tokens,
                            "total_tokens": outcome.usage.total_tokens,
                        }
                    })
                    .to_string(),
                }),
                Err(err) => Ok(ChatEvent {
                    kind: "error".to_string(),
                    content: err.to_string(),
                    detail_json: String::new(),
                }),
            };
            let _ = event_tx.send(terminal);
            state.chats.write().await.remove(&ws_key);
        });

        Ok(Response::new(tokio_stream::wrappers::UnboundedReceiverStream::new(event_rx)))
    }

    async fn abort_chat(
        &self,
        request: Request<AbortChatRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let chats = self.state.chats.read().await;
        let entry = chats.get(&ws_key).ok_or_else(|| Status::not_found("no active chat"))?;
        entry.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(bus) = &entry.interrupt_bus {
            bus.send(Interrupt {
                priority: InterruptPriority::Emergency,
                message: "Aborted by user.".to_string(),
            });
        }
        Ok(Response::new(Empty {}))
    }
}

/// Converts a proto blueprint into the shared model.
fn proto_to_blueprint(proto: &Blueprint) -> Result<metteur_shared::Blueprint, DaemonError> {
    let id =
        uuid::Uuid::parse_str(&proto.id).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    let entry_node_id = uuid::Uuid::parse_str(&proto.entry_node_id)
        .map_err(|e| DaemonError::Serialization(e.to_string()))?;

    let nodes = proto.nodes.iter().map(proto_to_node).collect::<Result<Vec<_>, _>>()?;

    let edges = proto.edges.iter().map(proto_to_edge).collect::<Result<Vec<_>, _>>()?;

    Ok(metteur_shared::Blueprint {
        id,
        name: proto.name.clone(),
        nodes,
        edges,
        entry_node_id,
    })
}

/// Converts a proto node into the shared model.
fn proto_to_node(proto: &proto::Node) -> Result<metteur_shared::Node, DaemonError> {
    let node_type = match proto.node_type.as_str() {
        "Event" => metteur_shared::NodeType::Event,
        "Function" => metteur_shared::NodeType::Function,
        "Pure" => metteur_shared::NodeType::Pure,
        "Control" => metteur_shared::NodeType::Control,
        _ => metteur_shared::NodeType::Function,
    };
    let pins = proto.pins.iter().map(proto_to_pin).collect::<Result<Vec<_>, _>>()?;
    Ok(metteur_shared::Node {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        node_type,
        kind: proto.kind.clone(),
        position: (proto.pos_x, proto.pos_y),
        pins,
        data: serde_json::from_str(&proto.data_json).unwrap_or(serde_json::Value::Null),
    })
}

/// Converts a proto pin into the shared model.
fn proto_to_pin(proto: &proto::Pin) -> Result<metteur_shared::Pin, DaemonError> {
    Ok(metteur_shared::Pin {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        name: proto.name.clone(),
        pin_type: match proto.pin_type.as_str() {
            "ExecInput" => metteur_shared::PinType::ExecInput,
            "ExecOutput" => metteur_shared::PinType::ExecOutput,
            "DataInput" => metteur_shared::PinType::DataInput,
            _ => metteur_shared::PinType::DataOutput,
        },
        data_type: match proto.data_type.as_str() {
            "Bool" => metteur_shared::DataType::Bool,
            "Int" => metteur_shared::DataType::Int,
            "Float" => metteur_shared::DataType::Float,
            "String" => metteur_shared::DataType::String,
            "List" => metteur_shared::DataType::List,
            "Json" => metteur_shared::DataType::Json,
            _ => metteur_shared::DataType::Void,
        },
    })
}

/// Converts a proto edge into the shared model.
fn proto_to_edge(proto: &proto::Edge) -> Result<metteur_shared::Edge, DaemonError> {
    Ok(metteur_shared::Edge {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        source_node: uuid::Uuid::parse_str(&proto.source_node)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        source_pin: uuid::Uuid::parse_str(&proto.source_pin)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        target_node: uuid::Uuid::parse_str(&proto.target_node)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        target_pin: uuid::Uuid::parse_str(&proto.target_pin)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
    })
}

/// Converts a shared blueprint into the proto model.
fn blueprint_to_proto(blueprint: &metteur_shared::Blueprint) -> Blueprint {
    Blueprint {
        id: blueprint.id.to_string(),
        name: blueprint.name.clone(),
        nodes: blueprint
            .nodes
            .iter()
            .map(|n| proto::Node {
                id: n.id.to_string(),
                node_type: match n.node_type {
                    metteur_shared::NodeType::Event => "Event".to_string(),
                    metteur_shared::NodeType::Function => "Function".to_string(),
                    metteur_shared::NodeType::Pure => "Pure".to_string(),
                    metteur_shared::NodeType::Control => "Control".to_string(),
                },
                kind: n.kind.clone(),
                pos_x: n.position.0,
                pos_y: n.position.1,
                pins: n
                    .pins
                    .iter()
                    .map(|p| proto::Pin {
                        id: p.id.to_string(),
                        name: p.name.clone(),
                        pin_type: match p.pin_type {
                            metteur_shared::PinType::ExecInput => "ExecInput".to_string(),
                            metteur_shared::PinType::ExecOutput => "ExecOutput".to_string(),
                            metteur_shared::PinType::DataInput => "DataInput".to_string(),
                            metteur_shared::PinType::DataOutput => "DataOutput".to_string(),
                        },
                        data_type: match p.data_type {
                            metteur_shared::DataType::Void => "Void".to_string(),
                            metteur_shared::DataType::Bool => "Bool".to_string(),
                            metteur_shared::DataType::Int => "Int".to_string(),
                            metteur_shared::DataType::Float => "Float".to_string(),
                            metteur_shared::DataType::String => "String".to_string(),
                            metteur_shared::DataType::List => "List".to_string(),
                            metteur_shared::DataType::Json => "Json".to_string(),
                        },
                    })
                    .collect(),
                data_json: serde_json::to_string(&n.data).unwrap_or_else(|_| "null".to_string()),
            })
            .collect(),
        edges: blueprint
            .edges
            .iter()
            .map(|e| proto::Edge {
                id: e.id.to_string(),
                source_node: e.source_node.to_string(),
                source_pin: e.source_pin.to_string(),
                target_node: e.target_node.to_string(),
                target_pin: e.target_pin.to_string(),
            })
            .collect(),
        entry_node_id: blueprint.entry_node_id.to_string(),
    }
}

/// Converts a shared function entry into the proto model.
fn function_to_proto(entry: &FunctionEntry) -> ProtoFunctionInfo {
    let source = match entry.source {
        FunctionSource::Builtin => "builtin",
        FunctionSource::Global => "global",
        FunctionSource::Workspace => "workspace",
    };
    ProtoFunctionInfo {
        id: entry.id.to_string(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        inputs: entry.signature.inputs.iter().map(fn_pin_to_proto).collect(),
        outputs: entry.signature.outputs.iter().map(fn_pin_to_proto).collect(),
        source: source.to_string(),
        updated_at: 0,
    }
}

/// Converts a proto signature pin into the shared model.
fn proto_to_fn_pin(pin: &ProtoFnPin) -> Result<FnPin, DaemonError> {
    Ok(FnPin {
        name: pin.name.clone(),
        data_type: match pin.data_type.as_str() {
            "Bool" => metteur_shared::DataType::Bool,
            "Int" => metteur_shared::DataType::Int,
            "Float" => metteur_shared::DataType::Float,
            "String" => metteur_shared::DataType::String,
            "List" => metteur_shared::DataType::List,
            "Json" => metteur_shared::DataType::Json,
            _ => metteur_shared::DataType::Void,
        },
        description: if pin.description.is_empty() { None } else { Some(pin.description.clone()) },
    })
}

/// Converts a shared signature pin into the proto model.
fn fn_pin_to_proto(pin: &FnPin) -> ProtoFnPin {
    ProtoFnPin {
        name: pin.name.clone(),
        data_type: match pin.data_type {
            metteur_shared::DataType::Void => "Void".to_string(),
            metteur_shared::DataType::Bool => "Bool".to_string(),
            metteur_shared::DataType::Int => "Int".to_string(),
            metteur_shared::DataType::Float => "Float".to_string(),
            metteur_shared::DataType::String => "String".to_string(),
            metteur_shared::DataType::List => "List".to_string(),
            metteur_shared::DataType::Json => "Json".to_string(),
        },
        description: pin.description.clone().unwrap_or_default(),
    }
}

/// Converts a proto function request into a shared entry.
fn proto_to_function(
    info: &ProtoFunctionInfo,
    body: metteur_shared::Blueprint,
) -> Result<FunctionEntry, DaemonError> {
    let inputs = info
        .inputs
        .iter()
        .map(proto_to_fn_pin)
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = info
        .outputs
        .iter()
        .map(proto_to_fn_pin)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FunctionEntry {
        id: uuid::Uuid::parse_str(&info.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        name: info.name.clone(),
        description: info.description.clone(),
        signature: metteur_shared::model::function::FunctionSignature { inputs, outputs },
        body,
        source: FunctionSource::Workspace,
    })
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
