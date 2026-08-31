//! Command execution sandbox with whitelists and a six-level approval flow.

pub mod approval;
pub mod grant;
pub mod policy;
pub mod predict;

use std::hash::Hasher;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use metteur_shared::config::SandboxConfig;

use crate::error::DaemonResult;
use crate::execution::ExecutionEvent;
use crate::execution::context::ExecutionContext;

use approval::Decision;
use grant::GrantStore;

/// Default seconds before an unanswered approval request is denied.
const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 300;

/// Resolves the final allow/deny verdict for running `command`.
///
/// The check order is: persistent deny grants, run-scoped grants, policy
/// whitelist, persistent allow grants, then the interactive approval flow.
/// When no approval channel is attached (e.g. unit tests), non-whitelisted
/// commands fail closed.
pub async fn authorize(ctx: &ExecutionContext, command: &str) -> DaemonResult<bool> {
    let sandbox_cfg = match &ctx.config {
        Some(config) => config.read().await.sandbox.clone(),
        None => SandboxConfig::default(),
    };
    if !sandbox_cfg.enabled {
        return Ok(true);
    }

    let hash = command_hash(&policy::normalize(command));
    let grants = GrantStore::new(ctx.workspace_db.clone(), ctx.global_db.clone());

    // 1. A persisted deny overrides everything, including the whitelist.
    if grants.lookup(hash) == Some(false) {
        ctx.audit("sandbox.denied", serde_json::json!({ "command": command }));
        return Ok(false);
    }

    // 2. Run-scoped grants from earlier decisions in this execution.
    if let Some(allow) = ctx.approvals.as_ref().and_then(|b| b.run_grant(hash)) {
        if !allow {
            ctx.audit("sandbox.denied", serde_json::json!({ "command": command }));
        }
        return Ok(allow);
    }

    // 3. Whitelisted commands run directly.
    if policy::classify(command, &sandbox_cfg) == policy::PolicyVerdict::Allowed {
        return Ok(true);
    }

    // 4. A persisted allow covers unclassified commands.
    if grants.lookup(hash) == Some(true) {
        return Ok(true);
    }

    // 5. Ask the user through the approval channel attached to this run.
    let Some(broker) = &ctx.approvals else {
        ctx.audit(
            "sandbox.approval",
            serde_json::json!({ "command": command, "outcome": "denied", "reason": "no approval channel" }),
        );
        return Ok(false);
    };

    let impact = predict::predict(command);
    let detail = serde_json::json!({
        "tool": "ExecuteCommand",
        "node_id": ctx.current_node.to_string(),
        "command": command,
        "impact": {
            "program": impact.program,
            "affected_paths": impact.affected_paths,
            "risk": impact.risk.as_str(),
        },
    });

    let (request_id, rx) = broker.open_request(hash, command.to_string());
    if let Some(events) = &ctx.events {
        let _ = events.send(ExecutionEvent::ApprovalRequested {
            node_id: ctx.current_node,
            request_id: request_id.clone(),
            detail: detail.to_string(),
        });
    }
    ctx.audit(
        "sandbox.approval",
        serde_json::json!({ "command": command, "request_id": request_id, "outcome": "pending" }),
    );

    let timeout_secs = if sandbox_cfg.approval_timeout_secs > 0 {
        sandbox_cfg.approval_timeout_secs
    } else {
        DEFAULT_APPROVAL_TIMEOUT_SECS
    };
    let outcome = tokio::select! {
        response = rx => response.unwrap_or(Decision::Deny),
        _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => Decision::Deny,
        _ = wait_cancelled(ctx.cancel_requested.clone()) => Decision::Deny,
    };

    let allow = outcome == Decision::Allow;
    broker.record_run_grant(hash, allow);
    if let Some(metrics) = &ctx.metrics {
        use std::sync::atomic::Ordering;
        if allow {
            metrics.sandbox_approvals_allowed.fetch_add(1, Ordering::Relaxed);
        } else {
            metrics.sandbox_approvals_denied.fetch_add(1, Ordering::Relaxed);
        }
    }
    ctx.audit(
        "sandbox.approval",
        serde_json::json!({
            "command": command,
            "request_id": request_id,
            "outcome": if allow { "allowed" } else { "denied" },
        }),
    );
    Ok(allow)
}

