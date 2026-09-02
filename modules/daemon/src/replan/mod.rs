//! Replanning: user-approved edits to the executing blueprint.
//!
//! The `ReplanBlueprint` tool and the circuit breaker both funnel a JSON
//! *edit script* into [`approve_and_apply`], which asks the user for
//! approval and then applies the edits to the shared root blueprint,
//! persisting the result back to the workspace database so future runs
//! honour it too.

use metteur_shared::llm::ContextManager;
use metteur_shared::{Blueprint, NodeId};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::execution::interpreter::ExecutionEvent;
use crate::execution::react::{ReactOptions, run_react};
use crate::sandbox::approval::Decision;

/// One edit operation; parsed from `{ "op": ..., "match": ..., ... }` JSON.
enum EditOp {
    /// Replace `node.data` wholesale.
    SetData {
        selector: NodeSelector,
        data: serde_json::Value,
    },
    /// Replace one key inside `node.data` (a pin default value).
    SetPin {
        selector: NodeSelector,
        key: String,
        value: serde_json::Value,
    },
}

/// Selects a node by `{"kind": ..., "nth": N}` (1-based occurrence).
#[derive(Debug)]
struct NodeSelector {
    kind: String,
    nth: usize,
}

impl NodeSelector {
    fn find_index(&self, bp: &Blueprint) -> Option<usize> {
        let skip = self.nth.checked_sub(1)?;
        bp.nodes.iter().enumerate().filter(|(_, n)| n.kind == self.kind).nth(skip).map(|(i, _)| i)
    }
}

/// Parses `edits` (a JSON array) into typed operations.
fn parse_edits(script: &serde_json::Value) -> Result<Vec<EditOp>, String> {
    let edits = script.as_array().ok_or_else(|| "edits must be a JSON array".to_string())?;
    if edits.is_empty() {
        return Err("edits is empty".to_string());
    }
    edits.iter().map(parse_edit).collect()
}

fn parse_edit(edit: &serde_json::Value) -> Result<EditOp, String> {
    let obj = edit.as_object().ok_or_else(|| "each edit must be a JSON object".to_string())?;
    let op = obj.get("op").and_then(|v| v.as_str()).ok_or_else(|| "edit missing 'op'".to_string())?;
    let selector = parse_selector(obj.get("match"))?;
    match op {
        "set_data" => {
            let data = obj.get("data").cloned().ok_or_else(|| "set_data requires 'data'".to_string())?;
            Ok(EditOp::SetData {
                selector,
                data,
            })
        }
        "set_pin" => {
            let key = obj
                .get("pin")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "set_pin requires 'pin'".to_string())?
                .to_string();
            let value = obj.get("value").cloned().ok_or_else(|| "set_pin requires 'value'".to_string())?;
            Ok(EditOp::SetPin {
                selector,
                key,
                value,
            })
        }
        other => Err(format!("unknown edit op '{other}'")),
    }
}

fn parse_selector(value: Option<&serde_json::Value>) -> Result<NodeSelector, String> {
    let obj = value
        .and_then(|v| v.as_object())
        .ok_or_else(|| "edit missing 'match' object".to_string())?;
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "match.kind is required".to_string())?
        .to_string();
    let nth = obj.get("nth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    Ok(NodeSelector {
        kind,
        nth: nth.max(1),
    })
}

