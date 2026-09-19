//! Permission modes: who answers an authorization question, and what that
//! means for commands and file writes.
//!
//! The interesting cases are the ones where a decision has to come from
//! somewhere other than the user: the sandbox's own verdict, or the model in
//! `full` mode. Those are exercised here with a scripted model, so the tests do
//! not depend on a provider.

use std::sync::Arc;

use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::llm::{LlmClientFactory, MockClient, MockStep};
use metteur_daemon::registry::Registry;
use metteur_daemon::sandbox::{PermissionMode, authorize, authorize_write};
use metteur_shared::config::{Config, SandboxConfig};

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-perm-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A context with `mode` and a sandbox that consults a whitelist.
fn context(tag: &str, mode: PermissionMode, enabled: bool) -> ExecutionContext {
    let mut ctx = ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        temp_root(tag),
    );
    ctx.config = Some(Arc::new(tokio::sync::RwLock::new(Config {
        sandbox: SandboxConfig {
            enabled,
            mode: mode.as_str().to_string(),
            whitelist: vec!["echo *".to_string()],
            ..Default::default()
        },
        llm: metteur_shared::config::LlmConfig {
            default_model: Some("demo".to_string()),
            models: std::collections::HashMap::from([(
                "demo".to_string(),
                metteur_shared::config::LlmModelConfig {
                    api_type: "openai-chat".to_string(),
                    api_endpoint: "http://localhost:1".to_string(),
                    model_id: "demo".to_string(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
        ..Default::default()
    })));
    ctx.permission_mode = mode;
    ctx
}

/// Attaches a scripted model that answers the reviewer with `answer`.
fn script_reviewer(ctx: &mut ExecutionContext, answer: &str) {
    let client: Arc<dyn metteur_daemon::llm::LlmClient> =
        Arc::new(MockClient::new(vec![MockStep::Text(answer.to_string())]));
    ctx.llm_factory = LlmClientFactory::with_override(client);
}

#[tokio::test]
async fn sandbox_mode_runs_workspace_writes_without_asking() {
    let ctx = context("write-inside", PermissionMode::Sandbox, true);
    let target = ctx.workspace_root.join("src").join("main.rs");
    assert!(authorize_write(&ctx, &target, "src/main.rs", "write 12 bytes").await.unwrap());
}

#[tokio::test]
async fn sandbox_mode_relies_on_the_policy_for_commands() {
    let ctx = context("command-whitelist", PermissionMode::Sandbox, true);
    // Whitelisted: runs without an approval channel.
    assert!(authorize(&ctx, "echo hello").await.unwrap());
}

#[tokio::test]
async fn full_mode_sends_risky_commands_to_the_reviewer() {
    let mut ctx = context("command-review", PermissionMode::Full, true);
    // The predictor calls an unknown program risky, so the reviewer decides.
    script_reviewer(&mut ctx, "APPROVE");
    assert!(authorize(&ctx, "terraform destroy").await.unwrap());

    let mut denying = context("command-review-deny", PermissionMode::Full, true);
    script_reviewer(&mut denying, "DENY: this touches infrastructure outside the workspace");
    assert!(!authorize(&denying, "terraform destroy").await.unwrap());
}

#[tokio::test]
async fn full_mode_reviews_writes_outside_the_workspace() {
    let mut ctx = context("write-outside", PermissionMode::Full, true);
    let outside = temp_root("elsewhere").join("notes.txt");
    script_reviewer(&mut ctx, "DENY");
    assert!(!authorize_write(&ctx, &outside, "../notes.txt", "write 4 bytes").await.unwrap());
}

#[tokio::test]
async fn an_unavailable_reviewer_does_not_grant_a_denial() {
    // No model is configured for the reviewer, so the verdict is unknown. A
    // write inside the workspace still runs (the jail bounds it), which is the
    // documented fallback.
    let mut ctx = context("no-reviewer", PermissionMode::Full, true);
    ctx.llm_factory = LlmClientFactory::new();
    let target = ctx.workspace_root.join("a.txt");
    assert!(authorize_write(&ctx, &target, "a.txt", "write 1 byte").await.unwrap());
}

#[tokio::test]
async fn ask_mode_still_runs_writes_without_an_approval_channel() {
    // Nothing can ask (no client attached), so the write proceeds: refusing
    // would make an unattended run useless, and the jail still applies.
    let ctx = context("ask", PermissionMode::Ask, true);
    let target = ctx.workspace_root.join("a.txt");
    assert!(authorize_write(&ctx, &target, "a.txt", "write 1 byte").await.unwrap());
}
