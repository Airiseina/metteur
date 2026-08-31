//! Process-wide metrics exposed in Prometheus text format (doc §6.10).

pub mod http;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// Atomic counters and gauges collected across the daemon.
#[derive(Default)]
pub struct Metrics {
    pub workspaces_active: AtomicU64,
    pub executions_running: AtomicU64,
    pub executions_completed: AtomicU64,
    pub executions_failed: AtomicU64,
    pub llm_calls_total: AtomicU64,
    pub llm_input_tokens_total: AtomicU64,
    pub llm_output_tokens_total: AtomicU64,
    pub tool_calls_total: AtomicU64,
    pub sandbox_approvals_allowed: AtomicU64,
    pub sandbox_approvals_denied: AtomicU64,
    /// Per-server MCP call counters keyed by server alias.
    pub mcp_calls: Mutex<BTreeMap<String, u64>>,
}

impl Metrics {
    /// Records one MCP tool call for the given server alias.
    pub fn record_mcp_call(&self, server: &str) {
        *self.mcp_calls.lock().entry(server.to_string()).or_insert(0) += 1;
    }

    /// Renders all metrics in the Prometheus text exposition format.
    pub fn render(&self) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut out = String::with_capacity(1024);
        let push = |out: &mut String, name: &str, help: &str, value: u64| {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"));
        };
        push(
            &mut out,
            "metteur_workspaces_active",
            "Currently open workspaces.",
            g(&self.workspaces_active),
        );
        push(
            &mut out,
            "metteur_executions_running",
            "Executions currently running.",
            g(&self.executions_running),
        );
        push(
            &mut out,
            "metteur_executions_completed_total",
            "Executions finished successfully.",
            g(&self.executions_completed),
        );
        push(
            &mut out,
            "metteur_executions_failed_total",
            "Executions that ended in failure.",
            g(&self.executions_failed),
        );
        push(
            &mut out,
            "metteur_llm_calls_total",
            "LLM completion calls.",
            g(&self.llm_calls_total),
        );
        push(
            &mut out,
            "metteur_llm_input_tokens_total",
            "Input tokens consumed by LLM calls.",
            g(&self.llm_input_tokens_total),
        );
        push(
            &mut out,
            "metteur_llm_output_tokens_total",
            "Output tokens produced by LLM calls.",
            g(&self.llm_output_tokens_total),
        );
        push(&mut out, "metteur_tool_calls_total", "Tool invocations.", g(&self.tool_calls_total));
        push(
            &mut out,
            "metteur_sandbox_approvals_allowed_total",
            "Sandbox approvals granted.",
            g(&self.sandbox_approvals_allowed),
        );
        push(
            &mut out,
            "metteur_sandbox_approvals_denied_total",
            "Sandbox approvals denied or timed out.",
            g(&self.sandbox_approvals_denied),
        );
        let mcp_calls = self.mcp_calls.lock();
        if !mcp_calls.is_empty() {
            out.push_str("# HELP metteur_mcp_calls_total MCP tool calls per server.\n");
            out.push_str("# TYPE metteur_mcp_calls_total counter\n");
            for (server, count) in mcp_calls.iter() {
                out.push_str(&format!("metteur_mcp_calls_total{{server=\"{server}\"}} {count}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text_format() {
        let m = Metrics::default();
        m.workspaces_active.store(2, Ordering::Relaxed);
        m.executions_failed.store(1, Ordering::Relaxed);
        let text = m.render();
        assert!(text.contains("# TYPE metteur_workspaces_active gauge"));
        assert!(text.contains("metteur_workspaces_active 2\n"));
        assert!(text.contains("metteur_executions_failed_total 1\n"));
        assert!(text.starts_with("# HELP "));
    }
}
