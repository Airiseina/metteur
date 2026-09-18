//! The `EditFile` tool: anchored, atomic, multi-block file editing.
//!
//! The tool exists to make LLM edits land on the intended text with high
//! reliability. Four properties carry that:
//!
//! * **Content anchoring** — edits locate their target by exact text, never by
//!   line number, so a miscount cannot corrupt an unrelated region.
//! * **Atomicity** — every block must resolve before anything is written; a
//!   single bad block leaves the file untouched.
//! * **Normalized matching** — trailing whitespace and indentation differences
//!   (the two most common transcription errors) still match, with the
//!   replacement re-indented to the file's actual indentation.
//! * **Actionable diagnostics** — a miss reports the closest region with line
//!   numbers; an ambiguous match lists every candidate line.

use async_trait::async_trait;
use metteur_shared::Value;
use similar::{ChangeTag, TextDiff};

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::workspace::fs::WorkspaceFs;

use crate::registry::tool::Tool;

use super::Args;

/// Byte budget for the diff returned to the model.
const DIFF_MAX_BYTES: usize = 4 * 1024;

/// Lines of surrounding context included in a "not found" diagnostic.
const DIAGNOSTIC_CONTEXT_LINES: usize = 3;

/// One requested replacement block.
#[derive(Debug, Clone, PartialEq)]
pub struct EditBlock {
    /// Text to find (content-anchored, never line numbers).
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring a unique match.
    pub replace_all: bool,
}

/// How a block matched the file text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchLevel {
    /// Exact substring match.
    Exact,
    /// Matched after ignoring trailing whitespace on each line.
    TrailingWhitespace,
    /// Matched after removing the common leading indentation.
    Indentation,
}

impl MatchLevel {
    /// Short label used in the result summary.
    fn label(self) -> &'static str {
        match self {
            MatchLevel::Exact => "exact",
            MatchLevel::TrailingWhitespace => "trimmed",
            MatchLevel::Indentation => "indent-normalized",
        }
    }
}

/// A resolved replacement ready to apply.
#[derive(Debug, Clone, PartialEq)]
struct ResolvedEdit {
    /// Byte range in the working buffer.
    start: usize,
    /// Byte range end (exclusive) in the working buffer.
    end: usize,
    /// Text to splice in, already re-indented when needed.
    replacement: String,
    /// How the block was located.
    level: MatchLevel,
    /// 1-based line of the match start, for diagnostics and summaries.
    line: usize,
    /// Whether every occurrence is replaced.
    replace_all: bool,
}

/// Edits a file by anchored search/replace blocks.
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "EditFile"
    }

    fn description(&self) -> &str {
        "Edits an existing file with anchored search/replace blocks. Each \
         block's `old_string` must appear in the file (read it with ReadFile \
         first); never include line-number prefixes in `old_string`. All \
         blocks are applied atomically: if any block fails to match, the file \
         is left unchanged and the error lists the closest region. Use \
         `replace_all` when the text legitimately appears more than once. \
         Matching tolerates trailing-whitespace and indentation differences. \
         Returns the resulting diff."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit." },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Replacement blocks, applied in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "Exact text to find (without line numbers)." },
                            "new_string": { "type": "string", "description": "Replacement text." },
                            "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
                        },
                        "required": ["old_string", "new_string"]
                    }
                },
                "expected_sha256": {
                    "type": "string",
                    "description": "Optional guard: the file hash reported by ReadFile. The edit is rejected if the file changed since."
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn max_result_bytes(&self) -> usize {
        8 * 1024
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let path = a
            .string("path", 0)
            .ok_or_else(|| DaemonError::Execution("EditFile requires a path".to_string()))?;
        let expected = a.string("expected_sha256", 2);
        let blocks = parse_blocks(a.get("edits", 1))?;
        if blocks.is_empty() {
            return Err(DaemonError::Execution("EditFile requires at least one edit".to_string()));
        }

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let resolved = fs.resolve_existing(&path)?;
        if resolved.is_dir() {
            return Err(DaemonError::Execution(format!("{path} is a directory")));
        }
        let bytes = std::fs::read(&resolved)?;
        let original = String::from_utf8(bytes).map_err(|_| {
            DaemonError::Execution(format!(
                "{path} is not valid UTF-8; EditFile only edits text files"
            ))
        })?;

        if let Some(expected) = expected.as_deref().filter(|s| !s.is_empty()) {
            let actual = super::fs_tools::short_hash(original.as_bytes());
            if !actual.starts_with(expected) && !expected.starts_with(&actual) {
                return Err(DaemonError::Execution(format!(
                    "{path} changed since it was read (expected sha256 {expected}, found {actual}); \
                     read it again and re-apply the edit"
                )));
            }
        }

        let style = line_style(&original);
        let normalized = to_lf(&original);
        let (edited, report) = apply_blocks(&normalized, &blocks)?;
        if edited == normalized {
            return Ok(Value::String(format!(
                "No change: the replacement text is identical to the current content ({}).",
                report.summary()
            )));
        }

        let output = from_lf(&edited, style);
        ctx.transaction_log.record_file_write(
            resolved.clone(),
            Some(original.clone().into_bytes()),
            output.clone().into_bytes(),
        );
        fs.write(&path, output.as_bytes())?;
        ctx.note_file_mutation(&resolved);

        let diff = render_diff(&normalized, &edited);
        Ok(Value::String(format!(
            "Applied {}. sha256: {}\n{diff}",
            report.summary(),
            super::fs_tools::short_hash(output.as_bytes())
        )))
    }
}

