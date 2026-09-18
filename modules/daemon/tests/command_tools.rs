//! Integration tests for the command tools: blocking execution, the
//! start/status/wait/kill job lifecycle, timeouts and cancellation.

use std::sync::Arc;

use metteur_daemon::error::DaemonError;
use metteur_daemon::execution::context::ExecutionContext;
use metteur_daemon::execution::jobs::JobState;
use metteur_daemon::llm::LlmClientFactory;
use metteur_daemon::registry::tools::command::{ExecuteCommand, JobStatus, KillJob, StartCommand, WaitJob};
use metteur_daemon::registry::{Registry, Tool};
use metteur_shared::config::{Config, ExecutionConfig, SandboxConfig};
use metteur_shared::Value;

/// A command staying alive for roughly `seconds` (see `command_jobs`).
fn busy_for(seconds: u32) -> String {
    if cfg!(windows) {
        format!("ping -n {} 127.0.0.1 > nul", seconds + 1)
    } else {
        format!("sleep {seconds}")
    }
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("metteur-cmd-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A context whose sandbox is disabled, so commands run without approvals.
fn context(tag: &str) -> ExecutionContext {
    let mut ctx = ExecutionContext::new(
        Arc::new(Registry::with_builtins()),
        LlmClientFactory::new(),
        temp_root(tag),
    );
    ctx.config = Some(Arc::new(tokio::sync::RwLock::new(Config {
        sandbox: SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        execution: ExecutionConfig {
            job_tail_lines: 40,
            ..Default::default()
        },
        ..Default::default()
    })));
    ctx
}

fn json_arg(command: &str, timeout: Option<i64>) -> Vec<Value> {
    let mut args = serde_json::json!({ "command": command });
    if let Some(timeout) = timeout {
        args["timeout_secs"] = serde_json::json!(timeout);
    }
    vec![Value::Json(args)]
}

/// Extracts the job id from a `StartCommand` payload.
fn job_id_of(value: &Value) -> String {
    payload(value)["job_id"].as_str().expect("the payload names the job").to_string()
}

/// Waits until every job of the run reached a terminal state.
async fn wait_all_finished(ctx: &ExecutionContext) {
    for job in ctx.jobs.list(Some(ctx.run_id)) {
        if job.state.is_running() {
            let _ = ctx.jobs.wait(&job.id).await;
        }
    }
}

fn payload(value: &Value) -> serde_json::Value {
    match value {
        Value::Json(json) => json.clone(),
        other => panic!("expected a JSON payload, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_command_waits_for_the_build_to_finish() {
    let mut ctx = context("blocking");
    let result = ExecuteCommand
        .call(&json_arg("echo blocking-done", None), &mut ctx)
        .await
        .unwrap();
    let payload = payload(&result);
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["state"], "exited");
    let output = payload["output"].as_str().unwrap();
    assert!(output.contains("blocking-done"), "{output}");
    // The command is tracked as a job even though the tool blocked.
    assert_eq!(ctx.jobs.len(), 1);
}

#[tokio::test]
async fn execute_command_reports_a_non_zero_exit() {
    let mut ctx = context("nonzero");
    let result = ExecuteCommand.call(&json_arg("exit 7", None), &mut ctx).await.unwrap();
    assert_eq!(payload(&result)["exit_code"], 7);
}

#[tokio::test]
async fn a_timeout_terminates_the_command() {
    let mut ctx = context("timeout");
    let error = ExecuteCommand
        .call(&json_arg(&busy_for(30), Some(1)), &mut ctx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    // The process must not be left behind.
    wait_all_finished(&ctx).await;
    assert!(!ctx.jobs.any_running(ctx.run_id));
}

#[tokio::test]
async fn cancelling_a_run_aborts_a_blocking_command() {
    let mut ctx = context("cancel");
    let cancel = Arc::clone(&ctx.cancel_requested);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let error = ExecuteCommand
        .call(&json_arg(&busy_for(30), None), &mut ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, DaemonError::Interrupted(_)), "{error}");
    wait_all_finished(&ctx).await;
    assert!(!ctx.jobs.any_running(ctx.run_id), "the cancelled command is killed");
}

#[tokio::test]
async fn start_status_wait_round_trip() {
    let mut ctx = context("roundtrip");
    let started = StartCommand.call(&json_arg("echo job-output", None), &mut ctx).await.unwrap();
    let job_id = job_id_of(&started);
    assert_eq!(payload(&started)["state"], "running");

    // Status works while the job runs or after it ended.
    let status = JobStatus
        .call(&[Value::Json(serde_json::json!({ "job_id": job_id }))], &mut ctx)
        .await
        .unwrap();
    let Value::String(status) = status else {
        panic!("expected a status line");
    };
    assert!(status.contains(&job_id), "{status}");

    let waited = WaitJob
        .call(&[Value::Json(serde_json::json!({ "job_id": job_id }))], &mut ctx)
        .await
        .unwrap();
    let payload = payload(&waited);
    assert_eq!(payload["exit_code"], 0);
    assert!(payload["output"].as_str().unwrap().contains("job-output"));

    // Listing without an id reports this run's jobs.
    let listed = JobStatus.call(&[Value::Json(serde_json::json!({}))], &mut ctx).await.unwrap();
    let Value::String(listed) = listed else {
        panic!("expected a job list");
    };
    assert!(listed.contains(&job_id), "{listed}");
}

#[tokio::test]
async fn a_wait_timeout_leaves_the_job_running() {
    let mut ctx = context("wait-timeout");
    let started = StartCommand
        .call(&json_arg(&busy_for(30), None), &mut ctx)
        .await
        .unwrap();
    let job_id = job_id_of(&started);

    let waited = WaitJob
        .call(
            &[Value::Json(serde_json::json!({ "job_id": job_id, "timeout_secs": 1 }))],
            &mut ctx,
        )
        .await
        .unwrap();
    let payload = payload(&waited);
    assert_eq!(payload["state"], "running");
    assert!(payload["note"].as_str().unwrap_or_default().contains("still running"));
    assert!(ctx.jobs.any_running(ctx.run_id), "a wait timeout must not kill the job");


    KillJob
        .call(&[Value::Json(serde_json::json!({ "job_id": job_id }))], &mut ctx)
        .await
        .unwrap();
    let final_state = ctx.jobs.wait(&job_id).await.unwrap();
    assert_eq!(final_state.state, JobState::Killed);
}

#[tokio::test]
async fn jobs_of_another_run_cannot_be_controlled() {
    let mut ctx = context("foreign");
    // A job started by a different run in the same workspace.
    let foreign = ctx
        .jobs
        .start("echo other-run", &ctx.workspace_root.clone(), uuid::Uuid::new_v4(), 4096)
        .unwrap();

    for (tool, name) in [
        (&JobStatus as &dyn Tool, "JobStatus"),
        (&WaitJob as &dyn Tool, "WaitJob"),
        (&KillJob as &dyn Tool, "KillJob"),
    ] {
        let error = tool
            .call(&[Value::Json(serde_json::json!({ "job_id": foreign }))], &mut ctx)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("belongs to another run"),
            "{name} accepted a foreign job: {error}"
        );
    }
}

#[tokio::test]
async fn unknown_job_ids_are_reported() {
    let mut ctx = context("unknown");
    let error = WaitJob
        .call(&[Value::Json(serde_json::json!({ "job_id": "deadbeef" }))], &mut ctx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
    assert!(error.to_string().contains("restart"), "the message explains why: {error}");
}

#[tokio::test]
async fn kill_without_an_id_terminates_this_runs_jobs() {
    let mut ctx = context("kill-all");
    let _first = StartCommand.call(&json_arg(&busy_for(30), None), &mut ctx).await.unwrap();
    let _second = StartCommand.call(&json_arg(&busy_for(30), None), &mut ctx).await.unwrap();

    let killed = KillJob.call(&[Value::Json(serde_json::json!({}))], &mut ctx).await.unwrap();
    let Value::String(message) = killed else {
        panic!("expected a summary");
    };
    assert!(message.contains("2 running job(s)"), "{message}");
    // The kill request is asynchronous: the processes end shortly after.
    wait_all_finished(&ctx).await;
    assert!(!ctx.jobs.any_running(ctx.run_id));
    assert_eq!(ctx.jobs.kill_owned_by(ctx.run_id), 0, "killing twice is a no-op");
}

#[tokio::test]
async fn job_ids_are_unique_and_short() {
    let mut ctx = context("ids");
    let mut ids = Vec::new();
    for _ in 0..3 {
        let started =
            StartCommand.call(&json_arg("echo id", None), &mut ctx).await.unwrap();
        ids.push(job_id_of(&started));
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "every job gets its own id");
    assert!(ids.iter().all(|id| id.len() == 8), "{ids:?}");
}
