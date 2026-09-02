//! Sandbox RPCs: approval responses.

use std::path::PathBuf;

use tonic::{Request, Response, Status};

use super::super::proto::{ApprovalDecisionRequest, Empty};
use super::*;

impl DaemonService {
    pub(crate) async fn respond_approval(
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
}