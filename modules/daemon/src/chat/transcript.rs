//! The display transcript of a chat session.
//!
//! The model's context and the user's transcript are different things. The
//! context is compressed, evicted and rewritten with markers as a run proceeds,
//! and it drops everything that is not a user/assistant message because a
//! provider only accepts a narrow shape. A transcript rebuilt from it therefore
//! loses tool calls entirely — which is exactly what happened before this
//! module existed: switching sessions showed a conversation with no tools in
//! it.
//!
//! So the daemon records what the *conversation looked like* alongside the
//! context: every tool call with its subject, duration and outcome, notices,
//! failures, and the assistant's reasoning. [`Transcript`] accumulates those
//! entries while a turn streams, and the record persists them.

use serde::{Deserialize, Serialize};

/// Maximum transcript entries kept per session.
///
/// Oldest entries are dropped first, which bounds a long-lived session without
/// touching the model's context.
pub const MAX_ENTRIES: usize = 400;

/// Maximum characters kept per tool result in the transcript.
///
/// The expanded view of a restored tool call shows this much; the live view
/// shows everything. Keeping full outputs here would duplicate the context in
/// the database for a view nobody reads in full.
pub const MAX_TOOL_CHARS: usize = 8 * 1024;

/// The role of a transcript entry, as rendered by a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryRole {
    /// A turn written by the user.
    User,
    /// An assistant answer, possibly with the reasoning that preceded it.
    Assistant,
    /// One tool call.
    Tool,
    /// An engine notice (retry, job progress, context release).
    Notice,
    /// A failed turn.
    Error,
}

/// One entry of the display transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Who or what produced this entry.
    pub role: EntryRole,
    /// Creation time in milliseconds since the Unix epoch.
    pub at: u64,
    /// Rendered text (the answer, the user's turn, the tool's output).
    #[serde(default)]
    pub content: String,
    /// Reasoning that preceded an assistant answer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// Tool name, for `Tool` entries.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Provider call id, which pairs a start with its result.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    /// One-line subject of the call (the same summary the live row shows).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Whether the call succeeded; meaningful for `Tool` entries.
    #[serde(default)]
    pub ok: bool,
    /// Wall-clock duration of the call in milliseconds.
    #[serde(default)]
    pub elapsed_ms: u64,
}

impl TranscriptEntry {
    fn new(role: EntryRole, at: u64) -> Self {
        Self {
            role,
            at,
            content: String::new(),
            reasoning: String::new(),
            tool: String::new(),
            call_id: String::new(),
            summary: String::new(),
            ok: true,
            elapsed_ms: 0,
        }
    }
}

/// Accumulates the transcript of one turn.
///
/// A tool call is recorded twice: when it starts (so an interrupted turn still
/// shows what it was doing) and when it returns, which updates the same entry.
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<TranscriptEntry>,
    /// Previous transcript of the session, carried through the turn.
    history: Vec<TranscriptEntry>,
}

impl Transcript {
    /// Continues the transcript of an existing session.
    pub fn resuming(history: Vec<TranscriptEntry>) -> Self {
        Self {
            entries: Vec::new(),
            history,
        }
    }

    /// Records the user's turn.
    pub fn push_user(&mut self, text: impl Into<String>, at: u64) {
        let mut entry = TranscriptEntry::new(EntryRole::User, at);
        entry.content = text.into();
        self.entries.push(entry);
    }

    /// Records a finished assistant answer.
    pub fn push_assistant(
        &mut self,
        text: impl Into<String>,
        reasoning: impl Into<String>,
        at: u64,
    ) {
        let mut entry = TranscriptEntry::new(EntryRole::Assistant, at);
        entry.content = text.into();
        entry.reasoning = reasoning.into();
        self.entries.push(entry);
    }

    /// Records a tool call that is about to run.
    pub fn push_tool_start(
        &mut self,
        call_id: impl Into<String>,
        tool: impl Into<String>,
        summary: impl Into<String>,
        at: u64,
    ) {
        let mut entry = TranscriptEntry::new(EntryRole::Tool, at);
        entry.call_id = call_id.into();
        entry.tool = tool.into();
        entry.summary = summary.into();
        self.entries.push(entry);
    }

    /// Completes the tool call started with `call_id`.
    ///
    /// Falls back to appending when the start was never seen (a resumed turn, or
    /// a provider that reports only results).
    pub fn push_tool_result(
        &mut self,
        call_id: &str,
        tool: &str,
        ok: bool,
        elapsed_ms: u64,
        content: impl Into<String>,
        at: u64,
    ) {
        let content = truncate(content.into(), MAX_TOOL_CHARS);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.role == EntryRole::Tool && entry.call_id == call_id)
        {
            entry.ok = ok;
            entry.elapsed_ms = elapsed_ms;
            entry.content = content;
            entry.at = at;
            return;
        }
        let mut entry = TranscriptEntry::new(EntryRole::Tool, at);
        entry.call_id = call_id.to_string();
        entry.tool = tool.to_string();
        entry.ok = ok;
        entry.elapsed_ms = elapsed_ms;
        entry.content = content;
        self.entries.push(entry);
    }

    /// Records an engine notice.
    pub fn push_notice(&mut self, text: impl Into<String>, at: u64) {
        let mut entry = TranscriptEntry::new(EntryRole::Notice, at);
        entry.content = text.into();
        self.entries.push(entry);
    }

    /// Records a failed turn.
    pub fn push_error(&mut self, text: impl Into<String>, at: u64) {
        let mut entry = TranscriptEntry::new(EntryRole::Error, at);
        entry.content = text.into();
        self.entries.push(entry);
    }

    /// Returns the session's transcript, oldest entry first, bounded in size.
    pub fn finish(self) -> Vec<TranscriptEntry> {
        let mut all = self.history;
        all.extend(self.entries);
        if all.len() > MAX_ENTRIES {
            all.drain(..all.len() - MAX_ENTRIES);
        }
        all
    }
}

/// Truncates `text` to `max` bytes on a character boundary.
fn truncate(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[transcript truncated]", &text[..end])
}

/// Serializes a transcript for the wire, or `[]` when it cannot be encoded.
pub fn to_json(entries: &[TranscriptEntry]) -> String {
    serde_json::to_string(entries).unwrap_or_else(|_| "[]".to_string())
}