/// Parses the `edits` argument into replacement blocks.
fn parse_blocks(raw: Option<Value>) -> DaemonResult<Vec<EditBlock>> {
    let json = match raw {
        Some(Value::Json(value)) => value,
        Some(Value::String(text)) => serde_json::from_str(&text).map_err(|err| {
            DaemonError::Execution(format!("EditFile edits must be a JSON array: {err}"))
        })?,
        Some(Value::List(items)) => {
            return items
                .iter()
                .map(|item| match item {
                    Value::Json(value) => parse_block(value),
                    _ => Err(DaemonError::Execution(
                        "EditFile edit entries must be objects".to_string(),
                    )),
                })
                .collect();
        }
        _ => return Err(DaemonError::Execution("EditFile requires edits".to_string())),
    };
    let array = json.as_array().ok_or_else(|| {
        DaemonError::Execution("EditFile edits must be a JSON array".to_string())
    })?;
    array.iter().map(parse_block).collect()
}

/// Parses one replacement block.
fn parse_block(value: &serde_json::Value) -> DaemonResult<EditBlock> {
    let old_string = value
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DaemonError::Execution("each edit requires old_string".to_string()))?;
    let new_string = value
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DaemonError::Execution("each edit requires new_string".to_string()))?;
    if old_string.is_empty() {
        return Err(DaemonError::Execution(
            "old_string must not be empty; provide the exact text to replace".to_string(),
        ));
    }
    let replace_all = value.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(EditBlock {
        old_string: old_string.to_string(),
        new_string: new_string.to_string(),
        replace_all,
    })
}

/// The line-ending style and trailing-newline state of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineStyle {
    /// The file uses CRLF line endings.
    crlf: bool,
    /// The file ends with a newline.
    trailing_newline: bool,
}

/// Detects the dominant line style of the original text.
fn line_style(text: &str) -> LineStyle {
    LineStyle {
        crlf: text.contains("\r\n"),
        trailing_newline: text.ends_with('\n'),
    }
}

/// Normalizes CRLF to LF for matching.
fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Restores the original line style after editing.
fn from_lf(text: &str, style: LineStyle) -> String {
    let mut out = text.to_string();
    if style.crlf {
        out = out.replace("\n", "\r\n");
    }
    if style.trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    } else if !style.trailing_newline && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Applies every block to a working buffer, failing fast without mutating.
///
/// Each block resolves against the buffer produced by its predecessors, so a
/// later edit may target text an earlier one just introduced.
fn apply_blocks(
    text: &str,
    blocks: &[EditBlock],
) -> DaemonResult<(String, EditReport)> {
    let mut buffer = text.to_string();
    let mut resolved = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match resolve_block(&buffer, block) {
            Ok(edits) => {
                for edit in edits.iter().rev() {
                    buffer.replace_range(edit.start..edit.end, &edit.replacement);
                }
                resolved.extend(edits);
            }
            Err(err) => {
                return Err(DaemonError::Execution(format!(
                    "edit #{} failed: {err}",
                    index + 1
                )));
            }
        }
    }
    Ok((buffer, EditReport { edits: resolved }))
}

/// A located occurrence of an anchor in the buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Located {
    /// Byte range start.
    start: usize,
    /// Byte range end (exclusive).
    end: usize,
    /// How the anchor was matched.
    level: MatchLevel,
}

