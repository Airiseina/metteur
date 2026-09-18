//! Approximate token accounting.
//!
//! The daemon has no bundled tokenizer: providers differ, model families
//! differ, and shipping several BPE tables would dwarf the benefit. Instead
//! the estimators below are deliberately conservative — they over-count rather
//! than under-count, so window guards trigger early instead of after a request
//! has already overflowed. Real usage figures always come from the provider
//! response.

use super::{ContextManager, ContentBlock};

/// Average characters per token for ASCII prose and source code.
const ASCII_CHARS_PER_TOKEN: usize = 4;

/// Estimates the token count of a single text.
///
/// ASCII runs are divided by [`ASCII_CHARS_PER_TOKEN`]; every non-ASCII
/// character counts as a full token, which approximates CJK text (roughly one
/// token per character) far better than a flat character division.
pub fn estimate_tokens(text: &str) -> u64 {
    let ascii = text.chars().filter(|c| c.is_ascii()).count();
    let non_ascii = text.chars().count() - ascii;
    (ascii as u64).div_ceil(ASCII_CHARS_PER_TOKEN as u64) + non_ascii as u64
}

/// Estimates the tokens of one message, including tool calls.
pub fn estimate_message(message: &super::Message) -> u64 {
    let mut total: u64 = 0;
    for block in &message.content {
        total += match block {
            ContentBlock::Text(text) => estimate_tokens(text),
            ContentBlock::Thinking {
                text,
                ..
            } => estimate_tokens(text),
            ContentBlock::RedactedThinking {
                ..
            } => 0,
        };
    }
    for call in &message.tool_calls {
        total += estimate_tokens(&call.name) + estimate_tokens(&call.arguments.to_string());
    }
    total
}

/// Estimates the tokens of a whole context (system fragments + messages).
pub fn estimate_context_tokens(context: &ContextManager) -> u64 {
    let system: u64 =
        context.system_fragments.iter().map(|f| estimate_tokens(&f.content)).sum();
    let messages: u64 = context.messages.iter().map(estimate_message).sum();
    system + messages
}