/// Applies the edit script to `bp`, returning a short human summary.
pub fn apply_edits(bp: &mut Blueprint, script: &serde_json::Value) -> Result<String, String> {
    let ops = parse_edits(script)?;
    let mut summary = Vec::new();
    for op in ops {
        match op {
            EditOp::SetData {
                selector,
                data,
            } => {
                let idx = selector.find_index(bp).ok_or_else(|| {
                    format!("no node matching kind '{}' (nth {})", selector.kind, selector.nth)
                })?;
                bp.nodes[idx].data = data;
                summary.push(format!("{}#{} data", selector.kind, selector.nth));
            }
            EditOp::SetPin {
                selector,
                key,
                value,
            } => {
                let idx = selector.find_index(bp).ok_or_else(|| {
                    format!("no node matching kind '{}' (nth {})", selector.kind, selector.nth)
                })?;
                if !bp.nodes[idx].data.is_object() {
                    bp.nodes[idx].data = serde_json::json!({});
                }
                bp.nodes[idx].data.as_object_mut().map(|m| m.insert(key.clone(), value));
                summary.push(format!("{}#{} {key}", selector.kind, selector.nth));
            }
        }
    }
    Ok(summary.join(", "))
}

/// Opens an approval request on the shared broker and emits the matching
/// `approval_request` event. Returns whether the user allowed it.
///
/// A missing broker or a timeout both count as a denial.
pub async fn await_approval(
    ctx: &mut ExecutionContext,
    request_type: &str,
    message: &str,
    detail: serde_json::Value,
) -> DaemonResult<bool> {
    let broker = ctx
        .approvals
        .clone()
        .ok_or_else(|| DaemonError::Execution("no approval broker available".to_string()))?;
    let detail_json = serde_json::to_string(&detail)
        .unwrap_or_else(|_| serde_json::json!({ "request_type": request_type }).to_string());
    let command_hash = crate::sandbox::command_hash(&format!("{message}\n{detail_json}"));
    let (request_id, rx) = broker.open_request(command_hash, format!("{message}\n{detail_json}"));
    if let Some(tx) = &ctx.events {
        let _ = tx.send(ExecutionEvent::ApprovalRequested {
            node_id: ctx.current_node,
            request_id,
            detail: detail_json,
        });
    }
    let timeout_secs = ctx
        .config
        .as_ref()
        .map(|c| c.try_read().map(|cfg| cfg.sandbox.approval_timeout_secs).unwrap_or(0))
        .unwrap_or(0)
        .max(30);
    let decision = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
        .await
        .map_err(|_| DaemonError::Execution("approval request timed out".to_string()))?
        .map_err(|_| DaemonError::Execution("approval request cancelled".to_string()))?;
    Ok(decision == Decision::Allow)
}

/// Persists the edited blueprint back to the workspace database.
fn persist(ctx: &ExecutionContext, bp: &Blueprint) -> DaemonResult<()> {
    let Some(db) = &ctx.workspace_db else {
        return Ok(());
    };
    let data =
        serde_json::to_vec(bp).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    db.put(crate::storage::persistence::cf::BLUEPRINTS, bp.id.as_bytes(), &data)
}

/// Applies `script` to the shared root blueprint and persists the result.
pub async fn apply_to_shared(
    ctx: &mut ExecutionContext,
    script: &serde_json::Value,
) -> DaemonResult<String> {
    let handle = ctx
        .blueprint
        .clone()
        .ok_or_else(|| DaemonError::Execution("no shared blueprint attached".to_string()))?;
    let summary = {
        let mut bp = handle.write();
        replan_apply(&mut bp, script)?
    };
    persist(ctx, &handle.read())?;
    ctx.audit(
        "replan.applied",
        serde_json::json!({ "script": script, "summary": &summary }),
    );
    Ok(summary)
}

fn replan_apply(bp: &mut Blueprint, script: &serde_json::Value) -> DaemonResult<String> {
    apply_edits(bp, script).map_err(DaemonError::Execution)
}

/// Asks the user to approve `script` and, when allowed, applies it.
pub async fn approve_and_apply(
    ctx: &mut ExecutionContext,
    summary: &str,
    script: &serde_json::Value,
) -> DaemonResult<String> {
    let allow = await_approval(
        ctx,
        "replan_proposal",
        summary,
        serde_json::json!({
            "request_type": "replan_proposal",
            "tool": "ReplanBlueprint",
            "summary": summary,
            "edits": script,
        }),
    )
    .await?;
    if !allow {
        return Err(DaemonError::Execution("replan denied by user".to_string()));
    }
    apply_to_shared(ctx, script).await
}