/// Locates one block in the buffer, returning its concrete replacements.
fn resolve_block(text: &str, block: &EditBlock) -> DaemonResult<Vec<ResolvedEdit>> {
    let ranges = find_ranges(text, &block.old_string);
    if ranges.is_empty() {
        return Err(DaemonError::Execution(not_found_diagnostic(text, &block.old_string)));
    }
    if ranges.len() > 1 && !block.replace_all {
        return Err(DaemonError::Execution(ambiguous_diagnostic(text, &block.old_string, &ranges)));
    }
    let selected = if block.replace_all {
        ranges
    } else {
        ranges.into_iter().take(1).collect()
    };
    let anchor_indent = leading_whitespace(&block.old_string);
    Ok(selected
        .into_iter()
        .map(|located| {
            // A line-anchored match (levels 2 and 3) includes the file's own
            // indentation in its span, so the replacement must reproduce it;
            // a substring match starts after it and leaves it in place.
            let line_start = line_start_of(text, located.start);
            let file_indent = leading_whitespace(&text[line_start..]);
            let replacement = reindent_replacement(
                &block.new_string,
                &anchor_indent,
                &file_indent,
                located.start == line_start,
            );
            ResolvedEdit {
                start: located.start,
                end: located.end,
                replacement,
                level: located.level,
                line: line_of(text, located.start),
                replace_all: block.replace_all,
            }
        })
        .collect())
}

/// Finds every occurrence of `needle` with progressively looser matching.
///
/// Matches are tried in order of strictness and the first level that produces
/// any match wins, so a file containing both an exact and a whitespace-altered
/// copy is edited exactly, not loosely.
fn find_ranges(text: &str, needle: &str) -> Vec<Located> {
    // L1: exact substring.
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = text[cursor..].find(needle) {
        let start = cursor + offset;
        out.push(Located {
            start,
            end: start + needle.len(),
            level: MatchLevel::Exact,
        });
        cursor = start + needle.len();
    }
    if !out.is_empty() {
        return out;
    }

    // L2: compare with trailing whitespace removed per line.
    if let Some(ranges) = find_line_normalized(text, needle, false) {
        return ranges;
    }
    // L3: also ignore leading indentation.
    find_line_normalized(text, needle, true).unwrap_or_default()
}

