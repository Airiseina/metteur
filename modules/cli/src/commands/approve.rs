//! Approval command handlers.

use metteur_proto::proto::ApprovalDecisionRequest;
use metteur_proto::proto::daemon_client::DaemonClient;
use tonic::transport::Channel;

use super::*;

/// Handles `approve <request_id> <decision> [workspace|global]`.
pub(crate) async fn handle_approve(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    request_id: String,
    decision: String,
) -> anyhow::Result<Outcome> {
    client
        .respond_approval(ApprovalDecisionRequest {
            workspace_path: state.current_ws.clone().unwrap_or_default(),
            request_id: request_id.clone(),
            decision: decision.clone(),
        })
        .await
        .map_err(status)?;
    Ok(Outcome::Printed(format!("approval {request_id}: {decision}")))
}

/// Handles `approve-auto on|off`.
pub(crate) async fn handle_approve_auto(
    state: &mut SessionState,
    on: bool,
) -> anyhow::Result<Outcome> {
    state.auto_approve = on;
    Ok(Outcome::Printed(format!(
        "auto-approve {}",
        if on {
            "enabled"
        } else {
            "disabled"
        }
    )))
}
