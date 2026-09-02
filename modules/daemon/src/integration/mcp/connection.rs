//! MCP server connections backed by the rmcp SDK.

use std::collections::HashMap;

use async_trait::async_trait;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, PaginatedRequestParams, ReadResourceRequestParams};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{DaemonError, DaemonResult};

/// A single tool exposed by a remote MCP server.
#[derive(Debug, Clone)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    /// JSON Schema of the tool arguments.
    pub schema: Value,
}

/// A resource exposed by a remote MCP server.
#[derive(Debug, Clone)]
pub struct RemoteResource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

/// Transport-agnostic operations on one MCP server connection.
///
/// The trait isolates the rmcp types so hosts and tests can substitute fake
/// implementations.
#[async_trait]
pub trait McpConnectionOps: Send + Sync {
    async fn list_tools(&self) -> DaemonResult<Vec<RemoteTool>>;
    async fn call_tool(&self, name: &str, args: Value) -> DaemonResult<String>;
    async fn list_resources(&self) -> DaemonResult<Vec<RemoteResource>>;
    async fn read_resource_text(&self, uri: &str) -> DaemonResult<String>;

    /// Terminates the underlying session or process.
    ///
    /// Safe to call while other references exist: it waits for in-flight
    /// operations on this connection to finish before cancelling.
    async fn shutdown(&self);
}

/// An active rmcp client session.
///
/// The service lives inside an [`AsyncMutex`] so [`McpConnectionOps::shutdown`]
/// can take ownership for `cancel()` even while callers only hold shared
/// references to the connection.
pub struct RmcpConnection {
    service: AsyncMutex<Option<RunningService<RoleClient, ()>>>,
}

impl RmcpConnection {
    /// Clones the live service peer, failing after shutdown.
    async fn peer(&self) -> DaemonResult<Peer<RoleClient>> {
        let guard = self.service.lock().await;
        guard
            .as_ref()
            .map(|service| service.peer().clone())
            .ok_or_else(|| DaemonError::Mcp("connection already closed".to_string()))
    }

    /// Connects to an MCP server spawned as a child process (stdio).
    pub async fn connect_stdio(
        command: &[String],
        env: &HashMap<String, String>,
    ) -> DaemonResult<Self> {
        let (program, args) = command.split_first().ok_or_else(|| {
            DaemonError::Mcp("stdio transport requires a non-empty command".to_string())
        })?;
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args).envs(env);
        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| DaemonError::Mcp(format!("failed to spawn MCP server: {e}")))?;
        Self::start_stdio(transport).await
    }

    /// Connects to an MCP server over Streamable HTTP.
    pub async fn connect_http(url: &str) -> DaemonResult<Self> {
        let transport = StreamableHttpClientTransport::from_uri(url.to_string());
        Self::start_http(transport).await
    }

    async fn start_stdio(transport: TokioChildProcess) -> DaemonResult<Self> {
        let service = ()
            .serve(transport)
            .await
            .map_err(|err| DaemonError::Mcp(format!("MCP initialization failed: {err}")))?;
        Ok(Self {
            service: AsyncMutex::new(Some(service)),
        })
    }

    async fn start_http(
        transport: StreamableHttpClientTransport<reqwest::Client>,
    ) -> DaemonResult<Self> {
        let service = ()
            .serve(transport)
            .await
            .map_err(|err| DaemonError::Mcp(format!("MCP initialization failed: {err}")))?;
        Ok(Self {
            service: AsyncMutex::new(Some(service)),
        })
    }
}

#[async_trait]
impl McpConnectionOps for RmcpConnection {
    async fn list_tools(&self) -> DaemonResult<Vec<RemoteTool>> {
        let result = self
            .peer()
            .await?
            .list_tools(Some(PaginatedRequestParams::default()))
            .await
            .map_err(mcp_error)?;
        Ok(result
            .tools
            .into_iter()
            .map(|tool| RemoteTool {
                name: tool.name.to_string(),
                description: tool.description.as_deref().unwrap_or("").to_string(),
                schema: serde_json::to_value(tool.input_schema.as_ref())
                    .unwrap_or(serde_json::json!({"type": "object"})),
            })
            .collect())
    }

    async fn call_tool(&self, name: &str, args: Value) -> DaemonResult<String> {
        let arguments = match args {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        let result = self.peer().await?.call_tool(params).await.map_err(mcp_error)?;
        if result.is_error.unwrap_or(false) {
            return Err(DaemonError::Mcp(format!(
                "tool {name} returned an error: {}",
                content_to_string(&result.content)
            )));
        }
        Ok(content_to_string(&result.content))
    }

    async fn list_resources(&self) -> DaemonResult<Vec<RemoteResource>> {
        let result = self
            .peer()
            .await?
            .list_resources(Some(PaginatedRequestParams::default()))
            .await
            .map_err(mcp_error)?;
        Ok(result
            .resources
            .into_iter()
            .map(|resource| RemoteResource {
                uri: resource.uri,
                name: resource.name,
                description: resource.description.unwrap_or_default(),
                mime_type: resource.mime_type.unwrap_or_default(),
            })
            .collect())
    }

    async fn read_resource_text(&self, uri: &str) -> DaemonResult<String> {
        let result = self
            .peer()
            .await?
            .read_resource(ReadResourceRequestParams::new(uri.to_string()))
            .await
            .map_err(mcp_error)?;
        let mut text = String::new();
        for content in result.contents {
            match content {
                rmcp::model::ResourceContents::TextResourceContents {
                    text: piece,
                    ..
                } => {
                    text.push_str(&piece);
                }
                rmcp::model::ResourceContents::BlobResourceContents {
                    blob,
                    ..
                } => {
                    text.push_str(&format!("\n[binary resource: {} bytes]", blob.len()));
                }
                _ => {}
            }
        }
        Ok(text)
    }

    async fn shutdown(&self) {
        // Taking the lock waits for any in-flight operation on this
        // connection; `take()` guarantees exactly-once cancellation even if
        // shutdown itself is called twice.
        if let Some(service) = self.service.lock().await.take() {
            let _ = service.cancel().await;
        }
    }
}

/// Flattens response content into plain text (text parts joined by newlines).
fn content_to_string(content: &[rmcp::model::ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mcp_error(err: rmcp::ServiceError) -> DaemonError {
    DaemonError::Mcp(err.to_string())
}