/// Line-wise normalized search used by levels 2 and 3.
///
/// `ignore_indent` additionally strips each line's leading whitespace before
/// comparing, so a block the model transcribed flush-left still matches a
/// deeply indented region.
fn find_line_normalized(text: &str, needle: &str, ignore_indent: bool) -> Option<Vec<Located>> {
    let needle_lines: Vec<&str> = split_lines(needle);
    let needle_norm: Vec<String> =
        needle_lines.iter().map(|line| normalize_line(line, ignore_indent)).collect();
    if needle_norm.iter().all(|line| line.is_empty()) {
        return None;
    }
    let text_lines: Vec<&str> = split_lines(text);
    let text_norm: Vec<String> =
        text_lines.iter().map(|line| normalize_line(line, ignore_indent)).collect();

    let level =
        if ignore_indent { MatchLevel::Indentation } else { MatchLevel::TrailingWhitespace };
    let mut out = Vec::new();
    let last_start = text_lines.len().saturating_sub(needle_lines.len());
    for start_line in 0..=last_start {
        let window = &text_norm[start_line..start_line + needle_norm.len()];
        if window != needle_norm.as_slice() {
            continue;
        }
        let (start, end) = span_of_lines(text, &text_lines, start_line, needle_lines.len());
        out.push(Located {
            start,
            end,
            level,
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Splits text into lines, tolerating a trailing newline.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

/// Normalizes one line for comparison.
fn normalize_line(line: &str, strip_indent: bool) -> String {
    let trimmed = line.trim_end();
    if strip_indent {
        trimmed.trim_start().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Byte span covering `count` lines starting at `start_line`.
fn span_of_lines(text: &str, lines: &[&str], start_line: usize, count: usize) -> (usize, usize) {
    let start: usize = lines[..start_line].iter().map(|line| line.len() + 1).sum();
    let body: usize = lines[start_line..start_line + count].iter().map(|line| line.len()).sum();
    // Inner newlines of the matched block count too (count - 1 of them).
    let end = (start + body + count.saturating_sub(1)).min(text.len());
    (start, end)
}

/// Returns the leading whitespace of the first line of `text`.
fn leading_whitespace(text: &str) -> String {
    text.split('\n').next().unwrap_or("").chars().take_while(|c| c.is_whitespace()).collect()
}

/// Returns the byte offset at which the line containing `offset` starts.
fn line_start_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].rfind('\n').map(|index| index + 1).unwrap_or(0)
}

/// Re-indents a replacement so it lands at the anchor's real indentation.
///
/// Every line is re-based onto `file_indent`: a line the model authored with
/// its own leading whitespace relative to the anchor keeps that relative depth,
/// while a flush-left line is lifted onto the anchor's indentation. When
/// `prefix_first` is set the match began at the start of a line and included
/// the file's indentation, so the first line needs re-basing too.
fn reindent_replacement(
    text: &str,
    anchor_indent: &str,
    file_indent: &str,
    prefix_first: bool,
) -> String {
    if file_indent.is_empty() && anchor_indent.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim().is_empty() {
            out.push_str(line);
            continue;
        }
        if index == 0 && !prefix_first {
            out.push_str(line);
            continue;
        }
        let line_indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        let relative = line_indent.strip_prefix(anchor_indent).unwrap_or(&line_indent);
        out.push_str(file_indent);
        out.push_str(relative);
        out.push_str(&line[line_indent.len()..]);
    }
    out
}

/// Builds the "no match" diagnostic: the anchor plus the closest region.
fn not_found_diagnostic(text: &str, needle: &str) -> String {
    let mut message = String::from(
        "old_string was not found. The file may have changed, the text may be \
         indented differently, or the anchor may be incomplete.",
    );
    if let Some((line, snippet)) = closest_region(text, needle) {
        message.push_str(&format!(
            "\nClosest region (line {line}):\n{snippet}\nRe-read the file and copy the exact text."
        ));
    } else {
        message.push_str("\nRead the file again to confirm its current content.");
    }
    message
}

/// Builds the "multiple matches" diagnostic listing candidate lines.
fn ambiguous_diagnostic(text: &str, needle: &str, ranges: &[Located]) -> String {
    let mut lines: Vec<String> =
        ranges.iter().map(|located| line_of(text, located.start).to_string()).collect();
    lines.sort();
    lines.dedup();
    let preview = needle.lines().next().unwrap_or("").trim();
    format!(
        "old_string matches {} locations (lines {}). Add surrounding context to make it \
         unique, or set replace_all=true to replace all of them. First line of the anchor: \
         \"{preview}\"",
        ranges.len(),
        lines.join(", ")
    )
}

/// 1-based line number containing `offset`.
fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

/// Finds the region most similar to `needle`, as `(line, numbered snippet)`.
fn closest_region(text: &str, needle: &str) -> Option<(usize, String)> {
    let needle_first = needle.lines().next().unwrap_or("").trim();
    if needle_first.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut best: Option<(f32, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let score = similar::TextDiff::from_chars(needle_first, line.trim()).ratio();
        if best.map(|(best_score, _)| score > best_score).unwrap_or(true) {
            best = Some((score, index));
        }
    }
    let (_, index) = best?;
    let start = index.saturating_sub(DIAGNOSTIC_CONTEXT_LINES);
    let end = (index + DIAGNOSTIC_CONTEXT_LINES + 1).min(lines.len());
    let width = end.to_string().len();
    let snippet = lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let number = start + offset + 1;
            format!("{number:>width$}| {line}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some((index + 1, snippet))
}

/// Renders a unified-style diff between the two buffers.
fn render_diff(before: &str, after: &str) -> String {
    let diff = TextDiff::from_lines(before, after);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => continue,
        };
        out.push_str(sign);
        out.push_str(change.value().trim_end_matches('\n'));
        out.push('\n');
        if out.len() > DIFF_MAX_BYTES {
            out.push_str("... [diff truncated]\n");
            break;
        }
    }
    if out.is_empty() {
        "No textual difference.".to_string()
    } else {
        out
    }
}

/// Summary of the applied edits.
struct EditReport {
    edits: Vec<ResolvedEdit>,
}

impl EditReport {
    /// Human-readable summary line for the tool result.
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        for edit in &self.edits {
            let scope = if edit.replace_all { "all matches" } else { "1 block" };
            parts.push(format!("line {} ({}, {})", edit.line, edit.level.label(), scope));
        }
        format!("{} edit(s): {}", self.edits.len(), parts.join(", "))
    }
}
