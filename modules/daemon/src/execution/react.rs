//! Shared ReAct loop kernel.
//!
//! [`run_react`] owns the iteration loop previously embedded in the Call LLM
//! node: cancellation/pause handling, interrupt injection, emergency racing,
//! tool invocation with transaction/audit records, anonymization of tool
//! traffic, billing-enriched usage audits and optional context compression.
//! It is reused by the CallLLM node, the SpawnSubAgent tool and the Abstract
//! node planner.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use metteur_shared::Usage;
use metteur_shared::config::{LlmConfig, LlmModelConfig};
use metteur_shared::llm::{
    ContentBlock, ContextManager, GenerationParams, Message, ReasoningEffort, Role, SystemFragment,
    ToolCall, ToolDefinition, ToolResult, ToolResultLifetime,
};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::execution::interrupt::InterruptBus;
use crate::execution::interrupt::InterruptPriority;
use crate::llm::{LlmClient, LlmProviderConfig, LlmResponse, MockClient, MockStep, ProviderKind};
use crate::observability::anon::Anonymizer;

/// The default maximum number of ReAct iterations.
pub const DEFAULT_MAX_ITERATIONS: usize = 10;

/// Options controlling a single ReAct run.
#[derive(Debug, Clone)]
pub struct ReactOptions {
    /// Provider key (`openai-chat`, `anthropic`, `openai-responses` or `mock`).
    pub provider: String,
    /// Explicit model override; falls back to configured defaults.
    pub model: Option<String>,
    /// Explicit base URL override.
    pub base_url: Option<String>,
    /// Explicit API key override.
    pub api_key: Option<String>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Nucleus sampling probability mass.
    pub top_p: Option<f64>,
    /// Maximum number of tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sequences at which generation stops.
    pub stop: Vec<String>,
    /// Reasoning effort for reasoning-capable models.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Seed for reproducible sampling.
    pub seed: Option<i64>,
    /// Presence penalty.
    pub presence_penalty: Option<f64>,
    /// Frequency penalty.
    pub frequency_penalty: Option<f64>,
    /// Maximum number of LLM iterations before giving up.
    pub max_iterations: usize,
    /// Tools callable in this run; `None` allows every registered tool.
    pub allowed_tools: Option<HashSet<String>>,
    /// Compress the context when it exceeds this many messages.
    pub compress_after_messages: Option<usize>,
    /// Human-readable label used in logs and audit details.
    pub label: String,
    /// Scripted text for the mock provider.
    pub mock_text: Option<String>,
    /// Artificial delay before each mock response.
    pub mock_delay_ms: Option<u64>,
}

impl Default for ReactOptions {
    fn default() -> Self {
        Self {
            provider: "openai-chat".to_string(),
            model: None,
            base_url: None,
            api_key: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: Vec::new(),
            reasoning_effort: None,
            seed: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            allowed_tools: None,
            compress_after_messages: None,
            label: String::new(),
            mock_text: None,
            mock_delay_ms: None,
        }
    }
}

/// A step event emitted by the streaming chat variant of the ReAct loop.
#[derive(Debug, Clone)]
pub enum ReactEvent {
    /// A completed assistant turn that answered without tool calls.
    Assistant {
        text: String,
    },
    /// A tool invocation with its (still anonymized) textual result.
    Tool {
        name: String,
        content: String,
    },
}

/// The result of a completed ReAct run.
#[derive(Debug, Clone)]
pub struct ReactOutcome {
    /// The final assistant text (still anonymized).
    pub text: String,
    /// The mutated conversation context after the run.
    pub context: ContextManager,
    /// Token usage accumulated across all completions of the run.
    pub usage: Usage,
}

/// Runs the ReAct loop until the model answers without tool calls.
///
/// Returns the final text together with the mutated context and accumulated
/// usage. The text remains anonymized so callers decide when to restore.
pub async fn run_react(
    ctx: &mut ExecutionContext,
    context: ContextManager,
    opts: &ReactOptions,
) -> DaemonResult<ReactOutcome> {
    react_loop(ctx, context, opts, None, None).await.map_err(|(err, _)| err)
}

