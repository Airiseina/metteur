//! Config read/update command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{GetConfigRequest, SetConfigRequest};
use tonic::transport::Channel;

use super::*;
use crate::print;

/// Empty path for global scope, current workspace for `ws` scope.
fn optional_ws(state: &SessionState, workspace: bool) -> anyhow::Result<String> {
    if workspace {
        require_ws(state)
    } else {
        Ok(String::new())
    }
}

/// Handles `cfg get [ws]`.
pub(crate) async fn handle_cfg_get(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let cfg = client
        .get_config(GetConfigRequest {
            workspace_path: optional_ws(state, workspace)?,
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(print::pretty_json(&cfg.config_json)))
}

/// Handles `cfg set <json> [ws]`.
pub(crate) async fn handle_cfg_set(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    json: String,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let ws = optional_ws(state, workspace)?;
    client
        .set_config(SetConfigRequest {
            workspace_path: ws,
            config_json: json,
        })
        .await
        .map_err(status)?;
    Ok(Outcome::Printed("config updated".to_string()))
}
