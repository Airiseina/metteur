//! Chat session persistence.
//!
//! A session stores the full conversation context so a chat can be resumed
//! after a daemon restart with its tool results intact. Storage is scoped to
//! the workspace database; see [`session`] for the record model.

pub mod session;