/// Streaming variant of [`run_react`] used by the ReAct chat RPC.
///
/// Emits one [`ReactEvent`] per assistant turn and tool call. When `on_delta`
/// is provided the LLM call is streamed and each text delta is forwarded,
/// allowing token-level output. On error the partially mutated context is
/// returned so callers can persist an interrupted session.
pub async fn run_react_streaming(
    ctx: &mut ExecutionContext,
    context: ContextManager,
    opts: &ReactOptions,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
    on_event: &mut (dyn FnMut(ReactEvent) + Send),
) -> Result<ReactOutcome, (DaemonError, ContextManager)> {
    react_loop(ctx, context, opts, on_delta, Some(on_event)).await
}

/// The shared ReAct iteration loop behind [`run_react`] and
/// [`run_react_streaming`].
///
/// Errors carry the context mutated so far so streaming callers can persist a
/// partial session; [`run_react`] discards it to keep the plain signature.
async fn react_loop(
    ctx: &mut ExecutionContext,
    mut context: ContextManager,
    opts: &ReactOptions,
    mut on_delta: Option<&mut (dyn FnMut(String) + Send)>,
    mut on_event: Option<&mut (dyn FnMut(ReactEvent) + Send)>,
) -> Result<ReactOutcome, (DaemonError, ContextManager)> {
    let llm_defaults = llm_default_config(ctx).await;
    let client = match build_client(ctx, opts, &llm_defaults) {
        Ok(client) => client,
        Err(err) => return Err((err, context)),
    };
    let params = build_params(opts, &llm_defaults);
    let tools = tool_definitions(&ctx.registry, opts.allowed_tools.as_ref());
    let anonymizer = build_anonymizer(ctx).await;
    let billing = billing_config(ctx).await;

    let mut total_usage = Usage::default();
    let mut final_text = String::new();

    for _ in 0..opts.max_iterations {
        // Honor cancellation and pause requests.
        if ctx.cancel_requested.load(std::sync::atomic::Ordering::SeqCst) {
            return Err((DaemonError::Interrupted("cancelled by user".to_string()), context));
        }
        while ctx.pause_requested.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Inject deferred normal interrupts and any urgent/emergency
        // interrupts queued before this LLM call.
        while let Some(msg) = ctx.pending_normal.pop_front() {
            context.push_message(Message::text(Role::User, msg));
        }
        if let Some(bus) = &ctx.interrupts {
            for msg in bus.drain(InterruptPriority::Urgent) {
                context.push_message(Message::text(Role::User, msg));
            }
            for msg in bus.drain(InterruptPriority::Emergency) {
                context.push_message(Message::text(Role::User, msg));
            }
        }

        // Compress long contexts before issuing the next request.
        compress_if_needed(
            ctx,
            client.as_ref(),
            &mut context,
            &params,
            opts,
            &billing,
            &mut total_usage,
        )
        .await;

        // Race the LLM call against an emergency interrupt so that an
        // emergency message can abort an in-flight request. The streamed
        // variant forwards text deltas through `on_delta` as they arrive.
        //
        // The model sees an anonymized shadow copy; stored context keeps the
        // original text so chat restore and audit remain readable.
        let response = {
            let outbound = outbound_context(&context, &anonymizer).await;
            let raced = match &mut on_delta {
                Some(delta) => {
                    race_stream(
                        client.as_ref(),
                        &outbound,
                        &params,
                        &tools,
                        &ctx.interrupts,
                        &mut **delta,
                    )
                    .await
                }
                None => {
                    race_complete(client.as_ref(), &outbound, &params, &tools, &ctx.interrupts)
                        .await
                }
            };
            match raced {
                Ok(LlmRace::Response(resp)) => resp,
                Ok(LlmRace::Emergency(msg)) => {
                    context.push_message(Message::text(Role::User, msg));
                    continue;
                }
                Err(err) => return Err((err, context)),
            }
        };

        record_usage(ctx, client.as_ref(), &response.usage, &billing).await;
        total_usage.input_tokens += response.usage.input_tokens;
        total_usage.output_tokens += response.usage.output_tokens;
        total_usage.reasoning_tokens += response.usage.reasoning_tokens;
        total_usage.total_tokens += response.usage.total_tokens;

        if response.tool_calls.is_empty() {
            final_text = response.text;
            if let Some(cb) = on_event.as_deref_mut() {
                cb(ReactEvent::Assistant {
                    text: final_text.clone(),
                });
            }
            break;
        }

        // Record the assistant message with its tool calls.
        context.push_message(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(response.text.clone())],
            tool_calls: response.tool_calls.clone(),
            tool_call_id: None,
        });

        // Execute each tool call and mix the results into the context.
        for call in &response.tool_calls {
            let result =
                match invoke_tool(ctx, call, opts.allowed_tools.as_ref(), &anonymizer).await {
                    Ok(result) => result,
                    Err(err) => return Err((err, context)),
                };
            if let Some(cb) = on_event.as_deref_mut() {
                cb(ReactEvent::Tool {
                    name: call.name.clone(),
                    content: result.clone(),
                });
            }
            let mixed = anonymizer.anonymize(&result).await;
            context.mix_in_tool_result(ToolResult {
                tool_call_id: call.id.clone(),
                content: mixed,
                timestamp: now_millis(),
                lifetime: ToolResultLifetime::OneShot,
            });
        }
    }

    Ok(ReactOutcome {
        text: final_text,
        context,
        usage: total_usage,
    })
}

