//! Function registry and registry-listing command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{
    DeleteFunctionRequest, Empty, FunctionInfo, ListFunctionsRequest, LoadFunctionRequest,
    SaveFunctionRequest,
};
use tonic::transport::Channel;

use crate::print;
use super::blueprint::{from_json, to_json};
use super::*;

/// Reads the workspace path for a function command, when scoped to `ws`.
fn func_ws(state: &SessionState, workspace: bool) -> anyhow::Result<String> {
    if workspace {
        Ok(require_ws(state)?)
    } else {
        Ok(String::new())
    }
}

/// Handles `func save <name> <file.json> [ws|global]`.
pub(crate) async fn handle_func_save(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    name: String,
    file: String,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let workspace_path = func_ws(state, workspace)?;
    let text = std::fs::read_to_string(&file)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file))?;
    let body = from_json(&text)?;
    let info = FunctionInfo {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.clone(),
        description: String::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        source: String::new(),
        updated_at: 0,
    };
    let resp = client
        .save_function(SaveFunctionRequest {
            workspace_path,
            info: Some(info),
            body: Some(body),
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(Outcome::Printed(format!(
        "function saved: {}",
        resp.info.map(|f| f.name).unwrap_or(name)
    )))
}

/// Handles `func list [ws|global]`.
pub(crate) async fn handle_func_list(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let workspace_path = func_ws(state, workspace)?;
    let list = client
        .list_functions(ListFunctionsRequest {
            workspace_path,
        })
        .await
        .map_err(status)?
        .into_inner();
    let mut lines = Vec::new();
    for f in list.functions {
        let inputs = f
            .inputs
            .iter()
            .map(|p| format!("{}:{}", p.name, p.data_type))
            .collect::<Vec<_>>()
            .join(", ");
        let outputs = f
            .outputs
            .iter()
            .map(|p| format!("{}:{}", p.name, p.data_type))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "{} [{}] ({}) -> ({})\n  {}",
            f.name, f.source, inputs, outputs, f.description
        ));
    }
    Ok(Outcome::Printed(if lines.is_empty() {
        "no functions registered".to_string()
    } else {
        lines.join("\n")
    }))
}

/// Handles `func load <name> [ws|global]`.
pub(crate) async fn handle_func_load(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    name: String,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let workspace_path = func_ws(state, workspace)?;
    let loaded = client
        .load_function(LoadFunctionRequest {
            workspace_path,
            name: name.clone(),
        })
        .await
        .map_err(status)?
        .into_inner();
    let body = loaded
        .body
        .ok_or_else(|| anyhow::anyhow!("function has no body"))?;
    Ok(Outcome::Printed(to_json(&body)?))
}

/// Handles `func rm <name> [ws|global]`.
pub(crate) async fn handle_func_rm(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    name: String,
    workspace: bool,
) -> anyhow::Result<Outcome> {
    let workspace_path = func_ws(state, workspace)?;
    client
        .delete_function(DeleteFunctionRequest {
            workspace_path,
            name: name.clone(),
        })
        .await
        .map_err(status)?;
    Ok(Outcome::Printed(format!("function deleted: {name}")))
}

/// Handles `tools`: lists registered tools.
pub(crate) async fn handle_tools(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client.list_tools(Empty {}).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::tools(&list)))
}

/// Handles `nodes`: lists available node kinds.
pub(crate) async fn handle_nodes(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client.list_node_kinds(Empty {}).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::node_kinds(&list.kinds)))
}