//! Context manager for assembling and pruning LLM context.
//!
//! The [`ContextManager`] holds system fragments, conversation messages and
//! tool results, and produces the message array sent to the model. It is
//! designed to be cloned freely so that a node can work on an isolated copy
//! without polluting the original context.

use serde::{Deserialize, Serialize};

use super::message::{Message, Role};
use super::tool::ToolResult;

/// A system prompt fragment with a priority and scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemFragment {
    /// Higher priority fragments are placed first.
    pub priority: i32,
    /// The scope this fragment applies to (e.g. a node kind).
    pub scope: String,
    /// The fragment content.
    pub content: String,
}

/// The policy used when evicting low-value tool results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionPolicy {
    /// Evict one-shot results first, then the oldest.
    Default,
}

/// An immutable snapshot of the context produced by [`ContextManager::filter`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// The system fragments.
    pub system_fragments: Vec<SystemFragment>,
    /// The messages.
    pub messages: Vec<Message>,
}

/// A per-region estimate of the context composition, for the audit UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRegion {
    /// The region label (`system`, `user`, `assistant`, `tool`).
    pub region: String,
    /// Estimated characters held by this region.
    pub chars: usize,
    /// Coarse token estimate (`chars / 4`).
    pub tokens: usize,
}

/// Manages the LLM context for a conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextManager {
    /// Ordered system fragments.
    pub system_fragments: Vec<SystemFragment>,
    /// Conversation messages.
    pub messages: Vec<Message>,
    /// Tool results retained for potential eviction.
    pub tool_results: Vec<ToolResult>,
}

impl ContextManager {
    /// Creates a context from system fragments and an initial user prompt.
    pub fn new_from_prompt(system: Vec<SystemFragment>, initial_prompt: impl Into<String>) -> Self {
        Self {
            system_fragments: system,
            messages: vec![Message::text(Role::User, initial_prompt)],
            tool_results: Vec::new(),
        }
    }

    /// Mixes a tool result into the context (side effect).
    ///
    /// The result is appended to the messages as a tool message and retained
    /// in `tool_results` for later eviction.
    pub fn mix_in_tool_result(&mut self, result: ToolResult) {
        let tool_call_id = result.tool_call_id.clone();
        self.messages.push(Message {
            role: Role::Tool,
            content: vec![super::message::ContentBlock::Text(result.content.clone())],
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id),
        });
        self.tool_results.push(result);
    }

    /// Appends a message to the context (side effect).
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Returns an immutable snapshot filtered by `predicate` (no side effect).
    pub fn filter(&self, predicate: impl Fn(&Message) -> bool) -> ContextSnapshot {
        ContextSnapshot {
            system_fragments: self.system_fragments.clone(),
            messages: self.messages.iter().filter(|m| predicate(m)).cloned().collect(),
        }
    }

    /// Evicts low-value tool results according to `policy` (side effect).
    ///
    /// One-shot results are removed first, followed by the oldest results,
    /// until the context is within `target_tool_results` retained results.
    pub fn evict(&mut self, policy: EvictionPolicy, target_tool_results: usize) {
        if self.tool_results.len() <= target_tool_results {
            return;
        }
        let mut evictable: Vec<usize> = (0..self.tool_results.len()).collect();
        match policy {
            EvictionPolicy::Default => {
                evictable.sort_by_key(|&i| {
                    let r = &self.tool_results[i];
                    (r.lifetime != super::tool::ToolResultLifetime::OneShot, r.timestamp)
                });
            }
        }
        let to_remove = self.tool_results.len() - target_tool_results;
        let removed: std::collections::HashSet<usize> =
            evictable.into_iter().take(to_remove).collect();

        let removed_ids: std::collections::HashSet<String> = self
            .tool_results
            .iter()
            .enumerate()
            .filter(|(i, _)| removed.contains(i))
            .map(|(_, r)| r.tool_call_id.clone())
            .collect();

        self.tool_results = self
            .tool_results
            .drain(..)
            .enumerate()
            .filter(|(i, _)| !removed.contains(i))
            .map(|(_, r)| r)
            .collect();
        self.messages.retain(|m: &Message| {
            !(m.role == Role::Tool
                && m.tool_call_id.as_deref().map(|id| removed_ids.contains(id)).unwrap_or(false))
        });
    }

    /// Merge all messages except the most recent `keep_recent` into a single
    /// assistant summary produced by `summarize`; no-op below threshold or
    /// when `summarize` returns `None`.
    ///
    /// Tool results whose id appears only in the removed messages are pruned
    /// from `tool_results`.
    pub fn compress(
        &mut self,
        keep_recent: usize,
        summarize: impl FnOnce(&[Message]) -> Option<String>,
    ) {
        if self.messages.len() <= keep_recent {
            return;
        }
        let split_at = self.messages.len() - keep_recent;
        let Some(summary) = summarize(&self.messages[..split_at]) else {
            return;
        };
        let removed_ids: std::collections::HashSet<String> =
            self.messages[..split_at].iter().filter_map(|m| m.tool_call_id.clone()).collect();
        let kept_ids: std::collections::HashSet<String> =
            self.messages[split_at..].iter().filter_map(|m| m.tool_call_id.clone()).collect();
        // Prune results orphaned by the removal; ids still referenced by a
        // kept message must survive.
        self.tool_results.retain(|r| {
            !(removed_ids.contains(&r.tool_call_id) && !kept_ids.contains(&r.tool_call_id))
        });
        self.messages.drain(..split_at);
        self.messages.insert(0, Message::text(Role::Assistant, summary));
    }

    /// Builds the message array to send to the model.
    ///
    /// Tool results are appended at the end to keep the prefix cache stable.
    pub fn build(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Estimates the context composition by region for the audit UI.
    ///
    /// System fragments form the `system` region; messages are grouped by
    /// [`Role`] into `user`, `assistant` and `tool` regions. Token counts are a
    /// coarse `chars / 4` proxy so the UI can show a live region ratio.
    pub fn usage_report(&self) -> Vec<ContextRegion> {
        let mut chars = std::collections::HashMap::new();
        let sys: usize = self.system_fragments.iter().map(|f| f.content.chars().count()).sum();
        if sys > 0 {
            chars.insert("system".to_string(), sys);
        }
        for message in &self.messages {
            let region = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let text = message.text_content();
            *chars.entry(region.to_string()).or_insert(0) += text.chars().count();
        }
        let mut regions: Vec<ContextRegion> = chars
            .into_iter()
            .map(|(region, chars)| ContextRegion {
                region,
                chars,
                tokens: chars / 4,
            })
            .collect();
        regions.sort_by_key(|a| std::cmp::Reverse(a.tokens));
        regions
    }
}

