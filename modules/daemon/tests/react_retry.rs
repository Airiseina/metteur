//! Integration tests for provider retry, backoff and model fallback.
//!
//! Failures are scripted through `MockStep::Status`/`MockStep::Transport`, and
//! multi-model behavior uses the per-model factory seam.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use metteur_daemon::error::{DaemonError, DaemonResult};
use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::execution::react::{ReactOptions, run_react, run_react_streaming};
use metteur_daemon::llm::{
    LlmClient, LlmClientFactory, LlmResponse, MockClient, MockStep, RetryPolicy, StreamDelta,
};
use metteur_daemon::registry::Registry;
use metteur_shared::config::{Config, LlmConfig, LlmModelConfig};
use metteur_shared::llm::{ContextManager, GenerationParams, ToolDefinition, Usage};

/// A client that emits one delta and then fails, to prove that a stream which
/// already reached the client is never replayed.
struct DeltaThenFail {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmClient for DeltaThenFail {
    fn provider(&self) -> &str {
        "test"
    }

    fn model(&self) -> &str {
        "delta-then-fail"
    }

    async fn complete(
        &self,
        _ctx: &ContextManager,
        _params: &GenerationParams,
        _tools: &[ToolDefinition],
    ) -> DaemonResult<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(DaemonError::LlmTransport("scripted transport failure".to_string()))
    }

    async fn stream(
        &self,
        _ctx: &ContextManager,
        _params: &GenerationParams,
        _tools: &[ToolDefinition],
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> DaemonResult<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        on_delta(StreamDelta::Text("partial".to_string()));
        Err(DaemonError::LlmTransport("scripted failure after output".to_string()))
    }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-retry-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A context configured with `llm` settings and no loaded workspace config.
fn configured(tag: &str, llm: LlmConfig) -> ExecutionContext {
    let mut ctx = ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        temp_root(tag),
    );
    ctx.config = Some(Arc::new(tokio::sync::RwLock::new(Config {
        llm,
        ..Default::default()
    })));
    ctx
}

fn options() -> ReactOptions {
    ReactOptions {
        provider: "openai-chat".to_string(),
        model: Some("primary".to_string()),
        ..Default::default()
    }
}

#[test]
fn retry_classification_covers_transient_failures_only() {
    for status in [408, 409, 425, 429, 500, 502, 503, 504, 529] {
        assert!(
            RetryPolicy::retryable(&DaemonError::LlmStatus {
                status,
                message: String::new()
            }),
            "status {status} should be retryable"
        );
    }
    for status in [400, 401, 403, 404, 422] {
        assert!(
            !RetryPolicy::retryable(&DaemonError::LlmStatus {
                status,
                message: String::new()
            }),
            "status {status} must not be retried"
        );
    }
    assert!(RetryPolicy::retryable(&DaemonError::LlmTransport("timeout".to_string())));
    assert!(!RetryPolicy::retryable(&DaemonError::Interrupted("cancelled".to_string())));
    assert!(!RetryPolicy::retryable(&DaemonError::Execution("no".to_string())));
}

#[test]
fn backoff_grows_with_the_attempt_and_respects_the_ceiling() {
    let policy = RetryPolicy {
        max_retries: 5,
        base_delay_ms: 100,
        max_delay_ms: 400,
        fallback_models: Vec::new(),
    };
    assert!(policy.delay_for(0) <= std::time::Duration::from_millis(100));
    assert!(policy.delay_for(1) <= std::time::Duration::from_millis(200));
    assert!(policy.delay_for(2) <= std::time::Duration::from_millis(400));
    // The ceiling clamps the exponential growth.
    for attempt in 3..12 {
        assert!(policy.delay_for(attempt) <= std::time::Duration::from_millis(400));
    }
    let zero = RetryPolicy {
        base_delay_ms: 0,
        ..policy.clone()
    };
    assert_eq!(zero.delay_for(3), std::time::Duration::ZERO);
}

#[tokio::test]
async fn a_transient_status_is_retried_and_then_succeeds() {
    let mut ctx = configured(
        "retry",
        LlmConfig {
            max_retries: 2,
            retry_base_delay_ms: 0,
            ..Default::default()
        },
    );
    // Two failures, then an answer: the retry budget covers both.
    let client: Arc<dyn LlmClient> = Arc::new(MockClient::new(vec![
        MockStep::Status(429),
        MockStep::Status(503),
        MockStep::Text("recovered".to_string()),
    ]));
    ctx.llm_factory = LlmClientFactory::with_override(client);
    let context = ContextManager::new_from_prompt(vec![], "go");
    let outcome = run_react(&mut ctx, context, &options()).await.unwrap();
    assert_eq!(outcome.text, "recovered");
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let mut ctx = configured(
        "no-retry",
        LlmConfig {
            max_retries: 3,
            retry_base_delay_ms: 0,
            ..Default::default()
        },
    );
    // A 400 is a request problem: repeating it would not help.
    let client: Arc<dyn LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Status(400), MockStep::Text("never".to_string())]));
    ctx.llm_factory = LlmClientFactory::with_override(client);
    let context = ContextManager::new_from_prompt(vec![], "go");
    let error = run_react(&mut ctx, context, &options()).await.unwrap_err();
    assert!(error.to_string().contains("400"), "{error}");
}

