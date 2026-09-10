//! Output formatting helpers for command results and execution events.

use metteur_proto::proto::{
    AuditLogList, ChatEvent, ChatSessionList, ExecutionEvent, ExecutionList, ExecutionTree,
    FileHistory, McpServerList, SnapshotList, ToolList, UsageSummary, WorkspaceList,
};

/// Renders a live execution event as `[node kind] message`.
pub fn event(ev: &ExecutionEvent) -> String {
    let mut line = format!("[{} {}]", ev.node_id, ev.kind);
    if !ev.message.is_empty() {
        line.push(' ');
        line.push_str(&ev.message);
    }
    // Distinguish sandbox, circuit-breaker and replan approval requests.
    if ev.kind == "approval_request"
        && !ev.detail_json.is_empty()
        && let Ok(detail) = serde_json::from_str::<serde_json::Value>(&ev.detail_json)
        && let Some(kind) = detail.get("request_type").and_then(|v| v.as_str())
    {
        line.push_str(&format!(" [{kind}]"));
    }
    line
}

/// Lists open workspaces, marking the current one.
pub fn workspaces(list: &WorkspaceList, current: Option<&str>) -> String {
    if list.workspaces.is_empty() {
        return "(no open workspaces)".to_string();
    }
    let mut out = String::new();
    for ws in &list.workspaces {
        let marker = if current.is_some() && current == Some(ws.path.as_str()) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!("{marker} {}", ws.path));
        if ws.locked {
            out.push_str(" (locked)");
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Lists registered tools in aligned columns.
pub fn tools(list: &ToolList) -> String {
    if list.tools.is_empty() {
        return "(no tools registered)".to_string();
    }
    let width = list.tools.iter().map(|t| t.name.len()).max().unwrap_or(4).max(4);
    let mut out = format!("{:<width$}  DESCRIPTION\n", "TOOL", width = width + 8);
    for tool in &list.tools {
        out.push_str(&format!("{:<width$}  {}\n", tool.name, tool.description, width = width + 8));
    }
    out.trim_end().to_string()
}

/// Lists available node kinds as a comma separated line.
pub fn node_kinds(kinds: &[String]) -> String {
    if kinds.is_empty() {
        "(no node kinds)".to_string()
    } else {
        kinds.join(", ")
    }
}

/// Lists executions (runs) for a workspace.
pub fn executions(list: &ExecutionList) -> String {
    if list.executions.is_empty() {
        return "(no executions)".to_string();
    }
    let mut out = String::from(
        "RUN ID                                 BLUEPRINT ID                           STATUS      NODES  UPDATED\n",
    );
    for run in &list.executions {
        out.push_str(&format!(
            "{:<38} {:<38} {:<11} {:>5}  {}\n",
            run.run_id,
            run.blueprint_id,
            run.status,
            run.executed_nodes,
            timestamp(run.updated_at),
        ));
    }
    out.trim_end().to_string()
}

/// Renders an agent execution tree as indented ASCII.
pub fn execution_tree(tree: &ExecutionTree) -> String {
    if tree.nodes.is_empty() {
        return "(empty execution tree)".to_string();
    }
    let by_id: std::collections::HashMap<&str, &metteur_proto::proto::ExecTreeNode> =
        tree.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    fn render(
        by_id: &std::collections::HashMap<&str, &metteur_proto::proto::ExecTreeNode>,
        id: &str,
        prefix: &str,
        out: &mut String,
    ) {
        let Some(node) = by_id.get(id) else {
            return;
        };
        let status = match node.status.split(':').next().unwrap_or("") {
            "done" => "done",
            "failed" => "FAILED",
            _ => "running",
        };
        out.push_str(&format!(
            "{}{} [{}] {} ({} tokens)\n",
            prefix, node.label, node.kind, status, node.tokens,
        ));
        for (i, child) in node.children.iter().enumerate() {
            let last = i + 1 == node.children.len();
            render(
                by_id,
                child,
                &format!(
                    "{prefix}{}",
                    if last {
                        "  "
                    } else {
                        "│ "
                    }
                ),
                out,
            );
        }
    }
    let mut out = String::new();
    for root in &tree.roots {
        render(&by_id, root, "", &mut out);
    }
    out.trim_end().to_string()
}

/// Lists workspace snapshots.
pub fn snapshots(list: &SnapshotList) -> String {
    if list.snapshots.is_empty() {
        return "(no snapshots)".to_string();
    }
    let mut out = String::new();
    for snap in &list.snapshots {
        let alias = if snap.alias.is_empty() {
            String::new()
        } else {
            format!("  @{}", snap.alias)
        };
        out.push_str(&format!(
            "{}  {}{}  {}\n",
            snap.id,
            timestamp(snap.created_at),
            alias,
            snap.description
        ));
    }
    out.trim_end().to_string()
}

/// Renders file history entries for one path.
pub fn file_history(list: &FileHistory, path: &str) -> String {
    if list.entries.is_empty() {
        return format!("(no history for {path})");
    }
    let mut out = String::new();
    for entry in &list.entries {
        out.push_str(&format!(
            "{}  {:<9} {}  {}\n",
            entry.snapshot_id,
            entry.status,
            timestamp(entry.created_at),
            entry.description
        ));
    }
    out.trim_end().to_string()
}

/// Renders audit entries oldest first, one line each.
pub fn audit(list: &AuditLogList) -> String {
    if list.entries.is_empty() {
        return "(empty audit log)".to_string();
    }
    let mut out = String::new();
    for entry in &list.entries {
        out.push_str(&format!(
            "[{}] {} {}\n",
            timestamp(entry.timestamp),
            entry.user_id,
            entry.operation
        ));
        if !entry.detail_json.is_empty() {
            out.push_str(&format!("    {}\n", compact_json(&entry.detail_json)));
        }
    }
    out.trim_end().to_string()
}

/// Renders a usage summary: a per-model table plus total cost.
pub fn usage(summary: &UsageSummary) -> String {
    if summary.models.is_empty() {
        return "(no usage recorded)".to_string();
    }
    let mut out = String::from(
        "MODEL                            CALLS         INPUT        OUTPUT    REASONING          COST\n",
    );
    for model in &summary.models {
        out.push_str(&format!(
            "{:<32} {:>5} {:>13} {:>13} {:>13}  {:>10}\n",
            model.model,
            model.calls,
            model.input_tokens,
            model.output_tokens,
            model.reasoning_tokens,
            micros(model.cost_micros)
        ));
    }
    out.push_str(&format!(
        "total cost: {} micros {} ({})",
        summary.total_cost_micros,
        summary.currency,
        micros(summary.total_cost_micros)
    ));
    out
}

/// Lists MCP servers and their status.
pub fn mcp_servers(list: &McpServerList) -> String {
    if list.servers.is_empty() {
        return "(no MCP servers configured)".to_string();
    }
    let mut out = String::new();
    for server in &list.servers {
        out.push_str(&format!("{}  {}  tools={}", server.name, server.status, server.tool_count));
        if !server.error.is_empty() {
            out.push_str(&format!("  error: {}", server.error));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Pretty-prints JSON text, falling back to the raw string.
pub fn pretty_json(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

/// Formats an addon listing.
pub fn addons(list: &metteur_proto::proto::AddonList) -> String {
    if list.addons.is_empty() {
        return "(no addons installed)".to_string();
    }
    let mut out = String::new();
    for addon in &list.addons {
        out.push_str(&format!(
            "{}  v{}  [{}] {}  tools={} fragments={}",
            addon.id,
            addon.version,
            if addon.enabled {
                "on"
            } else {
                "off"
            },
            addon.scope,
            addon.tool_count,
            addon.fragment_count
        ));
        if !addon.required_permissions.is_empty() {
            out.push_str(&format!("  perms={}", addon.required_permissions.join(",")));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Formats millis since the Unix epoch as `YYYY-MM-DD HH:MM:SS` (UTC).
fn timestamp(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| millis.to_string())
}

/// Formats cost micros exactly as a decimal amount.
fn micros(cost: u64) -> String {
    format!("{}.{:06}", cost / 1_000_000, cost % 1_000_000)
}

/// Compacts JSON text to a single line when parseable.
fn compact_json(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

/// How one chat event should be displayed.
pub enum ChatLine {
    /// A reply fragment printed inline without a newline.
    Inline(String),
    /// A complete line to print.
    Line(String),
    /// A terminal marker (`session`, `done`) with nothing to show.
    Done,
}

/// Renders one chat event for the REPL.
pub fn chat_event(event: &ChatEvent) -> ChatLine {
    match event.kind.as_str() {
        "assistant_delta" => ChatLine::Inline(event.content.clone()),
        "assistant" => ChatLine::Line(format!("\n{}", event.content)),
        "tool" => {
            let name = serde_json::from_str::<serde_json::Value>(&event.detail_json)
                .ok()
                .and_then(|d| d.get("name").and_then(|v| v.as_str()).map(str::to_string))
                .unwrap_or_default();
            ChatLine::Line(format!("\n[tool {name}] {}", event.content))
        }
        "error" => ChatLine::Line(format!("\n[chat error] {}", event.content)),
        _ => ChatLine::Done,
    }
}

/// Lists chat threads of a workspace.
pub fn chat_sessions(list: &ChatSessionList) -> String {
    if list.sessions.is_empty() {
        return "(no chat sessions)".to_string();
    }
    let mut out = String::from(
        "SESSION ID                           TURNS  MSGS  UPDATED              TITLE\n",
    );
    for session in &list.sessions {
        out.push_str(&format!(
            "{:<38} {:>5}  {:>4}  {:<20} {}\n",
            session.session_id,
            session.turns,
            session.message_count,
            timestamp(session.updated_at),
            session.title,
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_proto::proto::ModelUsage;

    fn fixture_summary() -> UsageSummary {
        UsageSummary {
            currency: "USD".to_string(),
            total_cost_micros: 12_750_500,
            models: vec![
                ModelUsage {
                    model: "gpt-5".to_string(),
                    calls: 3,
                    input_tokens: 1_000,
                    output_tokens: 2_000,
                    reasoning_tokens: 100,
                    cost_micros: 10_000_000,
                },
                ModelUsage {
                    model: "claude-haiku".to_string(),
                    calls: 1,
                    input_tokens: 50,
                    output_tokens: 60,
                    reasoning_tokens: 0,
                    cost_micros: 2_750_500,
                },
            ],
        }
    }

    #[test]
    fn usage_lists_models_and_total() {
        let text = usage(&fixture_summary());
        assert!(text.contains("gpt-5"));
        assert!(text.contains("claude-haiku"));
        assert!(text.contains("10.000000"));
        assert!(text.contains("2.750500"));
        assert!(text.contains("total cost: 12750500 micros USD (12.750500)"));
        assert!(text.contains("CALLS"));
    }

    #[test]
    fn usage_without_models_is_reported() {
        let summary = UsageSummary {
            currency: "USD".to_string(),
            total_cost_micros: 0,
            models: vec![],
        };
        assert_eq!(usage(&summary), "(no usage recorded)");
    }

    #[test]
    fn event_line_contains_node_kind_and_message() {
        let ev = ExecutionEvent {
            node_id: "abc".to_string(),
            kind: "message".to_string(),
            message: "hello".to_string(),
            detail_json: String::new(),
        };
        assert_eq!(event(&ev), "[abc message] hello");
        let silent = ExecutionEvent {
            node_id: "abc".to_string(),
            kind: "started".to_string(),
            message: String::new(),
            detail_json: String::new(),
        };
        assert_eq!(event(&silent), "[abc started]");
    }

    #[test]
    fn timestamps_render_as_utc() {
        let text = pretty_json("{\"b\":2,\"a\":1}");
        assert!(text.starts_with('{') && text.contains("\"a\": 1"));
        assert_eq!(pretty_json("not json"), "not json");
        assert_eq!(
            chrono::DateTime::from_timestamp_millis(0)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap(),
            "1970-01-01 00:00:00"
        );
    }
}