/// Polls the shared cancel flag until it is set.
async fn wait_cancelled(flag: Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Returns the stable grant key for a normalized command line.
pub fn command_hash(normalized_command: &str) -> u64 {
    let mut hasher = twox_hash::XxHash64::default();
    hasher.write(normalized_command.as_bytes());
    hasher.finish()
}

/// Collapses duplicated lines and truncates oversized tool output.
///
/// Consecutive duplicate lines are folded into a single marker; output longer
/// than `max_bytes` keeps its head and tail halves joined by an omission
/// marker.
pub fn compress_output(output: &str, max_bytes: usize) -> String {
    let deduped = fold_duplicate_lines(output);
    if deduped.len() <= max_bytes {
        return deduped;
    }
    let half = max_bytes / 2;
    let head: String = truncate_chars(&deduped, half, false);
    let tail: String = truncate_chars(&deduped, half, true);
    format!("{head}\n... {0} bytes omitted ...\n{tail}", deduped.len() - head.len() - tail.len())
}

fn fold_duplicate_lines(output: &str) -> String {
    let mut out = String::new();
    let mut lines = output.lines().peekable();
    while let Some(line) = lines.next() {
        let mut repeats = 1usize;
        while lines.peek() == Some(&line) && repeats < usize::MAX {
            lines.next();
            repeats += 1;
        }
        out.push_str(line);
        out.push('\n');
        if repeats > 1 {
            out.push_str(&format!("... {} duplicate lines omitted ...\n", repeats - 1));
        }
    }
    if output.ends_with('\n') || out.is_empty() {
        return out;
    }
    out.strip_suffix('\n').map(str::to_string).unwrap_or(out)
}

fn truncate_chars(text: &str, max_bytes: usize, from_end: bool) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut taken = 0usize;
    if from_end {
        let collected: String = text
            .chars()
            .rev()
            .take_while(|c| {
                let size = c.len_utf8();
                let keep = taken < max_bytes;
                taken += size;
                keep
            })
            .collect();
        collected.chars().rev().collect()
    } else {
        text.chars()
            .take_while(|c| {
                let size = c.len_utf8();
                let keep = taken < max_bytes;
                taken += size;
                keep
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_hash_is_stable_and_normalized_input_only() {
        assert_eq!(command_hash("cargo build"), command_hash("cargo build"));
        assert_ne!(command_hash("cargo build"), command_hash("cargo test"));
    }

    #[test]
    fn folds_consecutive_duplicate_lines() {
        let input = "a\na\na\nb\n";
        let out = compress_output(input, usize::MAX);
        assert_eq!(out, "a\n... 2 duplicate lines omitted ...\nb\n");
    }

    #[test]
    fn truncates_oversized_output_head_and_tail() {
        let long_line = "x".repeat(100_000);
        let out = compress_output(&long_line, 8192);
        assert!(out.len() < 10_000);
        assert!(out.contains("bytes omitted"));
        assert!(out.starts_with('x'));
        assert!(out.ends_with('x'));
    }

    #[tokio::test]
    async fn disabled_sandbox_allows_everything() {
        let mut ctx = test_context();
        ctx.config =
            Some(Arc::new(tokio::sync::RwLock::new(metteur_shared::config::Config::default())));
        assert!(authorize(&ctx, "rm -rf /").await.unwrap());
    }

    #[tokio::test]
    async fn whitelisted_command_runs_without_broker() {
        let mut cfg = metteur_shared::config::Config::default();
        cfg.sandbox.enabled = true;
        cfg.sandbox.whitelist = vec!["echo *".to_string()];
        let mut ctx = test_context();
        ctx.config = Some(Arc::new(tokio::sync::RwLock::new(cfg)));
        assert!(authorize(&ctx, "echo hi").await.unwrap());
    }

    #[tokio::test]
    async fn unlisted_command_fails_closed_without_broker() {
        let mut cfg = metteur_shared::config::Config::default();
        cfg.sandbox.enabled = true;
        let mut ctx = test_context();
        ctx.config = Some(Arc::new(tokio::sync::RwLock::new(cfg)));
        assert!(!authorize(&ctx, "python train.py").await.unwrap());
    }

    fn test_context() -> ExecutionContext {
        ExecutionContext::new(
            Arc::new(crate::registry::Registry::with_builtins()),
            crate::llm::LlmClientFactory::new(),
            std::env::temp_dir(),
        )
    }
}
