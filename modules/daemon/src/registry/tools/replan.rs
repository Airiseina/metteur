//! The `ReplanBlueprint` tool: modifies the running blueprint after approval.
//!
//! The LLM proposes a JSON edit script; the tool asks the user for approval
//! (via the sandbox approval channel) and only then applies the edits to the
//! shared root blueprint, so data-level changes take effect on the remainder
//! of the current run.

use async_trait::async_trait;
use metteur_shared::Value;

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;
use crate::registry::Tool;

use super::Args;

/// A tool that lets the LLM adjust the plan mid-run with user consent.
pub struct ReplanBlueprint;

#[async_trait]
impl Tool for ReplanBlueprint {
    fn name(&self) -> &str {
        "ReplanBlueprint"
    }

    fn description(&self) -> &str {
        "Modifies the currently executing blueprint after the user approves. \
         `summary` describes the change; `edits` is a JSON array of \
         {\"op\":\"set_data\"|\"set_pin\",\"match\":{\"kind\":\",\"nth\":},\"data\"|\"pin\"+\"value\"} \
         operations matching existing nodes by kind and occurrence. Data-level \
         edits apply immediately to the rest of the run."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Human summary of the proposed plan change."
                },
                "edits": {
                    "type": "array",
                    "description": "Edit operations applied to the blueprint."
                }
            },
            "required": ["summary", "edits"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let summary = a
            .string("summary", 0)
            .ok_or_else(|| crate::error::DaemonError::Execution(
                "ReplanBlueprint requires a summary".to_string()
            ))?;
        let edits = a
            .get("edits", 1)
            .ok_or_else(|| crate::error::DaemonError::Execution(
                "ReplanBlueprint requires edits".to_string()
            ))?;
        let edits_json = match &edits {
            Value::Json(j) => j.clone(),
            other => serde_json::json!([other]),
        };
        let applied = crate::replan::approve_and_apply(ctx, &summary, &edits_json).await?;
        Ok(Value::String(format!(
            "Replan approved and applied: {applied}. Data-level edits affect the rest of this run; structural changes take effect on the next run."
        )))
    }
}