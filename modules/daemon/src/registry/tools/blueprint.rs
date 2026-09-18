//! The `DraftBlueprint` tool: authors a plan as a compact JSON draft.
//!
//! This is the Plan-and-Execute entry point. Instead of emitting the verbose
//! canvas format — pin ids, UUIDs, coordinates, one edge object per wire —
//! the model writes only the steps and how they connect; a deterministic
//! compiler in the shared crate derives everything else and rejects mistakes
//! with messages that name the offending node or pin.
//!
//! Two properties make it usable inside a ReAct loop:
//!
//! * **The result is a draft, not a run.** The tool compiles, renders the plan
//!   back as readable text, and (unless asked to save) stops there, so the
//!   model and the user can review it before anything executes. Saving is an
//!   explicit flag, matching the "user approves plans" rule in the design.
//! * **Compilation is idempotent.** Ids and positions derive from the draft, so
//!   re-saving a corrected draft updates one blueprint instead of piling up
//!   near-duplicates.

use async_trait::async_trait;
use metteur_shared::llm::ToolResultLifetime;
use metteur_shared::Value;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::registry::tool::Tool;

use super::Args;

/// Byte budget for the rendered plan.
const RESULT_MAX_BYTES: usize = 16 * 1024;

/// Authors a blueprint from a compact JSON draft.
pub struct DraftBlueprint;

#[async_trait]
impl Tool for DraftBlueprint {
    fn name(&self) -> &str {
        "DraftBlueprint"
    }

    fn description(&self) -> &str {
        "Compiles a plan into a blueprint. Write `draft` as compact JSON:\n\
         {\"name\": \"...\", \"nodes\": {\"alias\": {\"kind\": \"Kind\", ...args}},\n\
         \x20\"flow\": [\"a -> b -> c\"], \"wires\": [\"target.pin <- source.pin\"]}\n\
         Rules: keys inside a node are its input values; a string \"$alias.pin\" \
         wires the output of another node instead of using a literal; `flow` \
         chains steps with \"->\" and may route a specific output \
         (\"br.True -> ok\"); node positions and ids are generated. Registry \
         tools are named directly (ReadFile, Grep, EditFile, LspCheck, ...). \
         Set `save` to true to store the blueprint in the workspace; otherwise \
         the compiled plan is returned for review only. Fix any reported error \
         and call again with the corrected draft."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "draft": {
                    "type": "object",
                    "description": "The plan: name, nodes keyed by alias, optional flow and wires."
                },
                "save": {
                    "type": "boolean",
                    "description": "Store the compiled blueprint in the workspace (default false: review only)."
                }
            },
            "required": ["draft"]
        })
    }

    fn lifetime(&self) -> ToolResultLifetime {
        ToolResultLifetime::Persistent
    }

    fn max_result_bytes(&self) -> usize {
        RESULT_MAX_BYTES
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let draft = a
            .get("draft", 0)
            .ok_or_else(|| DaemonError::Execution("DraftBlueprint requires a draft".to_string()))?;
        let save = a.bool("save", 1).unwrap_or(false);
        // The draft is authored as JSON; accept it either as a real object or
        // as a JSON string (some callers stringify nested arguments).
        let draft_json = match &draft {
            Value::Json(value) => value.clone(),
            Value::String(text) => serde_json::from_str(text).map_err(|err| {
                DaemonError::Execution(format!("`draft` is not valid JSON: {err}"))
            })?,
            // Any other shape is converted through the crate's value bridge
            // so the compiler sees one uniform JSON value.
            other => crate::execution::nodes::value_to_json(other),
        };

        let blueprint = metteur_shared::dsl::compile_draft_value(&draft_json)
            .map_err(|err| DaemonError::Execution(err.to_string()))?;

        ctx.audit(
            "blueprint.draft",
            serde_json::json!({
                "run_id": ctx.run_id.to_string(),
                "blueprint_id": blueprint.id.to_string(),
                "name": blueprint.name,
                "nodes": blueprint.nodes.len(),
                "edges": blueprint.edges.len(),
                "saved": save,
            }),
        );

        let saved = if save {
            let db = ctx.workspace_db.clone().ok_or_else(|| {
                DaemonError::Execution(
                    "saving a blueprint needs a workspace database; the run has none".to_string(),
                )
            })?;
            let encoded = serde_json::to_vec(&blueprint).map_err(|err| {
                DaemonError::Execution(format!("failed to encode blueprint: {err}"))
            })?;
            db.put(crate::storage::persistence::cf::BLUEPRINTS, blueprint.id.as_bytes(), &encoded)
                .map_err(|err| {
                    DaemonError::Execution(format!("failed to save blueprint: {err}"))
                })?;
            true
        } else {
            false
        };

        Ok(Value::String(render_report(&blueprint, saved)))
    }
}

