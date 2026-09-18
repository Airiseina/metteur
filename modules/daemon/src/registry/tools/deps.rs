//! The `GetDependencies` tool backed by the workspace dependency graph.

use async_trait::async_trait;
use metteur_shared::{ToolResultLifetime, Value};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;

use super::Args;
use crate::registry::tool::Tool;

/// Queries the project dependency graph for a file.
pub struct GetDependencies;

#[async_trait]
impl Tool for GetDependencies {
    fn name(&self) -> &str {
        "GetDependencies"
    }

    fn description(&self) -> &str {
        "Returns the files a given file imports and the files importing it."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file." }
            },
            "required": ["path"]
        })
    }

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("GetDependencies requires a path".to_string()))?;

        let entry = crate::depgraph::lookup(ctx, &path)?;
        Ok(Value::Json(serde_json::json!({
            "imports": entry.imports,
            "imported_by": entry.imported_by,
        })))
    }
}
