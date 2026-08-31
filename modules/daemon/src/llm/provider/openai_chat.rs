//! OpenAI Chat Completions provider (`POST /chat/completions`).

use async_trait::async_trait;
use metteur_shared::llm::{
    ContentBlock, ContextManager, GenerationParams, Message, ReasoningEffort, Role, ToolCall,
    ToolDefinition, Usage,
};
use serde_json::{Value as Json, json};

use crate::error::{DaemonError, DaemonResult};

use super::super::client::{LlmClient, LlmProviderConfig, LlmResponse, http_error};

/// A client for the OpenAI Chat Completions API.
pub struct OpenAiChatClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    default_params: GenerationParams,
}

impl OpenAiChatClient {
    /// Creates a new OpenAI Chat client.
    pub fn new(http: reqwest::Client, config: &LlmProviderConfig) -> Self {
        Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            default_params: config.default_params.clone(),
        }
    }

    /// Builds the request body for a completion.
    fn build_body(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
    ) -> Json {
        let mut body = json!({
            "model": self.model,
            "messages": build_messages(ctx),
        });
        apply_params(&mut body, params, &self.default_params);
        if !tools.is_empty() {
            body["tools"] = build_tools(tools);
        }
        body
    }

    /// Sends the request and parses the response.
    async fn send(&self, body: Json) -> DaemonResult<LlmResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| http_error("openai chat request", e))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| http_error("openai chat response", e))?;
        if !status.is_success() {
            return Err(DaemonError::Llm(format!("openai chat returned {status}: {text}")));
        }
        let parsed: Json = serde_json::from_str(&text)
            .map_err(|e| DaemonError::Llm(format!("invalid openai chat response: {e}")))?;
        parse_response(&parsed)
    }
}

#[async_trait]
impl LlmClient for OpenAiChatClient {
    fn provider(&self) -> &str {
        "openai-chat"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
    ) -> DaemonResult<LlmResponse> {
        let body = self.build_body(ctx, params, tools);
        self.send(body).await
    }

    async fn stream(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> DaemonResult<LlmResponse> {
        let mut body = self.build_body(ctx, params, tools);
        body["stream"] = json!(true);
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| http_error("openai chat stream request", e))?;
        let status = resp.status();
        if !status.is_success() {
            let text =
                resp.text().await.map_err(|e| http_error("openai chat stream response", e))?;
            return Err(DaemonError::Llm(format!("openai chat returned {status}: {text}")));
        }

        let mut stream = resp.bytes_stream();
        let mut text_out = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage = Usage::default();
        let mut buf = String::new();

        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| http_error("openai chat stream", e))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // Process complete SSE lines.
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].to_string();
                buf = buf[pos + 1..].to_string();
                let line = line.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line[5..].trim();
                if data == "[DONE]" {
                    continue;
                }
                let chunk_json: Json = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(delta) = chunk_json.pointer("/choices/0/delta/content")
                    && let Some(s) = delta.as_str()
                {
                    text_out.push_str(s);
                    on_delta(s.to_string());
                }
                if let Some(calls) = chunk_json.pointer("/choices/0/delta/tool_calls")
                    && let Some(arr) = calls.as_array()
                {
                    for call in arr {
                        merge_tool_call(&mut tool_calls, call);
                    }
                }
                if let Some(u) = chunk_json.get("usage") {
                    usage = parse_usage(u);
                }
            }
        }
        Ok(LlmResponse {
            text: text_out,
            tool_calls,
            usage,
        })
    }
}

/// Converts a context into the OpenAI messages array.
fn build_messages(ctx: &ContextManager) -> Vec<Json> {
    let mut messages: Vec<Json> = Vec::new();
    // System fragments become a single system message.
    if !ctx.system_fragments.is_empty() {
        let system = ctx
            .system_fragments
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        messages.push(json!({ "role": "system", "content": system }));
    }
    for msg in &ctx.messages {
        messages.push(message_to_json(msg));
    }
    messages
}

/// Converts a shared message into an OpenAI message object.
fn message_to_json(msg: &Message) -> Json {
    let content = msg
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text(t) => t.clone(),
        })
        .collect::<Vec<_>>()
        .join("");
    let mut obj = json!({ "role": role_str(msg.role), "content": content });
    if !msg.tool_calls.is_empty() {
        let calls: Vec<Json> = msg
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": {
                        "name": c.name,
                        "arguments": c.arguments.to_string(),
                    }
                })
            })
            .collect();
        obj["tool_calls"] = Json::Array(calls);
    }
    if let Some(id) = &msg.tool_call_id {
        obj["tool_call_id"] = json!(id);
    }
    obj
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Applies generation parameters to the request body.
fn apply_params(body: &mut Json, params: &GenerationParams, defaults: &GenerationParams) {
    let merged = merge_params(defaults, params);
    if let Some(v) = merged.temperature {
        body["temperature"] = json!(v);
    }
    if let Some(v) = merged.top_p {
        body["top_p"] = json!(v);
    }
    if let Some(v) = merged.max_tokens {
        body["max_completion_tokens"] = json!(v);
    }
    if !merged.stop.is_empty() {
        body["stop"] = json!(merged.stop);
    }
    if let Some(v) = merged.seed {
        body["seed"] = json!(v);
    }
    if let Some(v) = merged.presence_penalty {
        body["presence_penalty"] = json!(v);
    }
    if let Some(v) = merged.frequency_penalty {
        body["frequency_penalty"] = json!(v);
    }
    if let Some(effort) = merged.reasoning_effort {
        body["reasoning_effort"] = json!(reasoning_str(effort));
    }
}

