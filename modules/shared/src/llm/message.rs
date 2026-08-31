//! Conversation messages and content blocks.

use serde::{Deserialize, Serialize};

use super::tool::ToolCall;

/// The role of a message in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// System-level instructions.
    System,
    /// A user message.
    User,
    /// An assistant (model) message.
    Assistant,
    /// A tool result message.
    Tool,
}

/// A single content block within a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    /// Plain text content.
    Text(String),
}

/// A single conversation message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message.
    pub role: Role,
    /// The content blocks of the message.
    pub content: Vec<ContentBlock>,
    /// Tool calls made by an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The tool call id this message is a result for (tool role only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Creates a text message with the given role.
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::Text(text.into())],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Returns the concatenated text content of the message.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(t) => t.as_str(),
            })
            .collect()
    }
}
