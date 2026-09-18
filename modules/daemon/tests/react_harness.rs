//! Integration tests for the ReAct harness behaviors introduced in Round 13:
//! tool error tolerance, repetition detection, result truncation, parallel
//! read-only execution, and stale-read invalidation.
//!
//! The loop is driven through the `CallLLM` node path with the mock provider,
//! which scripts tool calls without an HTTP round trip.

use std::collections::HashMap;
use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::execution::react::{ReactOptions, run_react};
use metteur_daemon::llm::{LlmClientFactory, MockClient, MockStep};
use metteur_daemon::registry::{Registry, Tool};
use metteur_shared::llm::{ContextManager, Message, Role, ToolCall};
use metteur_shared::{ToolResultLifetime, Value};

use metteur_daemon::error::{DaemonError, DaemonResult};

/// A tool that always fails, for the error-tolerance tests.
struct AlwaysFails;

#[async_trait::async_trait]
impl Tool for AlwaysFails {
    fn name(&self) -> &str {
        "AlwaysFails"
    }

    fn description(&self) -> &str {
        "Always returns an error."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    async fn call(&self, _args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        Err(DaemonError::Execution("simulated failure".to_string()))
    }
}

/// A read-only tool returning a fixed payload, optionally very large.
struct FixedReader {
    payload: String,
}

#[async_trait::async_trait]
impl Tool for FixedReader {
    fn name(&self) -> &str {
        "FixedReader"
    }

    fn description(&self) -> &str {
        "Returns a fixed payload."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    fn max_result_bytes(&self) -> usize {
        // Explicit cap so the truncation test does not depend on config.
        64
    }

    async fn call(&self, _args: &[Value], _ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        Ok(Value::String(self.payload.clone()))
    }
}

/// A mutating tool that writes a file, for staleness tests.
struct Mutator;

#[async_trait::async_trait]
impl Tool for Mutator {
    fn name(&self) -> &str {
        "Mutator"
    }

    fn description(&self) -> &str {
        "Writes a fixed file."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    async fn call(&self, _args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let path = ctx.workspace_root.join("mutated.txt");
        std::fs::write(&path, "changed").unwrap();
        ctx.note_file_mutation(&path);
        Ok(Value::Bool(true))
    }
}

/// A read-only tool that declares it read `mutated.txt`.
struct ReadMutated;

#[async_trait::async_trait]
impl Tool for ReadMutated {
    fn name(&self) -> &str {
        "ReadMutated"
    }

