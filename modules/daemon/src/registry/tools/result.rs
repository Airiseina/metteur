//! Uniform result truncation for tool outputs.
//!
//! Every tool result that reaches an LLM or a blueprint pin is capped here so
//! a single call cannot flood the context. Truncation keeps the head and the
//! tail (both are informative: headers/summaries at the top, errors and
//! exit codes at the bottom) and marks the elision explicitly so the model
//! knows the result is incomplete.

/// Fraction of the budget reserved for the head of an oversized result.
const HEAD_SHARE: f64 = 0.6;

/// Caps `text` to `max_bytes`, keeping a head and tail slice.
///
/// `max_bytes == 0` disables the cap. Cuts always land on UTF-8 boundaries, so
/// the result is valid text regardless of the input encoding mix.
pub fn truncate_result(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }
    // Reserve room for the marker itself so the result respects the budget.
    let marker_budget = 128.min(max_bytes / 4);
    let content_budget = max_bytes.saturating_sub(marker_budget);
    let head_budget = (content_budget as f64 * HEAD_SHARE) as usize;
    let tail_budget = content_budget.saturating_sub(head_budget);

    let head_end = floor_char_boundary(text, head_budget);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_budget));
    let omitted = tail_start.saturating_sub(head_end);
    if omitted == 0 {
        return text.to_string();
    }
    format!(
        "{}\n... [truncated {} of {} bytes; narrow the request or use offset/limit] ...\n{}",
        &text[..head_end],
        omitted,
        text.len(),
        &text[tail_start..]
    )
}

/// Largest index `<= index` that lies on a UTF-8 boundary.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Smallest index `>= index` that lies on a UTF-8 boundary.
fn ceil_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}
