//! Configuration RPCs: get/set config and the audit log.

use std::path::PathBuf;

use metteur_shared::config::Config;
use tonic::{Request, Response, Status};

use crate::observability::audit::AuditWriter;
use crate::error::DaemonError;

use super::super::proto::{
    AuditEntry as ProtoAuditEntry, AuditLogList, Config as ProtoConfig, Empty, GetConfigRequest,
    ListAuditLogRequest, SetConfigRequest,
};
use super::super::acl::subject_from_request;
use super::*;

impl DaemonService {
    pub(crate) async fn get_config(
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

    pub(crate) async fn set_config(
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

    pub(crate) async fn list_audit_log(
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
}