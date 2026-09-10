//! Registry RPCs: tools, node kinds, MCP servers and addons.

use std::path::PathBuf;

use tonic::{Request, Response, Status};

use super::super::proto::{
    AddonInfo, AddonList, Empty, InstallAddonRequest, ListAddonsRequest, McpServerInfo,
    McpServerList, NodeKindList, SetAddonEnabledRequest, ToolInfo, ToolList, UninstallAddonRequest,
};
use super::*;

impl DaemonService {
    pub(crate) async fn list_tools(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ToolList>, Status> {
        let tools = self
            .state
            .registry
            .tools()
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
            })
            .collect();
        Ok(Response::new(ToolList {
            tools,
        }))
    }

    pub(crate) async fn list_node_kinds(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<NodeKindList>, Status> {
        let mut kinds = self.state.registry.node_kinds();
        kinds.sort();
        Ok(Response::new(NodeKindList {
            kinds,
        }))
    }

    pub(crate) async fn list_mcp_servers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<McpServerList>, Status> {
        let servers = match &self.state.mcp_host {
            Some(host) => host
                .statuses()
                .into_iter()
                .map(|status| McpServerInfo {
                    name: status.alias,
                    status: match status.state {
                        crate::integration::mcp::StatusKind::Connected => "Connected".to_string(),
                        crate::integration::mcp::StatusKind::Disabled => "Disabled".to_string(),
                        crate::integration::mcp::StatusKind::Failed => "Failed".to_string(),
                    },
                    tool_count: status.tool_count,
                    error: status.error,
                })
                .collect(),
            None => Vec::new(),
        };
        Ok(Response::new(McpServerList {
            servers,
        }))
    }

    pub(crate) async fn install_addon(
        &self,
        request: Request<InstallAddonRequest>,
    ) -> Result<Response<AddonInfo>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        let info = host
            .install(
                std::path::Path::new(&req.package_path),
                workspace.as_deref(),
                &req.granted_permissions,
            )
            .await
            .map_err(to_status)?;
        Ok(Response::new(addon_info_to_proto(info)))
    }

    pub(crate) async fn list_addons(
        &self,
        _request: Request<ListAddonsRequest>,
    ) -> Result<Response<AddonList>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let roots: Vec<PathBuf> =
            self.state.workspaces.list().await.iter().map(|ws| ws.root().to_path_buf()).collect();
        let addons = host.list(&roots).await.into_iter().map(addon_info_to_proto).collect();
        Ok(Response::new(AddonList {
            addons,
        }))
    }

    pub(crate) async fn uninstall_addon(
        &self,
        request: Request<UninstallAddonRequest>,
    ) -> Result<Response<Empty>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        host.uninstall(&req.id, workspace.as_deref()).await.map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn set_addon_enabled(
        &self,
        request: Request<SetAddonEnabledRequest>,
    ) -> Result<Response<Empty>, Status> {
        let Some(host) = &self.state.addon_host else {
            return Err(Status::unimplemented("addon host is not attached"));
        };
        let req = request.into_inner();
        let workspace = optional_workspace(&req.workspace_path)?;
        host.set_enabled(&req.id, req.enabled, workspace.as_deref()).await.map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }
}
