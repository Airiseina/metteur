//! Workspace management.

pub mod lock;
pub mod manager;

pub use manager::{METADATA_DIR, WorkspaceManager};