/// Builds the outbound snapshot sent to the model.
///
/// System fragments, message text and tool-call arguments are anonymized
/// through `anonymizer` (stable tokens, prefix-cache friendly). The stored
/// context is untouched: chat restore and audit keep the original text.
async fn outbound_context(context: &ContextManager, anonymizer: &Anonymizer) -> ContextManager {
    if !anonymizer.is_enabled() {
        return context.clone();
    }
    let mut outbound = context.clone();
    for fragment in &mut outbound.system_fragments {
        fragment.content = anonymizer.anonymize(&fragment.content).await;
    }
    for message in &mut outbound.messages {
        for block in &mut message.content {
            let ContentBlock::Text(text) = block;
            *text = anonymizer.anonymize(text).await;
        }
        for call in &mut message.tool_calls {
            let serialized = anonymizer.anonymize(&call.arguments.to_string()).await;
            if let Ok(arguments) = serde_json::from_str(&serialized) {
                call.arguments = arguments;
            }
        }
    }
    outbound
}

/// The outcome of the raced LLM call.
enum LlmRace {
    /// The model responded before any emergency interrupt arrived.
    Response(LlmResponse),
    /// An emergency interrupt cancelled the call; its message must be injected.
    Emergency(String),
}

/// Calls the model (streamed through `on_delta`), racing the request against
/// an emergency interrupt so the interrupt can abort an in-flight call. The
/// race is isolated here to keep the loop's borrows simple.
async fn race_stream(
    client: &dyn LlmClient,
    context: &ContextManager,
    params: &GenerationParams,
    tools: &[ToolDefinition],
    bus: &Option<InterruptBus>,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> DaemonResult<LlmRace> {
    match bus {
        Some(bus) => {
            let bus = bus.clone();
            tokio::select! {
                resp = client.stream(context, params, tools, on_delta) => {
                    resp.map(LlmRace::Response).map_err(|e| DaemonError::Llm(e.to_string()))
                }
                msg = bus.wait_emergency() => Ok(LlmRace::Emergency(msg.unwrap_or_default())),
            }
        }
        None => {
            let resp = client.stream(context, params, tools, on_delta).await?;
            Ok(LlmRace::Response(resp))
        }
    }
}

/// Non-streamed variant of [`race_stream`].
async fn race_complete(
    client: &dyn LlmClient,
    context: &ContextManager,
    params: &GenerationParams,
    tools: &[ToolDefinition],
    bus: &Option<InterruptBus>,
) -> DaemonResult<LlmRace> {
    match bus {
        Some(bus) => {
            let bus = bus.clone();
            tokio::select! {
                resp = client.complete(context, params, tools) => {
                    resp.map(LlmRace::Response).map_err(|e| DaemonError::Llm(e.to_string()))
                }
                msg = bus.wait_emergency() => Ok(LlmRace::Emergency(msg.unwrap_or_default())),
            }
        }
        None => {
            let resp = client.complete(context, params, tools).await?;
            Ok(LlmRace::Response(resp))
        }
    }
}

/// Returns the LLM defaults from the merged workspace configuration.
async fn llm_default_config(ctx: &ExecutionContext) -> LlmConfig {
    match &ctx.config {
        Some(config) => config.read().await.llm.clone(),
        None => LlmConfig::default(),
    }
}

/// Billing context threaded through usage recording: currency, reporting
/// time zone (peak-window evaluation) and per-model configuration table
/// (prices live in `llm.models`).
pub(crate) type BillingCtx = (String, String, HashMap<String, LlmModelConfig>);

/// Reads the billing section + model table of the merged workspace config.
///
/// The table is indexed by config key *and* by `model_id`, because the client
/// reports the provider id (`model_id`) while configuration is keyed by the
/// user-facing model name.
async fn billing_config(ctx: &ExecutionContext) -> Option<BillingCtx> {
    match &ctx.config {
        Some(config) => {
            let cfg = config.read().await;
            let mut models = cfg.llm.models.clone();
            for (key, model) in &cfg.llm.models {
                if !model.model_id.is_empty() && model.model_id != *key {
                    models.entry(model.model_id.clone()).or_insert_with(|| model.clone());
                }
            }
            Some((cfg.billing.currency.clone(), cfg.billing.timezone.clone(), models))
        }
        None => None,
    }
}

/// Builds an anonymizer from the anonymization configuration.
pub(crate) async fn build_anonymizer(ctx: &ExecutionContext) -> Anonymizer {
    match &ctx.config {
        Some(config) => {
            let anonymize = config.read().await.anonymize.clone();
            Anonymizer::from_config(&anonymize)
        }
        None => Anonymizer::disabled(),
    }
}

/// Builds an LLM client from the run options, resolving the model table.
///
/// The configured model entry (`llm.models[<key>]`) is authoritative for the
/// connection: its `api_type`, `api_endpoint`, `api_key` and `model_id` drive
/// the request, so a caller only has to name the model. Explicit per-call
/// `base_url`/`api_key` overrides still win, and an unconfigured model falls
/// back to the plain provider defaults.
fn build_client(
    ctx: &ExecutionContext,
    opts: &ReactOptions,
    defaults: &LlmConfig,
) -> DaemonResult<Arc<dyn LlmClient>> {
    if opts.provider == "mock" {
        let text = opts.mock_text.clone().unwrap_or_else(|| "mock response".to_string());
        let delay_ms = std::time::Duration::from_millis(opts.mock_delay_ms.unwrap_or(0));
        return Ok(Arc::new(MockClient::new_delayed(vec![MockStep::Text(text)], delay_ms)));
    }

    let model_key = opts
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| defaults.default_model.clone())
        .filter(|m| !m.is_empty());
    let model_cfg = model_key.as_ref().and_then(|key| defaults.models.get(key));

    // A configured `api_type` selects the provider; the option value is the
    // fallback for callers that drive a bare provider without a model entry.
    let provider = model_cfg
        .map(|cfg| cfg.api_type.as_str())
        .filter(|api_type| !api_type.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| opts.provider.clone());
    let kind = match provider.as_str() {
        "openai-chat" | "openai" => ProviderKind::OpenAiChat,
        "anthropic" => ProviderKind::Anthropic,
        "openai-responses" => ProviderKind::OpenAiResponses,
        other => return Err(DaemonError::Execution(format!("unknown llm provider '{other}'"))),
    };

    let model_key = model_key.ok_or_else(|| {
        DaemonError::Execution(
            "no model configured; add one under Settings > LLM & Models".to_string(),
        )
    })?;
    // The id sent to the provider usually differs from the config key.
    let model = model_cfg
        .map(|cfg| cfg.model_id.clone())
        .filter(|id| !id.is_empty())
        .unwrap_or(model_key);
    let base_url = opts
        .base_url
        .clone()
        .filter(|url| !url.is_empty())
        .or_else(|| model_cfg.map(|cfg| cfg.api_endpoint.clone()).filter(|url| !url.is_empty()))
        .unwrap_or_else(|| default_base_url(kind).to_string());
    let api_key = opts
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .or_else(|| model_cfg.map(|cfg| cfg.api_key.clone()).filter(|key| !key.is_empty()))
        .unwrap_or_default();

    let config = LlmProviderConfig::new(kind, base_url, api_key, model);
    ctx.llm_factory.create(&config).map_err(|e| DaemonError::Llm(e.to_string()))
}

