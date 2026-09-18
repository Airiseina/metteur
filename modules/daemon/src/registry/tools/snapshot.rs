//! The `SnapshotTake` tool capturing a version snapshot mid-run.

use async_trait::async_trait;
use metteur_shared::Value;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;

use super::Args;
use crate::registry::tool::Tool;

/// Captures a workspace version snapshot during execution.
///
/// Snapshots are unconditional (an explicit take is always intentional) and
/// accept an optional alias for later rollback by name.
pub struct SnapshotTake;

#[async_trait]
impl Tool for SnapshotTake {
    fn name(&self) -> &str {
        "SnapshotTake"
    }

    fn description(&self) -> &str {
        "Captures a version snapshot of the workspace. Pass `description` and \
         an optional `alias` for later rollback by name."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": { "type": "string" },
                "alias": { "type": "string" }
            }
        })
    }

    fn max_result_bytes(&self) -> usize {
        4 * 1024
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let manager = ctx.version_manager.clone().ok_or_else(|| {
            DaemonError::Execution("SnapshotTake requires an attached version manager".to_string())
        })?;
        let a = Args::new(args);
        let description = a.string("description", 0).unwrap_or_default();
        let alias = a.string("alias", 1);
        let snapshot = manager.create_snapshot_with(&description, alias.as_deref())?;
        Ok(Value::Json(serde_json::json!({
            "snapshot_id": snapshot.id.to_string(),
            "alias": snapshot.alias,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::execution::context::ExecutionContext;
    use crate::llm::LlmClientFactory;
    use crate::registry::Registry;

    #[tokio::test]
    async fn takes_snapshot_with_alias() {
        let root = std::env::temp_dir().join(format!("metteur-snap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = Arc::new(crate::storage::versioning::VersionManager::new(db, root.clone()));
        let mut ctx = ExecutionContext::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            root.clone(),
        );
        ctx.version_manager = Some(manager.clone());

        let tool = SnapshotTake;
        let result = tool
            .call(
                &[Value::Json(serde_json::json!({ "description": "mid-run", "alias": "mid" }))],
                &mut ctx,
            )
            .await
            .unwrap();
        let Value::Json(obj) = &result else {
            panic!("expected JSON result");
        };
        assert_eq!(obj.get("alias").and_then(|v| v.as_str()), Some("mid"));
        assert!(obj.get("snapshot_id").and_then(|v| v.as_str()).is_some());
        assert!(manager.find_snapshot_by_alias("mid").unwrap().is_some());
    }

    #[tokio::test]
    async fn requires_version_manager() {
        let mut ctx = ExecutionContext::new(
            Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            std::env::temp_dir(),
        );
        let result = SnapshotTake.call(&[], &mut ctx).await;
        assert!(matches!(result, Err(DaemonError::Execution(_))));
    }
}