/// Renders the compiled plan as a reviewable report.
///
/// The model reads this to verify its intent survived compilation (edges
/// landed, references resolved); the summary is deliberately textual so it is
/// cheap to read and easy to diff against the draft.
fn render_report(blueprint: &metteur_shared::Blueprint, saved: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Blueprint \"{}\" compiled: {} node(s), {} edge(s).\n",
        blueprint.name,
        blueprint.nodes.len(),
        blueprint.edges.len()
    ));
    out.push_str(if saved {
        "Saved to the workspace; run it by id.\n"
    } else {
        "Not saved (review only); call again with save=true to keep it.\n"
    });

    // Nodes in execution order where possible, so the report reads like a plan.
    let entry = blueprint.entry_node_id;
    let order = execution_order(blueprint, entry);
    out.push_str("\nSteps:\n");
    for node in &order {
        let marker = if node.id == entry { " (entry)" } else { "" };
        out.push_str(&format!("- {} [{}{}]\n", node_label(node), node.kind, marker));
    }
    let unvisited: Vec<&metteur_shared::Node> = blueprint
        .nodes
        .iter()
        .filter(|node| !order.iter().any(|seen| seen.id == node.id))
        .collect();
    if !unvisited.is_empty() {
        // Disconnected nodes are legal but usually a mistake worth surfacing.
        out.push_str("\nNot reached by the flow:\n");
        for node in unvisited {
            out.push_str(&format!("- {} [{}]\n", node_label(node), node.kind));
        }
    }

    let wires = data_wire_lines(blueprint);
    if !wires.is_empty() {
        out.push_str("\nData wiring:\n");
        for wire in wires {
            out.push_str(&format!("- {wire}\n"));
        }
    }
    out
}

/// Returns the nodes reachable from the entry, in breadth-first order.
fn execution_order(
    blueprint: &metteur_shared::Blueprint,
    entry: metteur_shared::NodeId,
) -> Vec<&metteur_shared::Node> {
    use std::collections::{HashSet, VecDeque};
    let mut order = Vec::new();
    if entry.is_nil() {
        return blueprint.nodes.iter().collect();
    }
    let mut seen: HashSet<metteur_shared::NodeId> = HashSet::new();
    let mut queue = VecDeque::from([entry]);
    seen.insert(entry);
    while let Some(current) = queue.pop_front() {
        if let Some(node) = blueprint.nodes.iter().find(|node| node.id == current) {
            order.push(node);
        }
        for edge in blueprint.edges.iter().filter(|edge| edge.source_node == current) {
            if seen.insert(edge.target_node) {
                queue.push_back(edge.target_node);
            }
        }
    }
    order
}

/// Builds a one-line label for a node: its kind plus the values that tell it
/// apart from its siblings.
///
/// A plan with three `ReadFile` steps must not render as three identical
/// lines, so the tool name is followed by the argument that identifies the
/// call (`path`, `pattern`, `command`, ...) rather than replacing it.
fn node_label(node: &metteur_shared::Node) -> String {
    let string_at = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|key| node.data.get(*key).and_then(|value| value.as_str()))
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let tool = string_at(&["tool_name"]);
    // The argument that distinguishes one call of this tool from another.
    let detail = string_at(&[
        "path",
        "pattern",
        "command",
        "query",
        "root",
        "model",
        "prompt",
        "tool_name",
    ]);
    match (tool, detail) {
        (Some(tool), Some(detail)) if !detail.is_empty() => {
            format!("{}: {} {}", node.kind, tool, truncate(&detail, 48))
        }
        (Some(tool), None) => format!("{}: {}", node.kind, tool),
        (None, Some(detail)) => format!("{}: {}", node.kind, truncate(&detail, 60)),
        _ => node.kind.clone(),
    }
}

/// Truncates a label for display.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}...")
}

/// Renders the data (non-execution) wires as `source.pin -> target.pin`.
fn data_wire_lines(blueprint: &metteur_shared::Blueprint) -> Vec<String> {
    use metteur_shared::PinType;
    let name_of = |node_id: metteur_shared::NodeId, pin_id: metteur_shared::PinId| {
        blueprint
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .and_then(|node| node.pins.iter().find(|pin| pin.id == pin_id))
            .map(|pin| pin.name.clone())
            .unwrap_or_else(|| "?".to_string())
    };
    let alias_of = |node_id: metteur_shared::NodeId| {
        blueprint
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .map(|node| {
                node.data
                    .get("tool_name")
                    .and_then(|value| value.as_str())
                    .unwrap_or(node.kind.as_str())
                    .to_string()
            })
            .unwrap_or_else(|| "?".to_string())
    };
    blueprint
        .edges
        .iter()
        .filter(|edge| {
            // Execution edges are implied by the flow list; only data wires
            // need restating.
            blueprint
                .nodes
                .iter()
                .find(|node| node.id == edge.source_node)
                .and_then(|node| node.pins.iter().find(|pin| pin.id == edge.source_pin))
                .is_some_and(|pin| pin.pin_type == PinType::DataOutput)
        })
        .map(|edge| {
            format!(
                "{}.{} -> {}.{}",
                alias_of(edge.source_node),
                name_of(edge.source_node, edge.source_pin),
                alias_of(edge.target_node),
                name_of(edge.target_node, edge.target_pin)
            )
        })
        .collect()
}
