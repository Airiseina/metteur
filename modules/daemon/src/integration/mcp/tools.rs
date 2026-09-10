//! Registry tool adapters for MCP server tools and resources.

use std::sync::Arc;

use async_trait::async_trait;
use metteur_shared::Value;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::Tool;

use super::McpHost;

/// Exposes one remote MCP server tool under a registered PascalCase name.
pub struct McpServerTool {
    host: Arc<McpHost>,
    /// Alias of the configured server this tool belongs to.
    pub server_alias: String,
    /// The remote tool name on the wire.
    pub remote_name: String,
    /// The name this tool is registered under.
    pub registered_name: String,
    /// Description captured at discovery time.
    pub description: String,
    /// Argument JSON Schema captured at discovery time.
    pub parameters_schema: serde_json::Value,
}

impl McpServerTool {
    /// Creates an adapter for one discovered remote tool.
    pub fn new(
        host: Arc<McpHost>,
        server_alias: &str,
        remote: super::connection::RemoteTool,
        registered_name: String,
    ) -> Self {
        Self {
            host,
            server_alias: server_alias.to_string(),
            remote_name: remote.name,
            registered_name,
            description: remote.description,
            parameters_schema: remote.schema,
        }
    }
}

#[async_trait]
impl Tool for McpServerTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters_schema.clone()
    }

    async fn call(&self, args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let object = args
            .iter()
            .find_map(|value| match value {
                Value::Json(serde_json::Value::Object(map)) => Some(map.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let text =
            self.host.call_server_tool(&self.server_alias, &self.remote_name, object).await?;
        Ok(Value::String(text))
    }
}

/// Lists the resources of one MCP server (dynamic discovery).
pub struct ListMcpResources {
    host: Arc<McpHost>,
}

impl ListMcpResources {
    pub fn new(host: Arc<McpHost>) -> Self {
        Self {
            host,
        }
    }
}

#[async_trait]
impl Tool for ListMcpResources {
    fn name(&self) -> &str {
        "ListMcpResources"
    }

    fn description(&self) -> &str {
        "List resources exposed by a connected MCP server. \
         Pass `server` with the server alias; template resources are marked \
         and list their URI variables."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"server": {"type": "string"}},
            "required": ["server"]
        })
    }

    async fn call(&self, args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let alias = string_arg(args, "server")?;
        let resources = self.host.list_server_resources(alias).await?;
        let items: Vec<serde_json::Value> = resources
            .iter()
            .map(|resource| {
                serde_json::json!({
                    "uri": resource.uri,
                    "name": resource.name,
                    "description": resource.description,
                    "mime_type": resource.mime_type,
                })
            })
            .collect();
        Ok(Value::Json(serde_json::json!({ "resources": items })))
    }
}

/// Reads one resource from an MCP server, filling URI templates when needed.
pub struct ReadMcpResource {
    host: Arc<McpHost>,
}

impl ReadMcpResource {
    pub fn new(host: Arc<McpHost>) -> Self {
        Self {
            host,
        }
    }
}

#[async_trait]
impl Tool for ReadMcpResource {
    fn name(&self) -> &str {
        "ReadMcpResource"
    }

    fn description(&self) -> &str {
        "Read a resource from an MCP server. Template URIs containing \
         `{variable}` segments are filled from the `arguments` object."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server": {"type": "string"},
                "uri": {"type": "string"},
                "arguments": {"type": "object"}
            },
            "required": ["server", "uri"]
        })
    }

    async fn call(&self, args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let alias = string_arg(args, "server")?;
        let uri = string_arg(args, "uri")?;
        let resolved_uri = fill_template(uri, args)?;
        let text = self.host.read_server_resource(alias, &resolved_uri).await?;
        Ok(Value::String(text))
    }
}

