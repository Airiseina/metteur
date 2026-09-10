//! MCP server listing command handler.

use metteur_proto::proto::Empty;
use metteur_proto::proto::daemon_client::DaemonClient;
use tonic::transport::Channel;

use super::*;
use crate::print;

/// Handles `mcp`: lists registered MCP servers.
pub(crate) async fn handle_mcp(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client.list_mcp_servers(Empty {}).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::mcp_servers(&list)))
}
