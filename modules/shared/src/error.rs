//! Shared error types used across crates.

use thiserror::Error;

/// Errors that can occur in shared components.
#[derive(Debug, Error)]
pub enum SharedError {
    /// An invalid URI was provided.
    #[error("invalid uri: {0}")]
    InvalidUri(String),

    /// A filesystem operation failed.
    #[error("filesystem error: {0}")]
    Fs(#[from] std::io::Error),

    /// A value could not be serialized or deserialized.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A required resource was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An operation could not be performed.
    #[error("invalid: {0}")]
    Invalid(String),

    /// The requested operation is not supported.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Convenience alias for shared results.
pub type SharedResult<T> = Result<T, SharedError>;
