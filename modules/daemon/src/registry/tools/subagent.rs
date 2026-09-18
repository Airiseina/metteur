//! The `SpawnSubAgent` tool: runs a focused sub-agent on an isolated context.

use std::collections::HashSet;

use async_trait::async_trait;
use metteur_shared::Value;
use metteur_shared::llm::{ContextManager, SystemFragment};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::execution::react::{DEFAULT_MAX_ITERATIONS, ReactOptions, run_react};
use crate::registry::tool::Tool;

use super::Args;

/// Maximum SubAgent nesting depth allowed.
const MAX_SUBAGENT_DEPTH: u32 = 4;

/// Spawns a focused sub-agent with its own isolated context.
///
/// The sub-agent runs the shared ReAct kernel from an empty conversation
/// seeded only with the given task, using a possibly cheaper model and a
/// restricted tool set. Its final text is returned as the tool result.
pub struct SpawnSubAgent;

#[async_trait]
impl Tool for SpawnSubAgent {
    fn name(&self) -> &str {
        "SpawnSubAgent"
    }

    fn description(&self) -> &str {
        "Spawns a focused sub-agent with its own isolated context to complete a \
         single task and returns its final answer."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "The task for the sub-agent." },
                "system": { "type": "string", "description": "Optional system prompt for the sub-agent." },
                "model": { "type": "string", "description": "Model override for the sub-agent." },
                "provider": { "type": "string", "description": "LLM provider key (defaults to openai-chat)." },
                "mock_text": { "type": "string", "description": "Scripted response for the mock provider (tests)." },
                "max_iterations": { "type": "integer", "description": "Maximum ReAct iterations (default 10)." },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool names the sub-agent may call; defaults to all except SpawnSubAgent."
                },
                "inherit_context": {
                    "type": "boolean",
                    "description": "Pass the recent conversation (and the system prompt) to the sub-agent. Costs tokens; use it when the task depends on what was already discussed."
                }
            },
            "required": ["task"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let task = a
            .string("task", 0)
            .ok_or_else(|| DaemonError::Execution("SpawnSubAgent requires a task".to_string()))?;
        let system = a.string("system", 1);
        let explicit_model = a.string("model", 2);
        let provider = a.string("provider", 3).unwrap_or_else(|| "openai-chat".to_string());
        let mock_text = a.string("mock_text", 4);
        let max_iterations = a.int("max_iterations", 5).map(|v| v as usize);
        let allowed_tools = allowed_tools_arg(&a);
        let inherit_context = a.bool("inherit_context", 7).unwrap_or(false);

        if ctx.depth >= MAX_SUBAGENT_DEPTH {
            return Err(DaemonError::Execution("subagent depth limit exceeded".to_string()));
        }

        // Model chain: explicit -> configured subagent default -> global default.
        let model = resolve_model(ctx, explicit_model).await;
        let inherit_tokens = match &ctx.config {
            Some(config) => config.read().await.llm.subagent_inherit_tokens as usize,
            None => 4096,
        };
        let default_tools: HashSet<String> = ctx
            .registry
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .filter(|name| name != Self::EXCLUDED_TOOL)
            .collect();
        let allowed_tools = allowed_tools.unwrap_or(default_tools);

        // A fresh sub-agent still needs to know where it works: the harness
        // sections (identity, rules, environment) travel with it either way.
        let mut fragments = crate::harness::HarnessPrompt::fragments(ctx).await;
        if let Some(content) = system {
            fragments.push(SystemFragment {
                priority: crate::harness::sections::PRIORITY_NODE,
                scope: "subagent".to_string(),
                content,
            });
        }
        let mut context =
            inherited_context(ctx, inherit_context, inherit_tokens).unwrap_or_default();
        // A nested sub-agent must not inherit its caller's task instruction:
        // its own replaces it. `apply` then installs the harness sections in
        // place of the copies an inherited context already carries.
        context.system_fragments.retain(|fragment| fragment.scope != "subagent");
        crate::harness::HarnessPrompt::apply(&mut context, fragments);
        push_user_turn(&mut context, task.clone());

        let opts = ReactOptions {
            provider,
            model,
            mock_text,
            max_iterations: max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS),
            allowed_tools: Some(allowed_tools),
            label: "SpawnSubAgent".to_string(),
            ..Default::default()
        };

        let mut child = ctx.child_nested();
        // `task` moves into the child context below; keep a label copy first.
        let label: String = task.chars().take(80).collect();
        ctx.tree_ops.push(crate::execution::TreeOp::SpawnChild {
            kind: crate::execution::TreeNodeKind::SubAgent,
            label,
        });
        if let Some(tx) = &ctx.events {
            let _ = tx.send(crate::execution::ExecutionEvent::Message {
                node_id: ctx.current_node,
                message: format!("subagent started (depth {})", child.depth),
            });
        }
        let outcome = match run_react(&mut child, context, &opts).await {
            Ok(outcome) => outcome,
            Err(err) => {
                ctx.tree_ops.push(crate::execution::TreeOp::FinishCurrent {
                    status: crate::execution::TreeNodeStatus::Failed(err.to_string()),
                });
                return Err(err);
            }
        };
        ctx.tree_ops.push(crate::execution::TreeOp::AddTokens(outcome.usage.total_tokens));
        ctx.tree_ops.push(crate::execution::TreeOp::FinishCurrent {
            status: crate::execution::TreeNodeStatus::Done,
        });
        if let Some(tx) = &ctx.events {
            let _ = tx.send(crate::execution::ExecutionEvent::Message {
                node_id: ctx.current_node,
                message: "subagent finished".to_string(),
            });
        }
        ctx.audit(
            "subagent.run",
            serde_json::json!({
                "model": opts.model.clone().unwrap_or_default(),
                "depth": child.depth,
            }),
        );
        // The result is a summary of a whole nested run; a short header lets
        // the caller judge it without re-reading the transcript, and the byte
        // cap keeps one subagent from flooding the parent context.
        let header = format!(
            "[subagent: model={} tokens={} iterations<= {}]",
            opts.model.clone().unwrap_or_else(|| "default".to_string()),
            outcome.usage.total_tokens,
            opts.max_iterations,
        );
        let text = format!("{header}\n{}", outcome.text);
        let capped = super::result::truncate_result(&text, self.max_result_bytes());
        Ok(Value::String(capped))
    }
}

