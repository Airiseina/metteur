//! Agent task lists.
//!
//! A todo list gives a long ReAct run an explicit plan the model can see and
//! update, which keeps multi-step work anchored and makes progress visible to
//! the user. The whole list is replaced on every write (see the tool docs):
//! incremental merges invite drift between what the model believes and what
//! the list holds.

use serde::{Deserialize, Serialize};

/// The state of one todo item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    #[default]
    Pending,
    /// Currently being worked on.
    InProgress,
    /// Finished.
    Completed,
}

impl TodoStatus {
    /// Parses a status from its wire form.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "pending" => Some(Self::Pending),
            "in_progress" | "inprogress" | "active" => Some(Self::InProgress),
            "completed" | "complete" | "done" => Some(Self::Completed),
            _ => None,
        }
    }

    /// Returns the wire form of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// One entry of an agent task list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// What the item is about, in the imperative ("Add the parser").
    pub content: String,
    /// The state of the item.
    #[serde(default)]
    pub status: TodoStatus,
    /// Present-continuous phrasing shown while the item is in progress
    /// ("Adding the parser"); optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

impl TodoItem {
    /// Creates a pending item.
    pub fn pending(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            status: TodoStatus::Pending,
            active_form: None,
        }
    }

    /// Returns the label to display for this item.
    pub fn label(&self) -> &str {
        match (self.status, &self.active_form) {
            (TodoStatus::InProgress, Some(active)) if !active.is_empty() => active,
            _ => &self.content,
        }
    }
}

/// Renders a list as a compact status report for the model.
///
/// Counts come first so the model can judge overall progress at a glance, then
/// one line per item with a `[ ]` / `[~]` / `[x]` marker.
pub fn render(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "No todos.".to_string();
    }
    let completed = todos.iter().filter(|t| t.status == TodoStatus::Completed).count();
    let in_progress = todos.iter().filter(|t| t.status == TodoStatus::InProgress).count();
    let mut out = format!(
        "{completed}/{} completed ({} in progress)\n",
        todos.len(),
        in_progress
    );
    for todo in todos {
        let marker = match todo.status {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
        };
        out.push_str(&format!("{marker} {}\n", todo.label()));
    }
    out
}

/// Returns the number of items in progress.
pub fn in_progress_count(todos: &[TodoItem]) -> usize {
    todos.iter().filter(|t| t.status == TodoStatus::InProgress).count()
}
