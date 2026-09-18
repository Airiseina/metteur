//! Call LLM node executor backed by the shared ReAct kernel.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::llm::{ContextManager, ReasoningEffort, SystemFragment};
use metteur_shared::{Node, PinId, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;
use crate::execution::react::{ReactOptions, run_react};
use crate::registry::NodeExecutor;

use super::string_output;

/// A node that runs a ReAct loop against an LLM.
///
/// The node reads an optional [`ContextManager`] from its `Context` input pin
/// (creating one from `system`/`prompt` data if absent) and delegates the
/// completion/tool-call loop to [`run_react`]. The final text is written to
/// the `Result` pin and the (possibly mutated) context to the `Context`
/// output pin.
pub struct CallLlmExecutor;

#[async_trait]
impl NodeExecutor for CallLlmExecutor {
    fn kind(&self) -> &str {
        "CallLLM"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        // Resolve the input context, cloning it for isolation.
        let mut system = crate::harness::HarnessPrompt::fragments(ctx).await;
        // The node's own instruction is task-level: it renders after the
        // harness sections, close to the conversation.
        if let Some(text) = node
            .data
            .get("system")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            system.push(SystemFragment {
                priority: crate::harness::sections::PRIORITY_NODE,
                scope: "call_llm.node".to_string(),
                content: text.to_string(),
            });
        }
        // Addon prompt fragments follow the node-provided ones.
        system.extend(ctx.addon_fragments.iter().cloned());
        let context = match input_context(node, inputs) {
            Some(c) => c,
            None => ContextManager::new_from_prompt(
                system,
                node.data.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            ),
        };

        let opts = react_options_from_node(node, ctx).await;
        let outcome = run_react(ctx, context, &opts).await?;
        ctx.tree_ops.push(crate::execution::TreeOp::AddTokens(outcome.usage.total_tokens));

        // Stream the final context composition so the audit UI can render a
        // live per-region usage bar for this CallLLM node.
        if let Some(tx) = &ctx.events {
            let regions = outcome.context.usage_report();
            let _ = tx.send(crate::execution::ExecutionEvent::ContextUsage {
                node_id: node.id,
                regions,
            });
        }

        // Write outputs: Result (text) and Context (cloned manager).
        let mut outputs = string_output(node, "Result", outcome.text)?;
        if let Some(pin) = node.pins.iter().find(|p| p.name == "Context") {
            outputs.insert(pin.id, Value::Context(outcome.context));
        }
        Ok(outputs)
    }
}

/// Reads the input context from the `Context` pin, if present.
fn input_context(node: &Node, inputs: &HashMap<PinId, Value>) -> Option<ContextManager> {
    let pin = node.pins.iter().find(|p| p.name == "Context")?;
    inputs.get(&pin.id).and_then(|v| v.as_context()).cloned()
}

/// Builds ReAct options from the node's data keys, over the workspace
/// defaults for the knobs the node does not set itself.
async fn react_options_from_node(node: &Node, ctx: &ExecutionContext) -> ReactOptions {
    let defaults = match &ctx.config {
        Some(config) => config.read().await.llm.clone(),
        None => Default::default(),
    };
    let data = &node.data;
    let string = |key: &str| {
        data.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
    };
    ReactOptions {
        provider: string("provider").unwrap_or_else(|| "openai-chat".to_string()),
        model: string("model"),
        base_url: string("base_url"),
        api_key: string("api_key"),
        temperature: data.get("temperature").and_then(|v| v.as_f64()),
        top_p: data.get("top_p").and_then(|v| v.as_f64()),
        max_tokens: data.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as u32),
        stop: data
            .get("stop")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default(),
        reasoning_effort: data.get("reasoning_effort").and_then(|v| v.as_str()).and_then(
            |s| match s {
                "none" => Some(ReasoningEffort::None),
                "low" => Some(ReasoningEffort::Low),
                "medium" => Some(ReasoningEffort::Medium),
                "high" => Some(ReasoningEffort::High),
                _ => None,
            },
        ),
        seed: data.get("seed").and_then(|v| v.as_i64()),
        presence_penalty: data.get("presence_penalty").and_then(|v| v.as_f64()),
        frequency_penalty: data.get("frequency_penalty").and_then(|v| v.as_f64()),
        max_iterations: data
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(crate::execution::react::DEFAULT_MAX_ITERATIONS),
        allowed_tools: None,
        compress_after_messages: data
            .get("compress_after_messages")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        label: "CallLLM".to_string(),
        mock_text: string("mock_text"),
        mock_delay_ms: data.get("mock_delay_ms").and_then(|v| v.as_u64()),
        tool_error_limit: defaults.tool_error_limit,
        repeat_call_limit: defaults.repeat_call_limit,
        max_tool_results: defaults.max_tool_results as usize,
        parallel_read_tools: defaults.parallel_read_tools,
        stale_result_placeholders: defaults.stale_result_placeholders,
        dedup_reads: defaults.dedup_reads,
        max_tool_result_bytes: defaults.max_tool_result_bytes as usize,
        anonymize_thinking: defaults.anonymize_thinking,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DaemonError;
    use metteur_shared::{DataType, Pin, PinType};

    fn build_node(data: serde_json::Value) -> Node {
        Node {
            id: uuid::Uuid::new_v4(),
            node_type: metteur_shared::NodeType::Function,
            kind: "CallLLM".to_string(),
            position: (0.0, 0.0),
            pins: vec![
                Pin {
                    id: uuid::Uuid::new_v4(),
                    name: "Result".to_string(),
                    pin_type: PinType::DataOutput,
                    data_type: DataType::String,
                    ..Default::default()
                },
                Pin {
                    id: uuid::Uuid::new_v4(),
                    name: "Context".to_string(),
                    pin_type: PinType::DataOutput,
                    data_type: DataType::Json,
                    ..Default::default()
                },
            ],
            data,
        }
    }

    fn new_ctx() -> ExecutionContext {
        ExecutionContext::new(
            std::sync::Arc::new(crate::registry::Registry::with_builtins()),
            crate::llm::LlmClientFactory::new(),
            std::env::temp_dir(),
        )
    }

    #[tokio::test]
    async fn mock_provider_returns_text() {
        let node = build_node(serde_json::json!({
            "provider": "mock",
            "mock_text": "hello from mock",
        }));
        let mut ctx = new_ctx();
        let executor = CallLlmExecutor;
        let outputs = executor.execute(&node, &HashMap::new(), &mut ctx).await.unwrap();
        let result = outputs.values().find(|v| matches!(v, Value::String(_))).unwrap();
        assert_eq!(result.as_str().unwrap(), "hello from mock");
    }

    #[tokio::test]
    async fn mock_provider_outputs_context() {
        let node = build_node(serde_json::json!({
            "provider": "mock",
            "mock_text": "hi",
        }));
        let mut ctx = new_ctx();
        let executor = CallLlmExecutor;
        let outputs = executor.execute(&node, &HashMap::new(), &mut ctx).await.unwrap();
        assert!(outputs.values().any(|v| matches!(v, Value::Context(_))));
    }

    #[tokio::test]
    async fn unknown_provider_fails_with_error() {
        let node = build_node(serde_json::json!({
            "provider": "nope",
        }));
        let mut ctx = new_ctx();
        let executor = CallLlmExecutor;
        let result = executor.execute(&node, &HashMap::new(), &mut ctx).await;
        assert!(matches!(result, Err(DaemonError::Execution(_))));
    }
}
