//! A mock LLM provider for testing.
//!
//! The mock returns a configurable sequence of responses (text, tool calls or
//! scripted failures) without any network access, enabling deterministic unit
//! and smoke tests — including retry and fallback paths.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use metteur_shared::llm::{ContextManager, GenerationParams, ToolCall, ToolDefinition, Usage};

use crate::error::{DaemonError, DaemonResult};

use super::client::{LlmClient, LlmResponse, StreamDelta};

/// A single scripted mock response.
#[derive(Debug, Clone)]
pub enum MockStep {
    /// Return the given text.
    Text(String),
    /// Reason through `thinking` (streamed as a reasoning delta) and then
    /// answer with `text`.
    Reasoning {
        /// The reasoning text.
        thinking: String,
        /// The answer text.
        text: String,
    },
    /// Return the given tool calls.
    Tools(Vec<ToolCall>),
    /// Return text alongside tool calls, as models usually do when they
    /// announce the call before making it.
    ToolsWithText {
        /// The assistant text.
        text: String,
        /// The requested tool calls.
        calls: Vec<ToolCall>,
    },
    /// Fail with a provider status code.
    Status(u16),
    /// Fail with a transport error.
    Transport(String),
}

/// A mock LLM client that replays a scripted sequence of responses.
pub struct MockClient {
    steps: Vec<MockStep>,
    cursor: Arc<AtomicUsize>,
    pre_delay: std::time::Duration,
}

impl MockClient {
    /// Creates a mock client that replays `steps` in order.
    pub fn new(steps: Vec<MockStep>) -> Self {
        Self::new_delayed(steps, std::time::Duration::ZERO)
    }

    /// Creates a mock client that sleeps `pre_delay` before each response.
    pub fn new_delayed(steps: Vec<MockStep>, pre_delay: std::time::Duration) -> Self {
        Self {
            steps,
            cursor: Arc::new(AtomicUsize::new(0)),
            pre_delay,
        }
    }

    /// Creates a mock client that always returns the given text.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![MockStep::Text(text.into())])
    }

    fn next(&self) -> MockStep {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.steps.get(idx).cloned().unwrap_or_else(|| MockStep::Text(String::new()))
    }
}

#[async_trait]
impl LlmClient for MockClient {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    async fn complete(
        &self,
        _ctx: &ContextManager,
        _params: &GenerationParams,
        _tools: &[ToolDefinition],
    ) -> DaemonResult<LlmResponse> {
        if !self.pre_delay.is_zero() {
            tokio::time::sleep(self.pre_delay).await;
        }
        match self.next() {
            MockStep::Text(text) => Ok(LlmResponse {
                text,
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: Usage::default(),
            }),
            MockStep::Reasoning {
                thinking,
                text,
            } => Ok(LlmResponse {
                text,
                thinking: vec![crate::llm::ThinkingBlock {
                    text: thinking,
                    ..Default::default()
                }],
                tool_calls: Vec::new(),
                usage: Usage::default(),
            }),
            MockStep::Tools(tool_calls) => Ok(LlmResponse {
                text: String::new(),
                thinking: Vec::new(),
                tool_calls,
                usage: Usage::default(),
            }),
            MockStep::ToolsWithText {
                text,
                calls,
            } => Ok(LlmResponse {
                text,
                thinking: Vec::new(),
                tool_calls: calls,
                usage: Usage::default(),
            }),
            MockStep::Status(status) => Err(DaemonError::LlmStatus {
                status,
                message: "scripted failure".to_string(),
            }),
            MockStep::Transport(message) => Err(DaemonError::LlmTransport(message)),
        }
    }

    async fn stream(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> DaemonResult<LlmResponse> {
        let resp = self.complete(ctx, params, tools).await?;
        for block in &resp.thinking {
            on_delta(StreamDelta::Reasoning(block.text.clone()));
        }
        on_delta(StreamDelta::Text(resp.text.clone()));
        Ok(resp)
    }
}
