//! Workspace RPCs: open, close and list.

use std::path::PathBuf;

use metteur_shared::model::function::FunctionSource;
use tonic::{Request, Response, Status};

use super::super::proto::{
    CloseWorkspaceRequest, Empty, OpenWorkspaceRequest, WorkspaceInfo, WorkspaceList,
};
use super::super::acl::subject_from_request;
use super::*;

impl DaemonService {
    pub(crate) async fn open_workspace(
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

    pub(crate) async fn close_workspace(
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

    pub(crate) async fn list_workspaces(
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
}