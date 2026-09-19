//! Permission modes: how much the agent may do before asking.
//!
//! One axis with three stops, chosen per conversation and defaulted from the
//! workspace configuration:
//!
//! | Mode | File edits | Commands |
//! |---|---|---|
//! | [`PermissionMode::Ask`] | confirm each one | confirm each one |
//! | [`PermissionMode::Sandbox`] | run inside the workspace, confirm outside | policy whitelist runs, everything else confirms |
//! | [`PermissionMode::Full`] | run, with a model reviewing risky calls | run, with a model reviewing risky calls |
//!
//! The modes name *who decides*, not *what is allowed*: the file-system jail
//! and the command policy still apply in every mode, and `full` never silently
//! ignores a refusal — it replaces the user's answer with a model's, and falls
//! back to the user when the model cannot answer.

use serde::{Deserialize, Serialize};

/// Who answers an authorization question.
///
/// `Sandbox` is the default: it is the least surprising starting point, since it
/// neither asks about every workspace-internal edit nor skips confirmation for
/// commands the policy does not recognize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Every mutating operation is confirmed by the user.
    Ask,
    /// Workspace-internal edits and policy-approved commands run unprompted.
    #[default]
    Sandbox,
    /// Risky operations go to the model for automatic approval.
    Full,
}

impl PermissionMode {
    /// Parses the configuration value, defaulting to [`PermissionMode::Sandbox`].
    ///
    /// An unknown string is not an error: a typo must not make the agent ask
    /// about every step, nor silently grant everything.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ask" | "manual" | "confirm" => Self::Ask,
            "full" | "auto" | "bypass" => Self::Full,
            _ => Self::Sandbox,
        }
    }

    /// The configuration value of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Sandbox => "sandbox",
            Self::Full => "full",
        }
    }

    /// Whether the user must be asked about an operation that is not otherwise
    /// allowed.
    pub fn asks_the_user(self) -> bool {
        !matches!(self, Self::Full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_fall_back_to_the_sandbox() {
        assert_eq!(PermissionMode::parse("ask"), PermissionMode::Ask);
        assert_eq!(PermissionMode::parse(" Full "), PermissionMode::Full);
        assert_eq!(PermissionMode::parse("sandbox"), PermissionMode::Sandbox);
        assert_eq!(PermissionMode::parse("nonsense"), PermissionMode::Sandbox);
        assert_eq!(PermissionMode::parse(""), PermissionMode::Sandbox);
    }

    #[test]
    fn only_full_mode_answers_by_itself() {
        assert!(PermissionMode::Ask.asks_the_user());
        assert!(PermissionMode::Sandbox.asks_the_user());
        assert!(!PermissionMode::Full.asks_the_user());
    }
}
