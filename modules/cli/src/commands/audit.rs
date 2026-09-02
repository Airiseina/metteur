//! Audit log command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::ListAuditLogRequest;
use tonic::transport::Channel;

use crate::print;
use super::*;

/// Handles `audit ws`.
pub(crate) async fn handle_audit_ws(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
) -> anyhow::Result<Outcome> {
    let ws = require_ws(state)?;
    let list = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: ws,
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(print::audit(&list)))
}

/// Handles `audit global`.
pub(crate) async fn handle_audit_global(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: String::new(),
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(print::audit(&list)))
}