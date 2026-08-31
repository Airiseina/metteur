//! Anthropic Messages provider (`POST /messages`).

use async_trait::async_trait;
use metteur_shared::llm::{
    ContentBlock, ContextManager, GenerationParams, Message, Role, ToolCall, ToolDefinition, Usage,
};
use serde_json::{Value as Json, json};

use crate::error::{DaemonError, DaemonResult};

use super::super::client::{LlmClient, LlmProviderConfig, LlmResponse, http_error};

/// The Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A client for the Anthropic Messages API.
pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    default_params: GenerationParams,
}

impl AnthropicClient {
    /// Creates a new Anthropic client.
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
        let merged = merge_params(&self.default_params, params);
        let mut body = json!({
            "model": self.model,
            "messages": build_messages(ctx),
            "max_tokens": merged.max_tokens.unwrap_or(1024),
        });
        if !ctx.system_fragments.is_empty() {
            let system = ctx
                .system_fragments
                .iter()
                .map(|f| f.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            body["system"] = json!(system);
        }
        if let Some(v) = merged.temperature {
            body["temperature"] = json!(v);
        }
        if let Some(v) = merged.top_p {
            body["top_p"] = json!(v);
        }
        if !merged.stop.is_empty() {
            body["stop_sequences"] = json!(merged.stop);
        }
        if !tools.is_empty() {
            body["tools"] = build_tools(tools);
        }
        body
    }

    /// Sends the request and parses the response.
    async fn send(&self, body: Json) -> DaemonResult<LlmResponse> {
        let url = format!("{}/messages", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| http_error("anthropic request", e))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| http_error("anthropic response", e))?;
        if !status.is_success() {
            return Err(DaemonError::Llm(format!("anthropic returned {status}: {text}")));
        }
        let parsed: Json = serde_json::from_str(&text)
            .map_err(|e| DaemonError::Llm(format!("invalid anthropic response: {e}")))?;
        parse_response(&parsed)
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    fn provider(&self) -> &str {
        "anthropic"
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
        let url = format!("{}/messages", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| http_error("anthropic stream request", e))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.map_err(|e| http_error("anthropic stream response", e))?;
            return Err(DaemonError::Llm(format!("anthropic returned {status}: {text}")));
        }

        let mut stream = resp.bytes_stream();
        let mut text_out = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage = Usage::default();
        let mut buf = String::new();

        use tokio_stream::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| http_error("anthropic stream", e))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].to_string();
                buf = buf[pos + 1..].to_string();
                let line = line.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line[5..].trim();
                let event: Json = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match event.get("type").and_then(|t| t.as_str()) {
                    Some("content_block_delta") => {
                        if let Some(text) = event.pointer("/delta/text").and_then(|v| v.as_str()) {
                            text_out.push_str(text);
                            on_delta(text.to_string());
                        }
                        if let Some(input) =
                            event.pointer("/delta/partial_json").and_then(|v| v.as_str())
                            && let Some(last) = tool_calls.last_mut()
                        {
                            let current = match &last.arguments {
                                Json::String(s) => s.clone(),
                                _ => String::new(),
                            };
                            let combined = format!("{current}{input}");
                            last.arguments =
                                serde_json::from_str(&combined).unwrap_or(Json::String(combined));
                        }
                    }
                    Some("content_block_start") => {
                        if let Some(tool_use) = event.pointer("/content_block")
                            && tool_use.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        {
                            let id = tool_use
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tool_use
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments: Json::Null,
                            });
                        }
                    }
                    Some("message_delta") => {
                        if let Some(u) = event.get("usage") {
                            usage = parse_usage(u);
                        }
                    }
                    _ => {}
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

/// Converts a context into the Anthropic messages array.
fn build_messages(ctx: &ContextManager) -> Vec<Json> {
    ctx.messages.iter().map(message_to_json).collect()
}

/// Converts a shared message into an Anthropic message object.
fn message_to_json(msg: &Message) -> Json {
    match msg.role {
        Role::Tool => {
            // Anthropic represents tool results as a user message with a
            // `tool_result` content block.
            let content = msg
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text(t) => t.clone(),
                })
                .collect::<Vec<_>>()
                .join("");
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": content,
                }]
            })
        }
        Role::Assistant if !msg.tool_calls.is_empty() => {
            let mut content: Vec<Json> = msg
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text(t) => json!({ "type": "text", "text": t }),
                })
                .collect();
            for call in &msg.tool_calls {
                content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": call.arguments,
                }));
            }
            json!({ "role": "assistant", "content": content })
        }
        _ => {
            let content = msg
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text(t) => t.clone(),
                })
                .collect::<Vec<_>>()
                .join("");
            json!({ "role": role_str(msg.role), "content": content })
        }
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user",
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

/// Parses an Anthropic response into a shared response.
fn parse_response(parsed: &Json) -> DaemonResult<LlmResponse> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(content) = parsed.get("content").and_then(|c| c.as_array()) {
        for block in content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let args = block.get("input").cloned().unwrap_or(Json::Null);
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
                _ => {}
            }
        }
    }
    let usage = parsed.get("usage").map(parse_usage).unwrap_or_default();
    Ok(LlmResponse {
        text,
        tool_calls,
        usage,
    })
}

/// Parses an Anthropic usage object.
fn parse_usage(u: &Json) -> Usage {
    let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: 0,
        total_tokens: input + output,
    }
}

/// Builds the tools array for a request.
pub(crate) fn build_tools(definitions: &[ToolDefinition]) -> Json {
    Json::Array(
        definitions
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "description": d.description,
                    "input_schema": d.parameters,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_messages_with_tool_result() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.mix_in_tool_result(metteur_shared::llm::ToolResult {
            tool_call_id: "toolu_1".to_string(),
            content: "42".to_string(),
            timestamp: 0,
            lifetime: metteur_shared::llm::ToolResultLifetime::OneShot,
        });
        let messages = build_messages(&ctx);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn parses_response_with_tool_use() {
        let json = json!({
            "content": [
                { "type": "text", "text": "Let me check." },
                { "type": "tool_use", "id": "toolu_1", "name": "ReadFile", "input": { "path": "a.txt" } }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let resp = parse_response(&json).unwrap();
        assert_eq!(resp.text, "Let me check.");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "ReadFile");
        assert_eq!(resp.usage.input_tokens, 10);
    }

    #[test]
    fn requires_max_tokens() {
        let client = AnthropicClient::new(
            reqwest::Client::new(),
            &LlmProviderConfig::new(
                super::super::super::ProviderKind::Anthropic,
                "https://api.anthropic.com",
                "key",
                "claude-sonnet-4",
            ),
        );
        let ctx = ContextManager::new_from_prompt(vec![], "hi");
        let body = client.build_body(&ctx, &GenerationParams::default(), &[]);
        assert!(body["max_tokens"].as_u64().unwrap() > 0);
    }
}