impl SpawnSubAgent {
    /// The tool excluded from the default tool set to prevent self-recursion.
    const EXCLUDED_TOOL: &'static str = "SpawnSubAgent";
}

/// Builds the seed context of a sub-agent that inherits the caller's
/// conversation.
///
/// Only the system prompt and the recent *prose* turns travel: an assistant
/// turn that carries tool calls cannot be replayed without its responses, so
/// tool traffic is dropped rather than half-copied. The tail is bounded by
/// `[llm].subagent_inherit_tokens`.
fn inherited_context(
    ctx: &ExecutionContext,
    inherit: bool,
    inherit_tokens: usize,
) -> Option<ContextManager> {
    if !inherit {
        return None;
    }
    let mut context = ctx.parent_context.clone()?;
    let history = context.text_history(inherit_tokens);
    context.messages = history;
    context.tool_results.clear();
    Some(context)
}

/// Appends the task as a user turn, extending a trailing user message instead
/// of starting a second one in a row (providers require alternating roles).
fn push_user_turn(context: &mut ContextManager, task: String) {
    use metteur_shared::llm::{Message, Role};
    let trailing_user = context.messages.last().map(|message| message.role) == Some(Role::User);
    let text = match trailing_user {
        true => {
            let previous = context.messages.pop().map(|m| m.text_content()).unwrap_or_default();
            format!("{previous}\n\n{task}")
        }
        false => task,
    };
    context.push_message(Message::text(Role::User, text));
}

/// Resolves the sub-agent model from configuration when not explicit.
async fn resolve_model(ctx: &ExecutionContext, explicit: Option<String>) -> Option<String> {
    if explicit.is_some() {
        return explicit;
    }
    match &ctx.config {
        Some(config) => {
            let llm = &config.read().await.llm;
            llm.subagent_default_model.clone().or_else(|| llm.default_model.clone())
        }
        None => None,
    }
}

/// Extracts the optional allowed-tools set from the arguments.
fn allowed_tools_arg(a: &Args<'_>) -> Option<HashSet<String>> {
    match a.get("allowed_tools", 6) {
        Some(Value::List(items)) => Some(
            items
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    Value::Json(j) => j.as_str().map(str::to_string),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn new_ctx(depth: u32) -> ExecutionContext {
        let mut ctx = ExecutionContext::new(
            std::sync::Arc::new(crate::registry::Registry::with_builtins()),
            crate::llm::LlmClientFactory::new(),
            std::env::temp_dir(),
        );
        ctx.depth = depth;
        ctx
    }

    fn args(task: &str, extra: &[(&str, serde_json::Value)]) -> Vec<Value> {
        let mut obj = serde_json::Map::new();
        obj.insert("task".to_string(), serde_json::json!(task));
        for (key, value) in extra {
            obj.insert((*key).to_string(), value.clone());
        }
        vec![Value::Json(serde_json::Value::Object(obj))]
    }

    #[tokio::test]
    async fn rejects_when_depth_limit_exceeded() {
        let mut ctx = new_ctx(MAX_SUBAGENT_DEPTH);
        let result = SpawnSubAgent.call(&args("do it", &[]), &mut ctx).await;
        assert!(
            matches!(result, Err(DaemonError::Execution(msg)) if msg.contains("depth limit exceeded"))
        );
    }

    #[tokio::test]
    async fn runs_sub_agent_with_mock_provider() {
        let mut ctx = new_ctx(0);
        let out = SpawnSubAgent
            .call(
                &args(
                    "summarize things",
                    &[
                        ("provider", serde_json::json!("mock")),
                        ("mock_text", serde_json::json!("sub agent answer")),
                    ],
                ),
                &mut ctx,
            )
            .await
            .unwrap();
        // The answer is prefixed with a metadata header the caller can audit.
        let Value::String(text) = out else {
            panic!("expected a string result");
        };
        assert!(text.starts_with("[subagent:"), "{text}");
        assert!(text.ends_with("sub agent answer"), "{text}");
    }

    #[tokio::test]
    async fn resolves_model_through_configuration_chain() {
        use metteur_shared::config::{Config, LlmConfig};
        use tokio::sync::RwLock;

        let ctx = new_ctx(0);
        // Without configuration the model stays unset.
        assert_eq!(resolve_model(&ctx, None).await, None);
        // Explicit task model always wins.
        assert_eq!(
            resolve_model(&ctx, Some("task-model".to_string())).await,
            Some("task-model".to_string())
        );

        let config = Config {
            llm: LlmConfig {
                default_model: Some("fallback-model".to_string()),
                subagent_default_model: Some("sub-model".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut configured = new_ctx(0);
        configured.config = Some(Arc::new(RwLock::new(config)));
        assert_eq!(resolve_model(&configured, None).await, Some("sub-model".to_string()));
        assert_eq!(
            resolve_model(&configured, Some("task-model".to_string())).await,
            Some("task-model".to_string())
        );
    }
}