fn default_base_url(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAiChat | ProviderKind::OpenAiResponses => "https://api.openai.com/v1",
        ProviderKind::Anthropic => "https://api.anthropic.com/v1",
    }
}

/// Builds generation parameters from the options over configured defaults.
fn build_params(opts: &ReactOptions, defaults: &LlmConfig) -> GenerationParams {
    GenerationParams {
        temperature: opts.temperature.or(defaults.temperature),
        top_p: opts.top_p,
        max_tokens: opts.max_tokens,
        stop: opts.stop.clone(),
        reasoning_effort: opts.reasoning_effort,
        seed: opts.seed,
        presence_penalty: opts.presence_penalty,
        frequency_penalty: opts.frequency_penalty,
    }
}

/// Builds tool definitions from the registry, restricted by `allowed`.
fn tool_definitions(
    registry: &crate::registry::Registry,
    allowed: Option<&HashSet<String>>,
) -> Vec<ToolDefinition> {
    registry
        .tools()
        .into_iter()
        .filter(|t| allowed.map(|set| set.contains(t.name())).unwrap_or(true))
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters(),
        })
        .collect()
}

/// Invokes a tool call and returns the textual result.
///
/// Calls outside `allowed` or unknown to the registry produce an error
/// message as the result so the model can recover. Arguments are
/// deanonymized before execution so tools see real values.
async fn invoke_tool(
    ctx: &mut ExecutionContext,
    call: &ToolCall,
    allowed: Option<&HashSet<String>>,
    anonymizer: &Anonymizer,
) -> DaemonResult<String> {
    if let Some(set) = allowed
        && !set.contains(&call.name)
    {
        return Ok(format!("Error: tool '{}' is not allowed in this context.", call.name));
    }
    let Some(tool) = ctx.registry.tool(&call.name) else {
        return Ok(format!("Error: tool '{}' is not available.", call.name));
    };
    let args_value = deanonymize_arguments(anonymizer, &call.arguments).await;
    let args = arguments_to_values(&args_value);
    ctx.transaction_log.record_tool_call(call.name.clone(), args.clone());
    ctx.audit("tool.call", serde_json::json!({ "name": call.name, "args": args_value }));
    let result = tool.call(&args, ctx).await?;
    Ok(crate::execution::nodes::value_to_string(&result))
}

