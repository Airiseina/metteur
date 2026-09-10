//! Snapshot/rollback/file-history command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{
    CreateSnapshotRequest, GetFileHistoryRequest, ListSnapshotsRequest, RollbackRequest,
};
use tonic::transport::Channel;

use super::*;
use crate::print;

/// Handles `snap <description...> [--alias <name>]`.
pub(crate) async fn handle_snap(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    description: String,
    alias: Option<String>,
) -> anyhow::Result<Outcome> {
    let ws = require_ws(state)?;
    let info = client
        .create_snapshot(CreateSnapshotRequest {
            workspace_path: ws,
            description: description.clone(),
            alias: alias.clone().unwrap_or_default(),
        })
        .await
        .map_err(status)?
        .into_inner();
    let tag = alias.as_deref().map(|a| format!(" as {a}")).unwrap_or_default();
    Ok(Outcome::Printed(format!("snapshot {} created ({description}){tag}", info.id)))
}

/// Handles `snaps`: lists workspace snapshots.
pub(crate) async fn handle_snaps(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
) -> anyhow::Result<Outcome> {
    let ws = require_ws(state)?;
    let list = client
        .list_snapshots(ListSnapshotsRequest {
            workspace_path: ws,
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(print::snapshots(&list)))
}

/// Handles `rollback <snapshot_id|alias>`.
pub(crate) async fn handle_rollback(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    target: String,
) -> anyhow::Result<Outcome> {
    let ws = require_ws(state)?;
    // A valid UUID is treated as a snapshot id, anything else as an
    // alias so users can roll back by name.
    let is_id = target.parse::<uuid::Uuid>().is_ok();
    client
        .rollback(RollbackRequest {
            workspace_path: ws,
            snapshot_id: if is_id {
                target.clone()
            } else {
                String::new()
            },
            alias: if is_id {
                String::new()
            } else {
                target.clone()
            },
        })
        .await
        .map_err(status)?;
    Ok(Outcome::Printed(format!("rolled back to {target}")))
}

/// Handles `hist <relpath>`: shows file history across snapshots.
pub(crate) async fn handle_hist(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    path: String,
) -> anyhow::Result<Outcome> {
    let ws = require_ws(state)?;
    let history = client
        .get_file_history(GetFileHistoryRequest {
            workspace_path: ws,
            path: path.clone(),
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(print::file_history(&history, &path)))
}