#[tokio::test]
async fn exhausted_retries_surface_the_last_error() {
    let mut ctx = configured(
        "exhausted",
        LlmConfig {
            max_retries: 1,
            retry_base_delay_ms: 0,
            ..Default::default()
        },
    );
    let client: Arc<dyn LlmClient> = Arc::new(MockClient::new(vec![
        MockStep::Status(500),
        MockStep::Status(500),
        MockStep::Text("never".to_string()),
    ]));
    ctx.llm_factory = LlmClientFactory::with_override(client);
    let context = ContextManager::new_from_prompt(vec![], "go");
    let error = run_react(&mut ctx, context, &options()).await.unwrap_err();
    assert!(error.to_string().contains("500"), "{error}");
}

#[tokio::test]
async fn a_streamed_failure_after_output_is_not_replayed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut ctx = configured(
        "streamed",
        LlmConfig {
            max_retries: 5,
            retry_base_delay_ms: 0,
            ..Default::default()
        },
    );
    ctx.llm_factory = LlmClientFactory::with_override(Arc::new(DeltaThenFail {
        calls: calls.clone(),
    }));
    let context = ContextManager::new_from_prompt(vec![], "go");
    let mut deltas = Vec::new();
    let mut on_delta = |delta: StreamDelta| deltas.push(delta);
    let error =
        run_react_streaming(&mut ctx, context, &options(), Some(&mut on_delta), &mut |_| {})
            .await
            .unwrap_err();
    assert!(matches!(error, (DaemonError::LlmTransport(_), _)));
    assert_eq!(deltas, vec![StreamDelta::Text("partial".to_string())]);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a stream that already produced output must not be retried"
    );
}

#[tokio::test]
async fn the_fallback_model_takes_over_when_retries_are_spent() {
    let mut ctx = configured(
        "fallback",
        LlmConfig {
            default_model: Some("primary".to_string()),
            max_retries: 0,
            fallback_models: vec!["secondary".to_string()],
            models: HashMap::from([
                (
                    "primary".to_string(),
                    LlmModelConfig {
                        model_id: "primary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
                (
                    "secondary".to_string(),
                    LlmModelConfig {
                        model_id: "secondary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        },
    );
    let primary: Arc<dyn LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Status(503), MockStep::Status(503)]));
    let secondary: Arc<dyn LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Text("from the backup".to_string())]));
    ctx.llm_factory = LlmClientFactory::with_model_overrides(HashMap::from([
        ("primary-id".to_string(), primary),
        ("secondary-id".to_string(), secondary),
    ]));
    let context = ContextManager::new_from_prompt(vec![], "go");
    let outcome = run_react(&mut ctx, context, &options()).await.unwrap();
    assert_eq!(outcome.text, "from the backup");
}

#[tokio::test]
async fn a_failing_fallback_chain_reports_the_error() {
    let mut ctx = configured(
        "chain",
        LlmConfig {
            default_model: Some("primary".to_string()),
            max_retries: 0,
            fallback_models: vec!["secondary".to_string()],
            models: HashMap::from([
                (
                    "primary".to_string(),
                    LlmModelConfig {
                        model_id: "primary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
                (
                    "secondary".to_string(),
                    LlmModelConfig {
                        model_id: "secondary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        },
    );
    // Both models keep failing: the run ends with a provider error rather than
    // bouncing the request between them forever.
    let failing =
        || -> Arc<dyn LlmClient> { Arc::new(MockClient::new(vec![MockStep::Status(500); 8])) };
    ctx.llm_factory = LlmClientFactory::with_model_overrides(HashMap::from([
        ("primary-id".to_string(), failing()),
        ("secondary-id".to_string(), failing()),
    ]));
    let context = ContextManager::new_from_prompt(vec![], "go");
    let error = run_react(&mut ctx, context, &options()).await.unwrap_err();
    assert!(error.to_string().contains("500"), "{error}");
}

#[tokio::test]
async fn fallback_never_reselects_the_active_model() {
    // The default model and the first fallback are the same entry: the chain
    // must move on instead of retrying the model that just failed.
    let mut ctx = configured(
        "same-model",
        LlmConfig {
            default_model: Some("primary".to_string()),
            max_retries: 0,
            fallback_models: vec!["primary".to_string(), "secondary".to_string()],
            models: HashMap::from([
                (
                    "primary".to_string(),
                    LlmModelConfig {
                        model_id: "primary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
                (
                    "secondary".to_string(),
                    LlmModelConfig {
                        model_id: "secondary-id".to_string(),
                        api_type: "openai-chat".to_string(),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        },
    );
    let primary: Arc<dyn LlmClient> = Arc::new(MockClient::new(vec![MockStep::Status(503); 4]));
    let secondary: Arc<dyn LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Text("second".to_string())]));
    ctx.llm_factory = LlmClientFactory::with_model_overrides(HashMap::from([
        ("primary-id".to_string(), primary),
        ("secondary-id".to_string(), secondary),
    ]));
    let opts = ReactOptions {
        model: Some("primary".to_string()),
        ..options()
    };
    let context = ContextManager::new_from_prompt(vec![], "go");
    let outcome = run_react(&mut ctx, context, &opts).await.unwrap();
    assert_eq!(outcome.text, "second");
    let _ = Usage::default();
}
