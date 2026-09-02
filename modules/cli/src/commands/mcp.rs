//! MCP server listing command handler.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::Empty;
use tonic::transport::Channel;

use crate::print;
use super::*;

/// Handles `mcp`: lists registered MCP servers.
pub(crate) async fn handle_mcp(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client.list_mcp_servers(Empty {}).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::mcp_servers(&list)))
}