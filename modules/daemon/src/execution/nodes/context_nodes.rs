//! Context-manager node executors.
//!
//! These nodes thread [`ContextManager`] values through the blueprint so a
//! conversation context can be created, transformed and inspected without
//! scattering prompt text across node pins.

use std::collections::HashMap;

use async_trait::async_trait;
use metteur_shared::llm::{ContextManager, Message, Role, SystemFragment};
use metteur_shared::{Node, PinId, Value};

use crate::error::DaemonResult;
use crate::execution::context::ExecutionContext;
use crate::registry::NodeExecutor;

use super::{int_input, string_input, string_output, value_input};

/// Reads a context input by pin name.
fn context_input(
    node: &Node,
    inputs: &HashMap<PinId, Value>,
    name: &str,
) -> DaemonResult<ContextManager> {
    let value = value_input(node, inputs, name)?;
    value
        .as_context()
        .cloned()
        .ok_or_else(|| crate::error::DaemonError::Execution(format!("input {name} is not a context")))
}

/// Writes a context to the `Result` pin.
fn context_output(node: &Node, value: ContextManager) -> DaemonResult<HashMap<PinId, Value>> {
    let pin = node
        .pins
        .iter()
        .find(|p| p.name == "Result")
        .ok_or_else(|| crate::error::DaemonError::Execution("missing pin Result".to_string()))?;
    Ok(HashMap::from([(pin.id, Value::Context(value))]))
}

/// Maps a role choice string to a [`Role`].
fn role_from_choice(raw: &str) -> Role {
    match raw {
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        "system" => Role::System,
        _ => Role::User,
    }
}

/// Creates a context from system text and an optional initial prompt.
pub struct ContextCreateExecutor;

#[async_trait]
impl NodeExecutor for ContextCreateExecutor {
    fn kind(&self) -> &str {
        "ContextCreate"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let system = string_input(node, inputs, "System")?;
        let prompt = string_input(node, inputs, "Prompt")?;
        let fragments = if system.is_empty() {
            Vec::new()
        } else {
            vec![SystemFragment {
                priority: 0,
                scope: "node".to_string(),
                content: system,
            }]
        };
        context_output(node, ContextManager::new_from_prompt(fragments, prompt))
    }
}

/// Clones a context so downstream mutations do not leak upstream.
pub struct ContextCloneExecutor;

#[async_trait]
impl NodeExecutor for ContextCloneExecutor {
    fn kind(&self) -> &str {
        "ContextClone"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        context_output(node, context_input(node, inputs, "In")?)
    }
}

/// Mixes one message into a context: `Context + text + role -> Result`.
pub struct ContextMergeExecutor;

#[async_trait]
impl NodeExecutor for ContextMergeExecutor {
    fn kind(&self) -> &str {
        "ContextMerge"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut context = context_input(node, inputs, "Context")?;
        let text = string_input(node, inputs, "Text")?;
        let role = role_from_choice(&string_input(node, inputs, "Role")?);
        context.push_message(Message::text(role, text));
        context_output(node, context)
    }
}

/// Keeps only the messages of one role: `Context + Role -> Result`.
pub struct ContextFilterExecutor;

#[async_trait]
impl NodeExecutor for ContextFilterExecutor {
    fn kind(&self) -> &str {
        "ContextFilter"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut context = context_input(node, inputs, "Context")?;
        let role = role_from_choice(&string_input(node, inputs, "Role")?);
        context.messages.retain(|m| m.role == role);
        context_output(node, context)
    }
}

/// Keeps the most recent `keep` messages of a context.
pub struct ContextTrimExecutor;

#[async_trait]
impl NodeExecutor for ContextTrimExecutor {
    fn kind(&self) -> &str {
        "ContextTrim"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let mut context = context_input(node, inputs, "Context")?;
        let keep = int_input(node, inputs, "Keep")?.max(0) as usize;
        if context.messages.len() > keep {
            let drop = context.messages.len() - keep;
            context.messages.drain(..drop);
        }
        context_output(node, context)
    }
}

/// Dumps a context's messages as text for inspection.
pub struct ContextToTextExecutor;

#[async_trait]
impl NodeExecutor for ContextToTextExecutor {
    fn kind(&self) -> &str {
        "ContextToText"
    }

    async fn execute(
        &self,
        node: &Node,
        inputs: &HashMap<PinId, Value>,
        _ctx: &mut ExecutionContext,
    ) -> DaemonResult<HashMap<PinId, Value>> {
        let context = context_input(node, inputs, "In")?;
        let mut out = String::new();
        for fragment in &context.system_fragments {
            out.push_str(&format!("[system] {}\n", fragment.content));
        }
        for message in &context.messages {
            let role = format!("{:?}", message.role).to_ascii_lowercase();
            out.push_str(&format!("[{role}] {}\n", message.text_content()));
        }
        string_output(node, "Result", out)
    }
}