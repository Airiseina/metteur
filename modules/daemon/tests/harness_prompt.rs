//! Harness integration in the daemon: the assembled prompt must reach the
//! contexts the engine actually builds.
//!
//! Pure assembly behaviour (ordering, project files, refresh semantics) is
//! covered by `metteur-harness`'s own tests; the tests here only assert that
//! the daemon wires the harness into its run contexts.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::harness;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::Registry;
use metteur_shared::config::Config;
use metteur_shared::llm::{ContextManager, Message, Role};

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-harness-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn context_for(root: &std::path::Path, config: Config) -> ExecutionContext {
    let mut ctx = ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        root.to_path_buf(),
    );
    ctx.config = Some(Arc::new(tokio::sync::RwLock::new(config)));
    ctx
}

#[tokio::test]
async fn start_node_context_carries_the_harness() {
    // The Start node builds the initial context that flows through a blueprint.
    use metteur_shared::{DataType, Node, NodeType, Pin, PinId, PinType, Value};

    let root = temp_root("start");
    let mut ctx = context_for(&root, Config::default());
    let pin = Pin {
        id: uuid::Uuid::new_v4(),
        name: "Context".to_string(),
        pin_type: PinType::DataOutput,
        data_type: DataType::Context,
        ..Default::default()
    };
    let node = Node {
        id: uuid::Uuid::new_v4(),
        node_type: NodeType::Event,
        kind: "Start".to_string(),
        position: (0.0, 0.0),
        pins: vec![pin.clone()],
        data: serde_json::json!({}),
    };
    let registry = ctx.registry.clone();
    let executor = registry.node_executor("Start").expect("Start executor");
    let outputs = executor
        .execute(&node, &std::collections::HashMap::<PinId, Value>::new(), &mut ctx)
        .await
        .unwrap();
    let context =
        outputs.get(&pin.id).and_then(|value| value.as_context()).expect("context output");
    assert!(context.system_fragments.iter().any(|f| f.scope == "harness.identity"));
    assert!(context.system_fragments.iter().any(|f| f.scope == "harness.env.base"));
}

#[tokio::test]
async fn environment_fragment_matches_the_harness_environment() {
    let root = temp_root("env-fragment");
    let ctx = context_for(&root, Config::default());
    let standalone = harness::environment_fragment(&ctx).await;
    let full = harness::fragments(&ctx).await;
    let from_harness = full
        .iter()
        .find(|f| f.scope == "harness.env.base")
        .expect("environment fragment");
    assert_eq!(&standalone, from_harness);
}

#[tokio::test]
async fn resume_note_carries_an_existing_plan() {
    // A resumed run must be reminded of its plan; the notice is appended as a
    // user message so it cannot disturb the cached system prefix.
    use metteur_daemon::execution::react::{ReactOptions, run_react};
    use metteur_daemon::llm::{MockClient, MockStep};
    use metteur_shared::llm::TodoItem;

    let root = temp_root("plan-note");
    let mut ctx = context_for(&root, Config::default());
    ctx.todos = vec![TodoItem {
        content: "ship it".to_string(),
        status: metteur_shared::llm::TodoStatus::InProgress,
        active_form: None,
    }];
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.events = Some(tx);
    let client: Arc<dyn metteur_daemon::llm::LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Text("done".to_string())]));
    ctx.llm_factory = LlmClientFactory::with_override(client);
    let opts = ReactOptions {
        provider: "openai-chat".to_string(),
        model: Some("mock-model".to_string()),
        ..Default::default()
    };
    let context = ContextManager::new_from_prompt(vec![], "continue");
    let outcome = run_react(&mut ctx, context, &opts).await.unwrap();
    let texts: Vec<String> = outcome.context.messages.iter().map(|m| m.text_content()).collect();
    assert!(texts.iter().any(|text| text.contains("[engine] current plan")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("ship it")));
    assert_eq!(outcome.context.messages[0].role, Role::User);
    let _ = Message::text(Role::User, "unused");
}
