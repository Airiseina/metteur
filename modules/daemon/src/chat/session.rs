//! Persisted chat session storage.
//!
//! One active session per workspace, stored in the workspace RocksDB under a
//! fixed key. The full [`ContextManager`] is persisted so a conversation can
//! resume after a restart with its tool results intact.

use metteur_shared::llm::{ContextManager, Role};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{DaemonError, DaemonResult};
use crate::storage::persistence::{Db, cf};

/// The DB key of the active session record.
const ACTIVE_KEY: &[u8] = b"active";

/// Truncation length for the session title.
const TITLE_MAX_CHARS: usize = 80;

/// A persisted chat session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSessionRecord {
    /// The session identifier.
    pub session_id: Uuid,
    /// Creation time in milliseconds since the Unix epoch.
    pub created_at: u64,
    /// Last write time in milliseconds since the Unix epoch.
    pub updated_at: u64,
    /// Number of completed turns (user messages with a finished reply).
    pub turns: u64,
    /// First user message, truncated, for list display.
    pub title: Option<String>,
    /// The conversation context, including retained tool results.
    pub context: ContextManager,
}

impl ChatSessionRecord {
    /// Creates a record for a fresh session started at `now`.
    pub fn new(now: u64, context: ContextManager) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
            turns: 0,
            title: title_of(&context),
            context,
        }
    }
}

/// Loads the workspace's active session, if any.
pub fn load_active(db: &Db) -> DaemonResult<Option<ChatSessionRecord>> {
    let Some(data) = db.get(cf::CHAT_SESSIONS, ACTIVE_KEY)? else {
        return Ok(None);
    };
    let record: ChatSessionRecord = serde_json::from_slice(&data)
        .map_err(|e| DaemonError::Serialization(e.to_string()))?;
    Ok(Some(record))
}

/// Overwrites the workspace's active session.
pub fn save_active(db: &Db, record: &ChatSessionRecord) -> DaemonResult<()> {
    let data = serde_json::to_vec(record)
        .map_err(|e| DaemonError::Serialization(e.to_string()))?;
    db.put(cf::CHAT_SESSIONS, ACTIVE_KEY, &data)
}

/// Removes the workspace's active session (idempotent).
pub fn delete_active(db: &Db) -> DaemonResult<()> {
    db.delete(cf::CHAT_SESSIONS, ACTIVE_KEY)
}

/// Counts user and assistant messages (system/tool messages excluded).
pub fn display_message_count(record: &ChatSessionRecord) -> usize {
    record
        .context
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .count()
}

/// Builds a truncated title from the first user message.
pub(crate) fn title_of(context: &ContextManager) -> Option<String> {
    let text = context.messages.iter().find(|m| m.role == Role::User).map(|m| m.text_content())?;
    Some(text.chars().take(TITLE_MAX_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_shared::llm::Message;

    fn db() -> Db {
        let dir = std::env::temp_dir().join(format!("metteur-chat-{}", Uuid::new_v4()));
        Db::open(&dir).unwrap()
    }

    fn context_with(text: &str) -> ContextManager {
        let mut ctx = ContextManager::default();
        ctx.push_message(Message::text(Role::User, text));
        ctx.push_message(Message::text(Role::Assistant, "ok"));
        ctx
    }

    #[test]
    fn save_load_roundtrip() {
        let db = db();
        assert!(load_active(&db).unwrap().is_none());
        let record = ChatSessionRecord::new(1000, context_with("hello"));
        save_active(&db, &record).unwrap();
        let loaded = load_active(&db).unwrap().unwrap();
        assert_eq!(loaded.session_id, record.session_id);
        assert_eq!(loaded.context.messages.len(), 2);
        assert_eq!(loaded.title.as_deref(), Some("hello"));
        assert_eq!(display_message_count(&loaded), 2);
    }

    #[test]
    fn delete_is_idempotent() {
        let db = db();
        delete_active(&db).unwrap();
        let record = ChatSessionRecord::new(1000, context_with("hi"));
        save_active(&db, &record).unwrap();
        delete_active(&db).unwrap();
        assert!(load_active(&db).unwrap().is_none());
    }

    #[test]
    fn title_is_truncated() {
        let long = "x".repeat(TITLE_MAX_CHARS + 50);
        let ctx = context_with(&long);
        let title = title_of(&ctx).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    }
}