/// Fills `{name}` placeholders in a URI template from JSON object arguments.
pub(crate) fn fill_template(template: &str, args: &[Value]) -> DaemonResult<String> {
    let map = args.iter().find_map(|value| match value {
        Value::Json(map @ serde_json::Value::Object(_)) => Some(map.clone()),
        _ => None,
    });
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}').map(|offset| start + offset).ok_or_else(|| {
            DaemonError::Execution(format!("unterminated placeholder in resource URI {template}"))
        })?;
        out.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        let replacement = map.as_ref().and_then(|map| map.get(name)).cloned().ok_or_else(|| {
            DaemonError::Execution(format!(
                "missing argument '{name}' for resource URI template {template}"
            ))
        })?;
        match replacement {
            serde_json::Value::String(value) => out.push_str(&value),
            other => out.push_str(&other.to_string()),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn string_arg<'a>(args: &'a [Value], name: &str) -> DaemonResult<&'a str> {
    args.iter()
        .find_map(|value| match value {
            Value::Json(serde_json::Value::Object(map)) => {
                map.get(name).and_then(|item| item.as_str())
            }
            _ => None,
        })
        .ok_or_else(|| DaemonError::Execution(format!("missing input {name}")))
}

/// Converts a snake_case or camelCase identifier into PascalCase.
pub(crate) fn pascal_case(input: &str) -> String {
    input
        .split(['_', '-', ' ', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// Derives the registered tool name from the server alias and remote tool
/// name, appending `2`, `3`, ... on conflicts with existing names.
///
/// The suffix is a bare digit (no separator) so generated names stay valid
/// under the registry's PascalCase rule.
///
/// Returns `(registered_name, unique)` where `unique` is true when no suffix
/// was needed.
pub(crate) fn unique_server_tool_name(
    alias: &str,
    remote_name: &str,
    taken: &std::collections::HashSet<String>,
) -> (String, bool) {
    let base = format!("{}{}", pascal_case(alias), pascal_case(remote_name));
    if !taken.contains(&base) {
        return (base, true);
    }
    for index in 2..u32::MAX {
        let candidate = format!("{base}{index}");
        if !taken.contains(&candidate) {
            return (candidate, false);
        }
    }
    unreachable!("tool name space is practically unbounded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_normalizes_separators() {
        assert_eq!(pascal_case("read_file"), "ReadFile");
        assert_eq!(pascal_case("get-weather-data"), "GetWeatherData");
        assert_eq!(pascal_case("Filesystem"), "Filesystem");
        assert_eq!(pascal_case(""), "");
    }

    #[test]
    fn server_tool_names_combine_alias_and_tool() {
        let taken = std::collections::HashSet::new();
        let (name, unique) = unique_server_tool_name("filesystem", "read_file", &taken);
        assert_eq!(name, "FilesystemReadFile");
        assert!(unique);
    }

    #[test]
    fn conflicting_names_get_numeric_suffix() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("GitStatus".to_string());
        let (name, unique) = unique_server_tool_name("git", "status", &taken);
        assert_eq!(name, "GitStatus2");
        assert!(!unique);
        // The suffixed name is free for future uniqueness checks.
        taken.insert(name);
        let (name2, _) = unique_server_tool_name("git", "status", &taken);
        assert_eq!(name2, "GitStatus3");
    }

    #[test]
    fn fills_uri_templates_from_arguments() {
        let args = vec![Value::Json(serde_json::json!({"owner": "octocat"}))];
        let uri = fill_template("mcp://gh/repos/{owner}/tree", &args).unwrap();
        assert_eq!(uri, "mcp://gh/repos/octocat/tree");
    }

    #[test]
    fn template_missing_argument_is_rejected() {
        let args = vec![Value::Json(serde_json::json!({"owner": "octocat"}))];
        assert!(fill_template("mcp://gh/{owner}/{repo}", &args).is_err());
    }

    #[test]
    fn plain_uri_passes_through() {
        assert_eq!(fill_template("mcp://static/file.txt", &[]).unwrap(), "mcp://static/file.txt");
    }
}
