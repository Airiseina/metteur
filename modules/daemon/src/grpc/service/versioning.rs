//! Versioning RPCs: snapshots, executions and file history.

use std::path::PathBuf;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::observability::audit::AuditWriter;
use crate::execution::interrupt::InterruptBus;
use crate::execution::{DbCheckpointSink, RunStatus};

use super::super::proto::{
    self, ContinueExecutionRequest, CreateSnapshotRequest, Empty, ExecutionEvent, ExecutionInfo,
    ExecutionList, FileHistory, FileHistoryEntry, GetExecutionUsageRequest, GetFileHistoryRequest,
    ListExecutionsRequest, ListSnapshotsRequest, RollbackRequest, SnapshotInfo, SnapshotList,
    UsageSummary,
};
use super::super::acl::subject_from_request;
use super::*;

impl DaemonService {
    pub(crate) async fn create_snapshot(
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

    pub(crate) async fn list_snapshots(
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

    pub(crate) async fn rollback(&self, request: Request<RollbackRequest>) -> Result<Response<Empty>, Status> {
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

    pub(crate) async fn list_executions(
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

    pub(crate) async fn continue_execution(
        &self,
        request: Request<ContinueExecutionRequest>,
    ) -> Result<
        Response<tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>>,
        Status,
    > {
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
            .get(crate::storage::persistence::cf::BLUEPRINTS, checkpoint.blueprint_id.as_bytes())
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

    pub(crate) async fn get_file_history(
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
                    crate::storage::versioning::FileChangeStatus::Added => "Added".to_string(),
                    crate::storage::versioning::FileChangeStatus::Modified => "Modified".to_string(),
                    crate::storage::versioning::FileChangeStatus::Deleted => "Deleted".to_string(),
                    crate::storage::versioning::FileChangeStatus::Unchanged => "Unchanged".to_string(),
                },
                content_hash: entry.hash.unwrap_or_default(),
            })
            .collect();
        Ok(Response::new(FileHistory {
            entries,
        }))
    }

    pub(crate) async fn get_execution_usage(
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
            crate::llm::billing::run_usage(&ws.db, &config.billing, &req.run_id).map_err(to_status)?;
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
}