/// Merges default params with request params (request wins).
fn merge_params(defaults: &GenerationParams, params: &GenerationParams) -> GenerationParams {
    GenerationParams {
        temperature: params.temperature.or(defaults.temperature),
        top_p: params.top_p.or(defaults.top_p),
        max_tokens: params.max_tokens.or(defaults.max_tokens),
        stop: if params.stop.is_empty() {
            defaults.stop.clone()
        } else {
            params.stop.clone()
        },
        reasoning_effort: params.reasoning_effort.or(defaults.reasoning_effort),
        seed: params.seed.or(defaults.seed),
        presence_penalty: params.presence_penalty.or(defaults.presence_penalty),
        frequency_penalty: params.frequency_penalty.or(defaults.frequency_penalty),
    }
}

fn reasoning_str(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Parses a chat completion response into a shared response.
fn parse_response(parsed: &Json) -> DaemonResult<LlmResponse> {
    let choice = parsed
        .pointer("/choices/0/message")
        .ok_or_else(|| DaemonError::Llm("missing choices[0].message".to_string()))?;

    let text = choice.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();

    let mut tool_calls = Vec::new();
    if let Some(calls) = choice.get("tool_calls").and_then(|c| c.as_array()) {
        for call in calls {
            let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name =
                call.pointer("/function/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let args = call
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Json::Null);
            tool_calls.push(ToolCall {
                id,
                name,
                arguments: args,
            });
        }
    }

    let usage = parsed.get("usage").map(parse_usage).unwrap_or_default();
    Ok(LlmResponse {
        text,
        tool_calls,
        usage,
    })
}

/// Parses an OpenAI usage object.
fn parse_usage(u: &Json) -> Usage {
    let input = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let reasoning = u
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: input + output,
    }
}

/// Merges a streaming tool call delta into the accumulated tool calls.
fn merge_tool_call(tool_calls: &mut Vec<ToolCall>, delta: &Json) {
    let index = delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    while tool_calls.len() <= index {
        tool_calls.push(ToolCall {
            id: String::new(),
            name: String::new(),
            arguments: Json::Null,
        });
    }
    let call = &mut tool_calls[index];
    if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
        call.id = id.to_string();
    }
    if let Some(name) = delta.pointer("/function/name").and_then(|v| v.as_str()) {
        call.name = name.to_string();
    }
    if let Some(args) = delta.pointer("/function/arguments").and_then(|v| v.as_str()) {
        let current = match &call.arguments {
            Json::String(s) => s.clone(),
            _ => String::new(),
        };
        let combined = format!("{current}{args}");
        call.arguments = serde_json::from_str(&combined).unwrap_or(Json::String(combined));
    }
}

/// Builds the tools array for a request (used by the ReAct loop).
pub(crate) fn build_tools(definitions: &[ToolDefinition]) -> Json {
    Json::Array(
        definitions
            .iter()
            .map(|d| {
                json!({
                    "type": "function",
                    "function": {
                        "name": d.name,
                        "description": d.description,
                        "parameters": d.parameters,
                    }
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_shared::llm::SystemFragment;

    #[test]
    fn builds_messages_with_system_and_tool() {
        let mut ctx = ContextManager::new_from_prompt(
            vec![SystemFragment {
                priority: 0,
                scope: "test".to_string(),
                content: "You are helpful.".to_string(),
            }],
            "hello",
        );
        ctx.mix_in_tool_result(metteur_shared::llm::ToolResult {
            tool_call_id: "call_1".to_string(),
            content: "42".to_string(),
            timestamp: 0,
            lifetime: metteur_shared::llm::ToolResultLifetime::OneShot,
        });
        let messages = build_messages(&ctx);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn parses_response_with_tool_call() {
        let json = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "ReadFile", "arguments": "{\"path\":\"a.txt\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        });
        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "ReadFile");
        assert_eq!(resp.usage.input_tokens, 10);
    }

    #[test]
    fn applies_params() {
        let mut body = json!({});
        let params = GenerationParams {
            temperature: Some(0.7),
            max_tokens: Some(100),
            reasoning_effort: Some(ReasoningEffort::High),
            ..Default::default()
        };
        apply_params(&mut body, &params, &GenerationParams::default());
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_completion_tokens"], 100);
        assert_eq!(body["reasoning_effort"], "high");
    }
}