const PLAN_AGENT_SYSTEM: &str = "You are the Metteur plan agent. The task has \
    repeatedly failed its deterministic validation. Produce a JSON edit script \
    that fixes the Blueprint, addressing ONLY the failure described. Each edit \
    is one of: \
    {\"op\":\"set_data\",\"match\":{\"kind\":\"<Kind>\",\"nth\":1},\"data\":{...}} \
    or \
    {\"op\":\"set_pin\",\"match\":{\"kind\":\"<Kind>\",\"nth\":1},\"pin\":\"<pin>\",\"value\":<value>}. \
    Node kinds known: the failing validator/judge and the node kind listed in the task. \
    Output ONLY the JSON array, no prose.";

/// Runs the internal PlanAgent to produce an edit script for the failure.
///
/// `mock_text` lets tests script the provider output.
pub async fn run_plan_agent(
    ctx: &mut ExecutionContext,
    task: &str,
    mock_text: Option<String>,
) -> DaemonResult<serde_json::Value> {
    let context = ContextManager::new_from_prompt(
        vec![metteur_shared::llm::SystemFragment {
            priority: 0,
            scope: "plan_agent".to_string(),
            content: PLAN_AGENT_SYSTEM.to_string(),
        }],
        task,
    );
    let opts = ReactOptions {
        provider: if mock_text.is_some() {
            "mock".to_string()
        } else {
            "openai-chat".to_string()
        },
        model: None,
        mock_text,
        allowed_tools: Some(std::collections::HashSet::new()),
        label: "PlanAgent".to_string(),
        ..Default::default()
    };
    let outcome = run_react(ctx, context, &opts).await?;
    let text = strip_code_fence(&outcome.text);
    serde_json::from_str(&text)
        .map_err(|e| DaemonError::Execution(format!("plan agent produced invalid JSON: {e}")))
}

/// Strips a ```json ... ``` fence an LLM may add around its answer.
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    let start = trimmed.find('[').unwrap_or(0);
    let end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    trimmed[start..end].to_string()
}

/// The circuit breaker: asks the user, runs the plan agent, re-approves the
/// produced edits and applies them. Returns the apply summary.
pub async fn trip_and_replan(
    ctx: &mut ExecutionContext,
    failing_node: NodeId,
    failures: u32,
) -> DaemonResult<String> {
    trip_and_replan_core(ctx, failing_node, failures, None).await
}

/// [`trip_and_replan`] with a scripted plan-agent output (tests).
pub async fn trip_and_replan_mock(
    ctx: &mut ExecutionContext,
    failing_node: NodeId,
    failures: u32,
    mock_text: String,
) -> DaemonResult<String> {
    trip_and_replan_core(ctx, failing_node, failures, Some(mock_text)).await
}

