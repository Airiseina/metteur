//! A mock LLM provider for testing.
//!
//! The mock returns a configurable sequence of responses (text or tool calls)
//! without any network access, enabling deterministic unit and smoke tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use metteur_shared::llm::{ContextManager, GenerationParams, ToolCall, ToolDefinition, Usage};

use crate::error::DaemonResult;

use super::client::{LlmClient, LlmResponse};

/// A single scripted mock response.
#[derive(Debug, Clone)]
pub enum MockStep {
    /// Return the given text.
    Text(String),
    /// Return the given tool calls.
    Tools(Vec<ToolCall>),
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
        Ok(match self.next() {
            MockStep::Text(text) => LlmResponse {
                text,
                tool_calls: Vec::new(),
                usage: Usage::default(),
            },
            MockStep::Tools(tool_calls) => LlmResponse {
                text: String::new(),
                tool_calls,
                usage: Usage::default(),
            },
        })
    }

    async fn stream(
        &self,
        ctx: &ContextManager,
        params: &GenerationParams,
        tools: &[ToolDefinition],
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> DaemonResult<LlmResponse> {
        let resp = self.complete(ctx, params, tools).await?;
        on_delta(resp.text.clone());
        Ok(resp)
    }
}
