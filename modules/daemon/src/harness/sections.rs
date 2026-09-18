//! Static harness prompt sections.
//!
//! The texts live in `prompts/*.md` and are embedded at compile time, so a
//! released binary always carries the exact prompt revision it was built with.
//! Editing a section is a prompt change: it invalidates the stored prefix of
//! running sessions once, by design (see `HarnessPrompt::refresh`).

use metteur_shared::llm::SystemFragment;

use super::SCOPE_PREFIX;

/// The agent identity and operating principles.
pub const IDENTITY: &str = include_str!("prompts/identity.md");

/// Tool usage rules.
pub const TOOLS: &str = include_str!("prompts/tools.md");

/// Coding standards.
pub const CODING: &str = include_str!("prompts/coding.md");

/// Plan/todo discipline.
pub const PLAN: &str = include_str!("prompts/plan.md");

/// The summarization prompt for context compression.
pub const COMPRESS: &str = include_str!("prompts/compress.md");

/// Identity renders first.
pub const PRIORITY_IDENTITY: i32 = 100;
/// Tool rules follow the identity.
pub const PRIORITY_TOOLS: i32 = 90;
/// Coding standards.
pub const PRIORITY_CODING: i32 = 80;
/// Plan discipline.
pub const PRIORITY_PLAN: i32 = 70;
/// The environment block.
pub const PRIORITY_ENVIRONMENT: i32 = 60;
/// Project instructions from the workspace.
pub const PRIORITY_PROJECT: i32 = 50;
/// Task-level instructions written on a blueprint node.
pub const PRIORITY_NODE: i32 = 20;
/// Operator-appended instructions, rendered last.
pub const PRIORITY_APPEND: i32 = 10;

/// The constant sections every harness prompt carries.
pub fn static_fragments() -> Vec<SystemFragment> {
    [("identity", PRIORITY_IDENTITY, IDENTITY), ("tools", PRIORITY_TOOLS, TOOLS)]
        .into_iter()
        .chain([("coding", PRIORITY_CODING, CODING), ("plan", PRIORITY_PLAN, PLAN)])
        .map(|(name, priority, content)| SystemFragment {
            priority,
            scope: format!("{SCOPE_PREFIX}{name}"),
            content: content.trim_end().to_string(),
        })
        .collect()
}