/// Restores real secrets inside a tool call's JSON arguments.
async fn deanonymize_arguments(
    anonymizer: &Anonymizer,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    let raw = anonymizer.deanonymize(&arguments.to_string()).await;
    serde_json::from_str(&raw).unwrap_or_else(|_| arguments.clone())
}

/// Converts a tool call's JSON arguments into a value list.
///
/// Object arguments are passed as a single JSON value so tools can resolve
/// arguments by name; array arguments are passed positionally.
fn arguments_to_values(args: &serde_json::Value) -> Vec<metteur_shared::Value> {
    match args {
        serde_json::Value::Array(items) => {
            items.iter().map(crate::execution::nodes::json_to_value).collect()
        }
        serde_json::Value::Object(_) => vec![metteur_shared::Value::Json(args.clone())],
        other => vec![crate::execution::nodes::json_to_value(other)],
    }
}

/// Audits one completion's usage, extended with run id and computed cost.
async fn record_usage(
    ctx: &ExecutionContext,
    client: &dyn LlmClient,
    usage: &Usage,
    billing: &Option<BillingCtx>,
) {
    let model = client.model().to_string();
    let mut detail = serde_json::json!({
        "provider": client.provider(),
        "model": model,
        "run_id": ctx.run_id.to_string(),
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
        "total_tokens": usage.total_tokens,
    });
    if let Some((currency, timezone, models)) = billing.as_ref()
        && let Some(model_cfg) = models.get(client.model())
        && let Some(cost) = crate::llm::billing::cost(
            crate::llm::billing::effective_pricing_at(model_cfg, timezone, chrono::Utc::now())
                .as_ref(),
            currency,
            usage,
        )
    {
        detail["cost_micros"] = cost.micros.into();
        detail["currency"] = cost.currency.into();
    }
    ctx.audit("llm.usage", detail);
    if let Some(metrics) = &ctx.metrics {
        use std::sync::atomic::Ordering;
        metrics.llm_calls_total.fetch_add(1, Ordering::Relaxed);
        metrics.llm_input_tokens_total.fetch_add(usage.input_tokens, Ordering::Relaxed);
        metrics
            .llm_output_tokens_total
            .fetch_add(usage.output_tokens + usage.reasoning_tokens, Ordering::Relaxed);
    }
}