#[cfg(test)]
mod tests {
    use super::super::tool::{ToolResult, ToolResultLifetime};
    use super::*;

    fn result(id: &str, lifetime: ToolResultLifetime, ts: u64) -> ToolResult {
        ToolResult {
            tool_call_id: id.to_string(),
            content: format!("result {id}"),
            timestamp: ts,
            lifetime,
        }
    }

    #[test]
    fn new_from_prompt_creates_user_message() {
        let ctx = ContextManager::new_from_prompt(vec![], "hello");
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::User);
    }

    #[test]
    fn mix_in_tool_result_appends_message() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.mix_in_tool_result(result("call_1", ToolResultLifetime::OneShot, 1));
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[1].role, Role::Tool);
        assert_eq!(ctx.messages[1].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn filter_has_no_side_effect() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.mix_in_tool_result(result("call_1", ToolResultLifetime::OneShot, 1));
        let snapshot = ctx.filter(|m| m.role == Role::User);
        assert_eq!(snapshot.messages.len(), 1);
        // Original context is unchanged.
        assert_eq!(ctx.messages.len(), 2);
    }

    #[test]
    fn evict_removes_one_shot_first() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.mix_in_tool_result(result("call_1", ToolResultLifetime::OneShot, 1));
        ctx.mix_in_tool_result(result("call_2", ToolResultLifetime::Persistent, 2));
        ctx.evict(EvictionPolicy::Default, 1);
        assert_eq!(ctx.tool_results.len(), 1);
        assert_eq!(ctx.tool_results[0].tool_call_id, "call_2");
        // The evicted tool message is removed too.
        assert_eq!(ctx.messages.len(), 2);
    }

    #[test]
    fn clone_is_deep() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.mix_in_tool_result(result("call_1", ToolResultLifetime::OneShot, 1));
        let mut cloned = ctx.clone();
        cloned.mix_in_tool_result(result("call_2", ToolResultLifetime::OneShot, 2));
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(cloned.messages.len(), 3);
    }

    #[test]
    fn compress_below_threshold_is_noop() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "hello");
        ctx.push_message(Message::text(Role::Assistant, "hi"));
        let mut called = false;
        ctx.compress(5, |_| {
            called = true;
            Some("summary".to_string())
        });
        assert!(!called);
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, Role::User);
    }

    #[test]
    fn compress_merges_into_single_assistant_summary() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "q1");
        ctx.push_message(Message::text(Role::Assistant, "a1"));
        ctx.push_message(Message::text(Role::User, "q2"));
        ctx.compress(1, |older| {
            assert_eq!(older.len(), 2);
            Some(format!("summary of {}", older.len()))
        });
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, Role::Assistant);
        assert_eq!(ctx.messages[0].text_content(), "summary of 2");
        // The most recent message is kept untouched.
        assert_eq!(ctx.messages[1].role, Role::User);
        assert_eq!(ctx.messages[1].text_content(), "q2");
    }

    #[test]
    fn compress_skips_when_summarizer_returns_none() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "q1");
        ctx.push_message(Message::text(Role::Assistant, "a1"));
        ctx.compress(1, |_| None);
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, Role::User);
    }

    #[test]
    fn compress_prunes_orphaned_tool_results() {
        let mut ctx = ContextManager::new_from_prompt(vec![], "q1");
        ctx.mix_in_tool_result(result("call_1", ToolResultLifetime::OneShot, 1));
        ctx.push_message(Message::text(Role::Assistant, "a1"));
        ctx.mix_in_tool_result(result("call_2", ToolResultLifetime::Persistent, 2));
        ctx.push_message(Message::text(Role::Assistant, "done"));
        // Removes [user q1, tool call_1]; keeps [assistant a1, tool call_2, assistant done].
        ctx.compress(3, |_| Some("earlier work".to_string()));
        assert_eq!(ctx.messages.len(), 4);
        assert_eq!(ctx.messages[0].role, Role::Assistant);
        assert_eq!(ctx.messages[0].text_content(), "earlier work");
        assert_eq!(ctx.tool_results.len(), 1);
        assert_eq!(ctx.tool_results[0].tool_call_id, "call_2");
    }
}
