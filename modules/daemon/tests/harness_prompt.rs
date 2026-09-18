//! Tests for harness prompt assembly: section ordering, the environment block,
//! project instruction files and the prefix-stability contract of `refresh`.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::harness::{HarnessPrompt, SCOPE_PREFIX};
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::Registry;
use metteur_shared::config::{Config, LlmConfig};
use metteur_shared::llm::{ContextManager, Message, Role, SystemFragment};

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

/// Renders the fragments the way a provider would, for order assertions.
fn render(fragments: &[SystemFragment]) -> Vec<String> {
    let context = ContextManager {
        system_fragments: fragments.to_vec(),
        ..Default::default()
    };
    context.ordered_fragments().into_iter().map(|f| f.scope.clone()).collect()
}

#[tokio::test]
async fn sections_render_in_priority_order() {
    let root = temp_root("order");
    let ctx = context_for(&root, Config::default());
    let fragments = HarnessPrompt::fragments(&ctx).await;
    let scopes = render(&fragments);
    assert_eq!(
        scopes,
        vec![
            "harness.identity",
            "harness.tools",
            "harness.coding",
            "harness.plan",
            "harness.environment",
        ]
    );
    assert!(fragments.iter().all(|f| f.scope.starts_with(SCOPE_PREFIX)));
    assert!(fragments.iter().all(|f| !f.content.trim().is_empty()));
}

#[test]
fn environment_block_reports_the_workspace_without_volatile_content() {
    let root = temp_root("env");
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    let environment =
        fragments.iter().find(|f| f.scope == "harness.environment").expect("environment fragment");
    assert!(environment.content.contains(&root.display().to_string()));
    assert!(environment.content.contains(std::env::consts::OS));
    assert!(
        environment.content.contains(&chrono::Utc::now().format("%Y-%m-%d").to_string()),
        "{}",
        environment.content
    );
    // The same inputs render the same text: nothing volatile is embedded.
    let again = HarnessPrompt::fragments_for(&root, &Config::default());
    assert_eq!(
        environment.content,
        again.iter().find(|f| f.scope == "harness.environment").unwrap().content
    );
}