    fn description(&self) -> &str {
        "Reads the mutation target."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn read_only(&self) -> bool {
        true
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    async fn call(&self, _args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let path = ctx.workspace_root.join("mutated.txt");
        ctx.note_read_paths([path]);
        Ok(Value::String("original".to_string()))
    }
}

/// Builds a context with the given tools registered.
fn context_with(root: &std::path::Path, tools: Vec<Arc<dyn Tool>>) -> ExecutionContext {
    let registry = Registry::with_builtins();
    for tool in tools {
        registry.register_tool(tool);
    }
    ExecutionContext::new(Arc::new(registry), LlmClientFactory::new(), root.to_path_buf())
}

/// Builds a call with the given name and arguments.
fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments,
    }
}

/// Runs the ReAct loop with a scripted mock client.
///
/// The factory is overridden so `ReactOptions` still carry a real provider
/// selection (the options are validated while building the client).
async fn run_with_steps(
    ctx: &mut ExecutionContext,
    steps: Vec<MockStep>,
    opts: ReactOptions,
) -> Result<metteur_daemon::execution::react::ReactOutcome, DaemonError> {
    let client: Arc<dyn metteur_daemon::llm::LlmClient> = Arc::new(MockClient::new(steps));
    ctx.llm_factory = LlmClientFactory::with_override(client);
    let context = ContextManager::new_from_prompt(vec![], "start");
    run_react(ctx, context, &opts).await
}

/// Scripted options that name a provider (overridden by the factory).
fn scripted() -> ReactOptions {
    ReactOptions {
        provider: "openai-chat".to_string(),
        model: Some("mock-model".to_string()),
        ..Default::default()
    }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-react-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[tokio::test]
async fn tool_failure_becomes_a_result_not_an_abort() {
    let root = temp_root("tolerant");
    let mut ctx = context_with(&root, vec![Arc::new(AlwaysFails)]);
    let steps = vec![
        MockStep::Tools(vec![call("AlwaysFails", serde_json::json!({}))]),
        MockStep::Text("recovered".to_string()),
    ];
    let outcome = run_with_steps(&mut ctx, steps, scripted()).await.unwrap();
    assert_eq!(outcome.text, "recovered");
    // The error text reached the model as a tool result.
    let tool_message = outcome
        .context
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("tool message present");
    assert!(tool_message.text_content().contains("Error:"), "{}", tool_message.text_content());
    assert!(tool_message.text_content().contains("simulated failure"));
}

#[tokio::test]
async fn consecutive_failures_abort_at_the_limit() {
    let root = temp_root("limit");
    let mut ctx = context_with(&root, vec![Arc::new(AlwaysFails)]);
    // More failing turns than the limit allows.
    let steps = (0..5)
        .map(|index| MockStep::Tools(vec![call("AlwaysFails", serde_json::json!({ "n": index }))]))
        .collect();
    let opts = ReactOptions {
        tool_error_limit: 2,
        ..scripted()
    };
    let error = run_with_steps(&mut ctx, steps, opts).await.unwrap_err();
    assert!(error.to_string().contains("consecutive tool failures"), "{error}");
}

#[tokio::test]
async fn repeated_identical_calls_abort_at_the_limit() {
    let root = temp_root("repeat");
    let mut ctx = context_with(&root, vec![Arc::new(FixedReader {
        payload: "data".to_string(),
    })]);
    // The same call four times over.
    let steps = (0..4)
        .map(|_| MockStep::Tools(vec![call("FixedReader", serde_json::json!({ "x": 1 }))]))
        .collect();
    let opts = ReactOptions {
        repeat_call_limit: 3,
        ..scripted()
    };
    let error = run_with_steps(&mut ctx, steps, opts).await.unwrap_err();
    assert!(error.to_string().contains("identical arguments"), "{error}");
}

#[tokio::test]
async fn repeated_calls_are_flagged_before_the_limit() {
    let root = temp_root("flag");
    let mut ctx = context_with(&root, vec![Arc::new(FixedReader {
        payload: "data".to_string(),
    })]);
    let steps = vec![
        MockStep::Tools(vec![call("FixedReader", serde_json::json!({ "x": 1 }))]),
        MockStep::Tools(vec![call("FixedReader", serde_json::json!({ "x": 1 }))]),
        MockStep::Text("done".to_string()),
    ];
    let outcome = run_with_steps(&mut ctx, steps, scripted()).await.unwrap();
    let flagged = outcome
        .context
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .any(|m| m.text_content().contains("identical"));
    assert!(flagged, "the second identical call must carry a warning");
    assert_eq!(outcome.text, "done");
}

#[tokio::test]
async fn oversized_results_are_truncated_to_the_tool_budget() {
    let root = temp_root("truncate");
    // 512 bytes against a 64-byte cap.
    let payload = "x".repeat(512);
    let mut ctx = context_with(&root, vec![Arc::new(FixedReader {
        payload,
    })]);
    let steps = vec![
        MockStep::Tools(vec![call("FixedReader", serde_json::json!({}))]),
        MockStep::Text("ok".to_string()),
    ];
    let outcome = run_with_steps(&mut ctx, steps, scripted()).await.unwrap();
    let tool_message = outcome
        .context
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("tool message present");
    let content = tool_message.text_content();
    assert!(content.len() <= 128, "truncated content still too long: {} bytes", content.len());
    assert!(content.contains("truncated"), "{content}");
}

#[tokio::test]
async fn read_only_tools_run_in_call_order() {
    let root = temp_root("parallel");
    let mut ctx = context_with(
        &root,
        vec![
            Arc::new(FixedReader {
                payload: "first".to_string(),
            }),
            Arc::new(ReadMutated),
        ],
    );
    let mut first = call("FixedReader", serde_json::json!({}));
    first.id = "call_a".to_string();
    let mut second = call("ReadMutated", serde_json::json!({}));
    second.id = "call_b".to_string();
    let steps = vec![
        MockStep::Tools(vec![first, second]),
        MockStep::Text("ok".to_string()),
    ];
    let outcome = run_with_steps(&mut ctx, steps, scripted()).await.unwrap();
    let results: Vec<String> = outcome
        .context
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text_content())
        .collect();
    // Results keep the order of the requests even though they ran together.
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], "first");
    assert_eq!(results[1], "original");
}

#[tokio::test]
async fn mutating_a_file_expires_earlier_reads() {
    let root = temp_root("stale");
    std::fs::write(root.join("mutated.txt"), "original").unwrap();
    let mut ctx = context_with(&root, vec![Arc::new(ReadMutated), Arc::new(Mutator)]);
    let steps = vec![
        MockStep::Tools(vec![call("ReadMutated", serde_json::json!({}))]),
        MockStep::Tools(vec![call("Mutator", serde_json::json!({}))]),
        MockStep::Text("done".to_string()),
    ];
    let outcome = run_with_steps(&mut ctx, steps, scripted()).await.unwrap();
    let messages: Vec<String> =
        outcome.context.messages.iter().map(|m| m.text_content()).collect();
    assert!(
        messages.iter().any(|text| text.contains("stale")),
        "the read made before the mutation must be marked stale: {messages:?}"
    );
}

#[tokio::test]
async fn stale_placeholders_can_be_disabled() {
    let root = temp_root("nostale");
    std::fs::write(root.join("mutated.txt"), "original").unwrap();
    let mut ctx = context_with(&root, vec![Arc::new(ReadMutated), Arc::new(Mutator)]);
    let steps = vec![
        MockStep::Tools(vec![call("ReadMutated", serde_json::json!({}))]),
        MockStep::Tools(vec![call("Mutator", serde_json::json!({}))]),
        MockStep::Text("done".to_string()),
    ];
    let opts = ReactOptions {
        stale_result_placeholders: false,
        ..scripted()
    };
    let outcome = run_with_steps(&mut ctx, steps, opts).await.unwrap();
    let messages: Vec<String> =
        outcome.context.messages.iter().map(|m| m.text_content()).collect();
    assert!(messages.iter().any(|text| text == "original"));
    assert!(!messages.iter().any(|text| text.contains("stale")));
}

#[tokio::test]
async fn eviction_bounds_the_retained_tool_results() {
    let root = temp_root("evict");
    let mut ctx = context_with(&root, vec![Arc::new(FixedReader {
        payload: "small".to_string(),
    })]);
    // Six turns, each with a distinct call (so repetition limits do not trip).
    let mut steps: Vec<MockStep> = (0..6)
        .map(|index| MockStep::Tools(vec![call("FixedReader", serde_json::json!({ "n": index }))]))
        .collect();
    steps.push(MockStep::Text("done".to_string()));
    let opts = ReactOptions {
        max_tool_results: 2,
        ..scripted()
    };
    let outcome = run_with_steps(&mut ctx, steps, opts).await.unwrap();
    assert!(outcome.context.tool_results.len() <= 2);
    // The conversation remains valid for the provider.
    let calls: Vec<&str> = outcome
        .context
        .messages
        .iter()
        .flat_map(|m| m.tool_calls.iter().map(|c| c.id.as_str()))
        .collect();
    let results: Vec<&str> = outcome
        .context
        .messages
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    for call_id in &calls {
        assert!(results.contains(call_id), "call {call_id} lost its response");
    }
}

#[tokio::test]
async fn allowed_tools_restrict_invocations() {
    let root = temp_root("allowed");
    let mut ctx = context_with(&root, vec![Arc::new(FixedReader {
        payload: "secret".to_string(),
    })]);
    let steps = vec![
        MockStep::Tools(vec![call("FixedReader", serde_json::json!({}))]),
        MockStep::Text("done".to_string()),
    ];
    let opts = ReactOptions {
        allowed_tools: Some(std::collections::HashSet::new()),
        ..scripted()
    };
    let outcome = run_with_steps(&mut ctx, steps, opts).await.unwrap();
    let tool_message = outcome
        .context
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("tool message present");
    assert!(tool_message.text_content().contains("not allowed"));
}

/// The mock client records the tool definitions it received so ordering can be
/// asserted; this test uses the production helper through the public API.
#[tokio::test]
async fn tool_definitions_are_sorted_for_a_stable_request_body() {
    let registry = Registry::with_builtins();
    let names: Vec<String> = registry.tools().iter().map(|t| t.name().to_string()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "the tool array must not depend on hash iteration order");
    // Sanity check on the registry surface this test relies on.
    let map: HashMap<String, usize> =
        registry.tool_names().into_iter().enumerate().map(|(i, n)| (n, i)).collect();
    assert!(map.contains_key("Grep"));
    let _: &Message = &Message::text(Role::User, "unused");
}
