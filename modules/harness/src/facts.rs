//! Runtime facts the harness renders into the prompt.

use std::path::PathBuf;

/// Facts about the runtime that the harness renders into the prompt.
///
/// Everything here is supplied by the caller rather than read from the
/// process: a prompt that embeds the current date or platform can only be
/// assembled predictably (and tested) when those values arrive as data.
///
/// # Stability
///
/// `workspace_root` and `platform` stay constant for the lifetime of a
/// session; `today` (and the model that travels with it) change at most once a
/// day. The assembly orders fragments so the volatile part renders last and
/// invalidates as little of the provider prefix cache as possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFacts {
    /// Absolute workspace root shown to the model.
    pub workspace_root: PathBuf,
    /// Current date as `YYYY-MM-DD`.
    pub today: String,
    /// Default model label (`key` or `key (api_type)`), when one is configured.
    pub model: Option<String>,
}

impl EnvFacts {
    /// Builds facts for a workspace at a point in time.
    ///
    /// `platform` is derived from the compilation target, which is the platform
    /// the daemon runs on — the one whose shell and paths the model must
    /// reason about.
    pub fn new(workspace_root: impl Into<PathBuf>, today: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            today: today.into(),
            model: None,
        }
    }

    /// Attaches the default model label.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// The platform string rendered into the prompt, e.g. `windows (x86_64)`.
    pub fn platform(&self) -> String {
        format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
    }
}
