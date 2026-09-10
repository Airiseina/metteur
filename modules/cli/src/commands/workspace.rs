//! Workspace open/close/list command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{CloseWorkspaceRequest, Empty, OpenWorkspaceRequest};
use tonic::transport::Channel;

use super::*;
use crate::print;

/// Handles `open <path>`.
pub(crate) async fn handle_open(
    client: &mut DaemonClient<Channel>,
    state: &mut SessionState,
    path: String,
) -> anyhow::Result<Outcome> {
    Ok(Outcome::Printed(open_workspace(client, state, &path).await?))
}

/// Handles `close <path>`.
pub(crate) async fn handle_close(
    client: &mut DaemonClient<Channel>,
    state: &mut SessionState,
    path: String,
) -> anyhow::Result<Outcome> {
    client
        .close_workspace(CloseWorkspaceRequest {
            path: path.clone(),
        })
        .await
        .map_err(status)?;
    if state.current_ws.as_deref() == Some(path.as_str()) {
        state.current_ws = None;
    }
    Ok(Outcome::Printed(format!("closed workspace {path}")))
}

/// Handles `ws`: lists open workspaces.
pub(crate) async fn handle_ws(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
) -> anyhow::Result<Outcome> {
    let list = client.list_workspaces(Empty {}).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::workspaces(&list, state.current_ws.as_deref())))
}

/// Opens a workspace and records it as the current session workspace.
async fn open_workspace(
    client: &mut DaemonClient<Channel>,
    state: &mut SessionState,
    path: &str,
) -> anyhow::Result<String> {
    let info = client
        .open_workspace(OpenWorkspaceRequest {
            path: path.to_string(),
        })
        .await
        .map_err(status)?
        .into_inner();
    state.current_ws = Some(path.to_string());
    Ok(if info.locked {
        format!("opened workspace {} (locked)", info.path)
    } else {
        format!("opened workspace {}", info.path)
    })
}