/// Compresses the context when it exceeds the configured threshold.
///
/// Compression failures are logged and skipped; they never abort the run.
async fn compress_if_needed(
    ctx: &mut ExecutionContext,
    client: &dyn LlmClient,
    context: &mut ContextManager,
    params: &GenerationParams,
    opts: &ReactOptions,
    billing: &Option<BillingCtx>,
    usage: &mut Usage,
) {
    let Some(keep_recent) = opts.compress_after_messages else {
        return;
    };
    if keep_recent == 0 || context.messages.len() <= keep_recent {
        return;
    }
    let outcome =
        summarize_and_compress(ctx, client, context, params, opts, billing, usage, keep_recent)
            .await;
    if let Err(err) = outcome {
        tracing::warn!("[{}] skipping context compression: {err}", opts.label);
    }
}

/// Produces an LLM summary of the older messages and merges it in.
#[allow(clippy::too_many_arguments)]
async fn summarize_and_compress(
    ctx: &mut ExecutionContext,
    client: &dyn LlmClient,
    context: &mut ContextManager,
    params: &GenerationParams,
    opts: &ReactOptions,
    billing: &Option<BillingCtx>,
    usage: &mut Usage,
    keep_recent: usize,
) -> DaemonResult<()> {
    let older: Vec<Message> = context.messages[..context.messages.len() - keep_recent].to_vec();
    if older.is_empty() {
        return Ok(());
    }
    let mut transcript = String::new();
    for message in &older {
        transcript.push_str(&format!("{:?}: {}\n", message.role, message.text_content()));
    }
    let summarizer_context = ContextManager::new_from_prompt(
        vec![SystemFragment {
            priority: 0,
            scope: "compress".to_string(),
            content: "You condense conversations.".to_string(),
        }],
        format!("Summarize the following conversation concisely:\n\n{transcript}"),
    );
    let response = client
        .complete(&summarizer_context, params, &[])
        .await
        .map_err(|e| DaemonError::Llm(e.to_string()))?;
    record_usage(ctx, client, &response.usage, billing).await;
    usage.input_tokens += response.usage.input_tokens;
    usage.output_tokens += response.usage.output_tokens;
    usage.reasoning_tokens += response.usage.reasoning_tokens;
    usage.total_tokens += response.usage.total_tokens;
    if response.text.trim().is_empty() {
        return Err(DaemonError::Llm("empty compression summary".to_string()));
    }
    let messages_before = context.messages.len();
    let summary = response.text;
    context.compress(keep_recent, |_| Some(summary));
    ctx.audit(
        "llm.compress",
        serde_json::json!({
            "label": opts.label,
            "messages_before": messages_before,
            "messages_after": context.messages.len(),
        }),
    );
    Ok(())
}