#[test]
fn the_configured_model_is_named_in_the_environment() {
    let root = temp_root("model");
    let config = Config {
        llm: LlmConfig {
            default_model: Some("deepseek".to_string()),
            models: std::collections::HashMap::from([(
                "deepseek".to_string(),
                metteur_shared::config::LlmModelConfig {
                    api_type: "anthropic".to_string(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &config);
    let environment = fragments.iter().find(|f| f.scope == "harness.environment").unwrap();
    assert!(environment.content.contains("model: deepseek (anthropic)"), "{}", environment.content);
}

#[test]
fn project_instructions_load_from_metteur_md_first() {
    let root = temp_root("project");
    std::fs::write(root.join("AGENTS.md"), "agents rules").unwrap();
    std::fs::write(root.join("METTEUR.md"), "metteur rules").unwrap();
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    let project =
        fragments.iter().find(|f| f.scope == "harness.project").expect("project fragment");
    assert!(project.content.contains("metteur rules"));
    assert!(project.content.contains("source=\"METTEUR.md\""));
    assert!(!project.content.contains("agents rules"));
}

#[test]
fn project_instructions_fall_back_and_can_be_disabled() {
    let root = temp_root("project-fallback");
    std::fs::write(root.join("AGENTS.md"), "agents rules").unwrap();
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    let project = fragments.iter().find(|f| f.scope == "harness.project").unwrap();
    assert!(project.content.contains("agents rules"));

    let disabled = Config {
        llm: LlmConfig {
            project_instructions: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &disabled);
    assert!(!fragments.iter().any(|f| f.scope == "harness.project"));
}

#[test]
fn project_instructions_are_truncated_at_the_byte_cap() {
    let root = temp_root("project-cap");
    std::fs::write(root.join("METTEUR.md"), "z".repeat(500)).unwrap();
    let config = Config {
        llm: LlmConfig {
            project_instructions_max_bytes: 64,
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &config);
    let project = fragments.iter().find(|f| f.scope == "harness.project").unwrap();
    assert!(project.content.contains("[truncated]"));
    assert!(project.content.len() < 300, "{}", project.content.len());
}

#[test]
fn system_prompt_append_renders_last() {
    let root = temp_root("append");
    let config = Config {
        llm: LlmConfig {
            system_prompt_append: Some("Always answer in Chinese.".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &config);
    let scopes = render(&fragments);
    assert_eq!(scopes.last().map(String::as_str), Some("harness.append"));
}

#[test]
fn refresh_replaces_harness_fragments_and_keeps_foreign_ones() {
    let root = temp_root("refresh");
    let mut context = ContextManager::new_from_prompt(
        vec![
            SystemFragment {
                priority: 0,
                scope: "addon.custom".to_string(),
                content: "addon text".to_string(),
            },
            SystemFragment {
                priority: 50,
                scope: "harness.project".to_string(),
                content: "stale project rules".to_string(),
            },
        ],
        "hi",
    );
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    assert!(HarnessPrompt::refresh(&mut context, fragments.clone()));
    assert!(context.system_fragments.iter().any(|f| f.scope == "addon.custom"));
    assert!(!context.system_fragments.iter().any(|f| f.content == "stale project rules"));
    assert!(context.system_fragments.iter().any(|f| f.scope == "harness.identity"));

    // A second refresh with identical content must not touch the context: the
    // stored prefix has to stay byte-identical for the provider cache.
    let before = context.system_fragments.clone();
    assert!(!HarnessPrompt::refresh(&mut context, fragments));
    assert_eq!(context.system_fragments, before);
}

#[test]
fn refresh_detects_new_project_instructions() {
    let root = temp_root("refresh-project");
    let mut context = ContextManager::new_from_prompt(vec![], "hi");
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    assert!(HarnessPrompt::refresh(&mut context, fragments));

    std::fs::write(root.join("METTEUR.md"), "new rule: use tabs").unwrap();
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    assert!(HarnessPrompt::refresh(&mut context, fragments));
    assert!(
        context.system_fragments.iter().any(|f| f.content.contains("use tabs")),
        "editing the project file must reach the next prompt"
    );
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

#[test]
fn project_instruction_names_cannot_escape_the_workspace() {
    let root = temp_root("project-escape");
    std::fs::write(root.join("METTEUR.md"), "workspace rules").unwrap();
    // A repository can write its own configuration; a traversal attempt must
    // be ignored instead of pulling an arbitrary file into the prompt.
    let config = Config {
        llm: LlmConfig {
            project_instruction_files: vec![
                "../outside.md".to_string(),
                "sub/dir.md".to_string(),
                "..".to_string(),
                "METTEUR.md".to_string(),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &config);
    let project = fragments.iter().find(|f| f.scope == "harness.project").unwrap();
    assert!(project.content.contains("workspace rules"));
    assert!(project.content.contains("source=\"METTEUR.md\""));

    // With only escaping names the fragment disappears entirely.
    let hostile = Config {
        llm: LlmConfig {
            project_instruction_files: vec!["../outside.md".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let fragments = HarnessPrompt::fragments_for(&root, &hostile);
    assert!(!fragments.iter().any(|f| f.scope == "harness.project"));
}

#[test]
fn refresh_normalizes_drifted_fragment_priorities() {
    // A session stored by an older prompt revision may carry the same text
    // with a different priority; the rendered prompt would interleave
    // differently with addon fragments, so the refresh must not skip it.
    let root = temp_root("refresh-priority");
    let mut context = ContextManager::new_from_prompt(
        vec![
            SystemFragment {
                priority: 0,
                scope: "harness.identity".to_string(),
                content: metteur_daemon::harness::sections::IDENTITY.trim_end().to_string(),
            },
            SystemFragment {
                priority: 55,
                scope: "addon.custom".to_string(),
                content: "addon text".to_string(),
            },
        ],
        "hi",
    );
    let fragments = HarnessPrompt::fragments_for(&root, &Config::default());
    assert!(HarnessPrompt::refresh(&mut context, fragments.clone()));
    let identity = context
        .system_fragments
        .iter()
        .find(|f| f.scope == "harness.identity")
        .expect("identity fragment");
    assert_eq!(identity.priority, metteur_daemon::harness::sections::PRIORITY_IDENTITY);
    // A second refresh is a no-op again.
    assert!(!HarnessPrompt::refresh(&mut context, fragments));
}
