//! LLM client abstraction and provider factory.

use std::sync::Arc;

use async_trait::async_trait;
use metteur_shared::llm::{ContextManager, GenerationParams, ToolCall, ToolDefinition, Usage};

use crate::error::{DaemonError, DaemonResult};

/// The result of a single LLM completion.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The generated text (empty if the model only made tool calls).
    pub text: String,
    /// Tool calls requested by the model.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage for the request.
    pub usage: Usage,
}

/// A client for a single LLM provider and model.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// The provider name (e.g. `openai-chat`, `anthropic`, `openai-responses`).
    fn provider(&self) -> &str;

    /// The model identifier.
    fn model(&self) -> &str;

    /// Performs a non-streaming completion.
    async fn complete(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
    ) -> DaemonResult<LlmResponse>;

    /// Performs a streaming completion, invoking `on_delta` for each text
    /// delta as it arrives.
    async fn stream(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> DaemonResult<LlmResponse>;
}

/// The kind of LLM provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI Chat Completions (`/chat/completions`).
    OpenAiChat,
    /// Anthropic Messages (`/messages`).
    Anthropic,
    /// OpenAI Responses (`/responses`).
    OpenAiResponses,
}

/// Configuration for a single LLM provider.
#[derive(Debug, Clone)]
pub struct LlmProviderConfig {
    /// The provider kind.
    pub kind: ProviderKind,
    /// The base URL (without the API path).
    pub base_url: String,
    /// The API key.
    pub api_key: String,
    /// The model identifier.
    pub model: String,
    /// Default generation parameters applied to every request.
    pub default_params: GenerationParams,
}

impl LlmProviderConfig {
    /// Creates a config from a provider kind, base URL, API key and model.
    pub fn new(
        kind: ProviderKind,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            default_params: GenerationParams::default(),
        }
    }
}

/// Creates [`LlmClient`] instances for the supported providers.
#[derive(Clone)]
pub struct LlmClientFactory {
    http: reqwest::Client,
}

impl LlmClientFactory {
    /// Creates a factory with a shared HTTP client.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// Creates a client for the given provider config.
    pub fn create(&self, config: &LlmProviderConfig) -> DaemonResult<Arc<dyn LlmClient>> {
        let client: Arc<dyn LlmClient> = match config.kind {
            ProviderKind::OpenAiChat => Arc::new(
                crate::llm::provider::openai_chat::OpenAiChatClient::new(self.http.clone(), config),
            ),
            ProviderKind::Anthropic => Arc::new(
                crate::llm::provider::anthropic::AnthropicClient::new(self.http.clone(), config),
            ),
            ProviderKind::OpenAiResponses => {
                Arc::new(crate::llm::provider::openai_responses::OpenAiResponsesClient::new(
                    self.http.clone(),
                    config,
                ))
            }
        };
        Ok(client)
    }
}

impl Default for LlmClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps an HTTP error into a daemon error.
pub(crate) fn http_error(context: &str, err: reqwest::Error) -> DaemonError {
    DaemonError::Llm(format!("{context}: {err}"))
}