async fn trip_and_replan_core(
    ctx: &mut ExecutionContext,
    failing_node: NodeId,
    failures: u32,
    plan_mock: Option<String>,
) -> DaemonResult<String> {
    let task = format!(
        "Validation failed {failures} consecutive times at node {failing_node}. \
         Propose edits that make it pass. If a CallLLM node feeds it, tighten its \
         system/prompt; if the validator compares against a constant, fix that \
         constant, ensuring the caller output satisfies it."
    );
    let allow = await_approval(
        ctx,
        "circuit_tripped",
        "The run hit the circuit breaker after repeated validation failures.",
        serde_json::json!({
            "request_type": "circuit_tripped",
            "node_id": failing_node.to_string(),
            "failures": failures,
        }),
    )
    .await?;
    if !allow {
        return Err(DaemonError::Execution("circuit tripped; aborted by user".to_string()));
    }
    let edits = run_plan_agent(ctx, &task, plan_mock).await?;
    approve_and_apply(ctx, &format!("Plan agent produced a revised plan for {failing_node}"), &edits)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: &str) -> metteur_shared::Node {
        metteur_shared::Node {
            id: uuid::Uuid::new_v4(),
            node_type: metteur_shared::NodeType::Function,
            kind: kind.to_string(),
            position: (0.0, 0.0),
            pins: Vec::new(),
            data: serde_json::json!({ "old": 1 }),
        }
    }

    fn bp() -> Blueprint {
        Blueprint {
            id: uuid::Uuid::new_v4(),
            name: "t".to_string(),
            nodes: vec![node("Validator"), node("CallLLM")],
            edges: Vec::new(),
            entry_node_id: uuid::Uuid::new_v4(),
        }
    }

    #[test]
    fn set_data_replaces_node_data() {
        let mut b = bp();
        let s = apply_edits(
            &mut b,
            &serde_json::json!([{
                "op": "set_data",
                "match": { "kind": "Validator", "nth": 1 },
                "data": { "mode": "not_empty" }
            }]),
        )
        .unwrap();
        assert!(s.contains("Validator#1"));
        assert_eq!(b.nodes[0].data["mode"], "not_empty");
    }

    #[test]
    fn set_pin_updates_one_key() {
        let mut b = bp();
        apply_edits(
            &mut b,
            &serde_json::json!([{
                "op": "set_pin",
                "match": { "kind": "CallLLM", "nth": 1 },
                "pin": "temperature",
                "value": 0.2
            }]),
        )
        .unwrap();
        assert_eq!(b.nodes[1].data["temperature"], 0.2);
        assert_eq!(b.nodes[1].data["old"], 1);
    }

    #[test]
    fn unknown_op_or_missing_match_is_rejected() {
        let mut b = bp();
        assert!(apply_edits(&mut b, &serde_json::json!([{ "op": "nope" }])).is_err());
        assert!(apply_edits(
            &mut b,
            &serde_json::json!([{ "op": "set_data", "match": { "kind": "Validator", "nth": 9 }, "data": {} }])
        )
        .is_err());
    }

    #[test]
    fn strips_code_fence() {
        assert_eq!(strip_code_fence("```json\n[{\"op\":\"x\"}]\n```"), "[{\"op\":\"x\"}]");
    }

    #[tokio::test]
    async fn trip_and_replan_mock_applies_approved_edits() {
        use crate::llm::LlmClientFactory;
        use crate::registry::Registry;
        use crate::sandbox::approval::{ApprovalBroker, Decision};
        use metteur_shared::config::Config;

        let broker = std::sync::Arc::new(ApprovalBroker::new());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = crate::execution::context::ExecutionContext::new(
            std::sync::Arc::new(Registry::with_builtins()),
            LlmClientFactory::new(),
            std::env::temp_dir(),
        );
        let shared = std::sync::Arc::new(parking_lot::RwLock::new(bp()));
        ctx.blueprint = Some(shared.clone());
        ctx.approvals = Some(broker.clone());
        ctx.events = Some(tx);
        ctx.config = Some(std::sync::Arc::new(tokio::sync::RwLock::new(Config::default())));

        let node_id = NodeId::nil();
        let mock = r#"[{"op":"set_pin","match":{"kind":"CallLLM","nth":1},"pin":"temperature","value":0.2}]"#
            .to_string();
        let task = tokio::spawn(async move {
            trip_and_replan_mock(&mut ctx, node_id, 3, mock).await
        });

        // Allow both the circuit-tripped and the replan-proposal requests.
        let mut allowed = 0u32;
        while allowed < 2 {
            let ids = broker.pending_ids();
            if !ids.is_empty() {
                for id in ids {
                    broker.respond(&id, Decision::Allow);
                    allowed += 1;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let summary = task.await.unwrap().unwrap();
        assert!(summary.contains("temperature"), "summary: {summary}");
        let edited = shared.read();
        assert_eq!(edited.nodes[1].data["temperature"], 0.2);
    }
}