/// Returns the current time in milliseconds since the Unix epoch.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_ctx() -> ExecutionContext {
        ExecutionContext::new(
            std::sync::Arc::new(crate::registry::Registry::with_builtins()),
            crate::llm::LlmClientFactory::new(),
            std::env::temp_dir(),
        )
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[tokio::test]
    async fn forbidden_tool_yields_error_message() {
        let mut ctx = new_ctx();
        let allowed = HashSet::from(["ReadFile".to_string()]);
        let anonymizer = Anonymizer::disabled();
        let result = invoke_tool(
            &mut ctx,
            &call("WriteFile", serde_json::json!({ "path": "x.txt" })),
            Some(&allowed),
            &anonymizer,
        )
        .await
        .unwrap();
        assert!(result.contains("not allowed"));
    }

    #[tokio::test]
    async fn unknown_tool_yields_error_message() {
        let mut ctx = new_ctx();
        let anonymizer = Anonymizer::disabled();
        let result =
            invoke_tool(&mut ctx, &call("NoSuchTool", serde_json::json!({})), None, &anonymizer)
                .await
                .unwrap();
        assert!(result.contains("not available"));
    }

    #[tokio::test]
    async fn mock_options_build_mock_client() {
        let ctx = new_ctx();
        let opts = ReactOptions {
            provider: "mock".to_string(),
            mock_text: Some("scripted".to_string()),
            mock_delay_ms: Some(0),
            ..Default::default()
        };
        let client = build_client(&ctx, &opts, &LlmConfig::default()).unwrap();
        assert_eq!(client.provider(), "mock");
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let ctx = new_ctx();
        let opts = ReactOptions {
            provider: "nope".to_string(),
            ..Default::default()
        };
        let err = match build_client(&ctx, &opts, &LlmConfig::default()) {
            Err(err) => err,
            Ok(_) => panic!("expected unknown provider to be rejected"),
        };
        assert!(err.to_string().contains("unknown llm provider"));
    }

    #[tokio::test]
    async fn configured_model_entry_drives_the_client() {
        use metteur_shared::config::LlmModelConfig;
        let ctx = new_ctx();
        let defaults = LlmConfig {
            default_model: Some("deepseek".to_string()),
            models: std::collections::HashMap::from([(
                "deepseek".to_string(),
                LlmModelConfig {
                    api_type: "anthropic".to_string(),
                    api_endpoint: "https://example.test/v1".to_string(),
                    model_id: "deepseek-chat".to_string(),
                    api_key: "sk-test".to_string(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        // The caller only names the model; provider, id and endpoint come from
        // the configured entry.
        let opts = ReactOptions {
            model: Some("deepseek".to_string()),
            ..Default::default()
        };
        let client = build_client(&ctx, &opts, &defaults).unwrap();
        assert_eq!(client.provider(), "anthropic");
        assert_eq!(client.model(), "deepseek-chat");

        // An unconfigured model with no default is an actionable error.
        let err = build_client(&ctx, &ReactOptions::default(), &LlmConfig::default())
            .err()
            .expect("missing model must be rejected");
        assert!(err.to_string().contains("no model configured"));
    }

    #[tokio::test]
    async fn tool_definitions_respect_allow_list() {
        let registry = crate::registry::Registry::with_builtins();
        let all = tool_definitions(&registry, None);
        let restricted =
            tool_definitions(&registry, Some(&HashSet::from(["ReadFile".to_string()])));
        let none = tool_definitions(&registry, Some(&HashSet::new()));
        assert!(all.len() > 1);
        assert_eq!(restricted.len(), 1);
        assert_eq!(restricted[0].name, "ReadFile");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn streaming_forwards_deltas_then_final_assistant() {
        let mut ctx = new_ctx();
        let context = ContextManager::new_from_prompt(vec![], "hi");
        let opts = ReactOptions {
            provider: "mock".to_string(),
            mock_text: Some("streamed".to_string()),
            ..Default::default()
        };
        let mut deltas = Vec::new();
        let mut events = Vec::new();
        let mut on_delta = |text: String| deltas.push(text);
        let mut on_event = |ev: ReactEvent| events.push(ev);
        let outcome =
            run_react_streaming(&mut ctx, context, &opts, Some(&mut on_delta), &mut on_event)
                .await
                .unwrap();
        // The mock provider delivers the whole text as a single delta.
        assert_eq!(deltas, vec!["streamed"]);
        assert!(matches!(&events[0], ReactEvent::Assistant { text } if text == "streamed"));
        assert_eq!(outcome.text, "streamed");
        // The loop keeps the initial user message; the final text is appended
        // by the caller when persisting.
        assert_eq!(outcome.context.messages.len(), 1);
    }

    #[tokio::test]
    async fn cancellation_returns_partial_context() {
        let mut ctx = new_ctx();
        let context = ContextManager::new_from_prompt(vec![], "question");
        ctx.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        let opts = ReactOptions {
            provider: "mock".to_string(),
            ..Default::default()
        };
        let result = run_react_streaming(&mut ctx, context, &opts, None, &mut |_| {}).await;
        let Err((err, partial)) = result else {
            panic!("expected cancellation error");
        };
        assert!(err.to_string().contains("cancelled"));
        // The pre-run context survived the interrupted loop.
        assert_eq!(partial.messages.len(), 1);
        assert_eq!(partial.messages[0].text_content(), "question");
    }
}
