//! Shared dispatch from `SessionUpdate` to `ReactTrace` mutations.
//!
//! Both the brain session view (`SessionDetailView::handle_spur_event`,
//! refactored in Task 1.2) and the per-executor worker-stream router
//! (`App::route_worker_notification`, added in Task 1.4) call this
//! module, guaranteeing the Stream tab and brain view derive from the
//! same protocol interpretation.
//!
//! ## Handled variants
//! - `AgentThoughtChunk` → `TraceKind::Think`
//! - `AgentMessageChunk` → `TraceKind::AgentMessage`
//! - `UserMessageChunk` → `TraceKind::UserMessage`
//! - `ToolCall` → `TraceKind::Act` (new)
//! - `ToolCallUpdate` → mutates existing `Act`'s status, or synthesizes
//!   an `Act` if the update arrives before the creation
//! - `Plan` → `TraceKind::Think` (summary)
//!
//! ## Deliberate no-ops
//! - `CurrentModeUpdate`, `AvailableCommandsUpdate`, `UsageUpdate` —
//!   these are session-scoped and handled by the caller (for brain
//!   view) or irrelevant (for per-executor traces).
//! - Any future variant not listed above is a no-op.

use std::collections::HashMap;

use spur_acp::{adapter, AgentKind, ContentBlock, ContentChunk, SessionUpdate, ToolCallStatus};

use super::{map_initial_status, merge_status, ReactTrace, TraceEntry, TraceKind};

/// Caller-provided state needed to construct a `TraceEntry` from a
/// `SessionUpdate`. Fields are passed by reference so the dispatcher
/// remains agnostic to where they live:
/// - `SessionDetailView` holds them on `self`.
/// - `App::route_worker_notification` constructs them per-call via
///   `WorkerStreams::route`.
pub struct DispatchCtx<'a, F: Fn() -> String> {
    pub agent_name: &'a str,
    pub agent_kind: AgentKind,
    pub now_stamp: F,
    pub tool_depth: &'a mut HashMap<String, u8>,
}

/// Mutate `trace` in response to a single `SessionUpdate`.
pub fn dispatch_session_update<F: Fn() -> String>(
    trace: &mut ReactTrace,
    update: &SessionUpdate,
    ctx: &mut DispatchCtx<'_, F>,
) {
    match update {
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    trace.append_think(text, (ctx.now_stamp)());
                }
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    trace.append_message(text, ctx.agent_name, (ctx.now_stamp)());
                }
            }
        }
        SessionUpdate::UserMessageChunk(chunk) => {
            if let ContentBlock::Text(tc) = &chunk.content {
                trace.append_user_message(&tc.text, (ctx.now_stamp)());
            }
        }
        SessionUpdate::ToolCall(tc) => {
            let meta = adapter::extract_tool_meta(tc, ctx.agent_kind);
            let display_name = meta.tool_name.as_deref().unwrap_or(tc.title.as_str());
            let depth = meta
                .parent_tool_use_id
                .as_ref()
                .and_then(|pid| ctx.tool_depth.get(pid).copied())
                .map(|d| d.saturating_add(1).min(8))
                .unwrap_or(0);
            ctx.tool_depth.insert(tc.tool_call_id.0.to_string(), depth);
            let indent = "  ".repeat(depth as usize);
            let tool = format!("{}{}", indent, display_name);
            let family = adapter::classify_tool(tc, ctx.agent_kind);
            let input = tc
                .raw_input
                .as_ref()
                .map(|v| adapter::format_input(v, ctx.agent_kind))
                .unwrap_or(adapter::ToolInputDisplay::Empty);
            let fallback_text = extract_tool_call_text(&tc.content)
                .or_else(|| tc.raw_input.as_ref().map(format_tool_args))
                .unwrap_or_default();
            let status =
                map_initial_status(tc.status, tc.raw_output.as_ref(), ctx.agent_kind);
            trace.push(TraceEntry {
                kind: TraceKind::Act {
                    tool,
                    family,
                    input,
                    tool_call_id: Some(tc.tool_call_id.clone()),
                    status,
                },
                text: fallback_text,
                timestamp: (ctx.now_stamp)(),
                #[cfg(feature = "markdown")]
                markdown: None,
            });
        }
        SessionUpdate::ToolCallUpdate(tcu) => {
            if let Some((idx, act_entry)) = trace.find_act_by_id_mut(&tcu.tool_call_id) {
                let new_status = if let TraceKind::Act { status, .. } = &act_entry.kind {
                    merge_status(
                        status,
                        tcu.fields.status,
                        tcu.fields.raw_output.as_ref(),
                        ctx.agent_kind,
                    )
                } else {
                    return;
                };
                if let TraceKind::Act { status, .. } = &mut act_entry.kind {
                    *status = new_status;
                }
                trace.mark_dirty_from_for_update(idx);
            } else if tcu.fields.title.is_some() || tcu.fields.kind.is_some() {
                tracing::debug!(
                    id = ?tcu.tool_call_id,
                    "ToolCallUpdate before ToolCall; synthesizing Act"
                );
                let tool = tcu.fields.title.clone().unwrap_or_else(|| "unknown".into());
                let family = adapter::ToolFamily::Unknown;
                let input = adapter::ToolInputDisplay::Empty;
                let status = map_initial_status(
                    tcu.fields.status.unwrap_or(ToolCallStatus::Pending),
                    tcu.fields.raw_output.as_ref(),
                    ctx.agent_kind,
                );
                trace.push(TraceEntry {
                    kind: TraceKind::Act {
                        tool,
                        family,
                        input,
                        tool_call_id: Some(tcu.tool_call_id.clone()),
                        status,
                    },
                    text: String::new(),
                    timestamp: (ctx.now_stamp)(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            } else {
                tracing::debug!(
                    id = ?tcu.tool_call_id,
                    "dropping ToolCallUpdate with no matching Act and no title/kind"
                );
            }
        }
        SessionUpdate::Plan(plan) => {
            let text = plan
                .entries
                .iter()
                .map(|e| {
                    let marker = match &e.status {
                        spur_acp::PlanEntryStatus::Completed => "[x]",
                        spur_acp::PlanEntryStatus::InProgress => "[~]",
                        _ => "[ ]",
                    };
                    format!("{} {}", marker, e.content)
                })
                .collect::<Vec<_>>()
                .join("\n");
            trace.push(TraceEntry {
                kind: TraceKind::Think,
                text,
                timestamp: (ctx.now_stamp)(),
                #[cfg(feature = "markdown")]
                markdown: None,
            });
        }
        _ => {}
    }
}

fn extract_text(chunk: &ContentChunk) -> Option<&str> {
    match &chunk.content {
        ContentBlock::Text(tc) => Some(&tc.text),
        _ => None,
    }
}

/// Extract renderable text from a `ToolCallContent` slice.
///
/// Handles all known variants:
/// - `Content` — returns the inner text (non-text blocks silently skipped).
/// - `Diff`    — formats as a truncated unified-style diff (max `DIFF_MAX_LINES` body lines).
/// - `Terminal` — returns a placeholder `[terminal: <id>]`.
/// - Unknown future variants — silently ignored (`ToolCallContent` is `#[non_exhaustive]`).
///
/// Returns `None` if nothing renderable was produced.
fn extract_tool_call_text(content: &[spur_acp::ToolCallContent]) -> Option<String> {
    use spur_acp::ToolCallContent;
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content(cb) => {
                if let spur_acp::ContentBlock::Text(tc) = &cb.content {
                    out.push_str(&tc.text);
                }
                // Non-Text ContentBlock variants (Image, Audio, Resource) silently skipped.
            }
            ToolCallContent::Diff(diff) => {
                out.push_str(&format_diff_truncated(
                    &diff.path.display().to_string(),
                    diff.old_text.as_deref(),
                    &diff.new_text,
                ));
            }
            ToolCallContent::Terminal(term) => {
                out.push_str(&format!("[terminal: {}]", term.terminal_id));
            }
            _ => {
                // ToolCallContent is #[non_exhaustive]; ignore unknown variants.
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

const DIFF_MAX_LINES: usize = 40;

/// Format a diff as a simplified unified-diff string, capped at `DIFF_MAX_LINES` body lines.
fn format_diff_truncated(path: &str, old: Option<&str>, new_: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path));
    out.push_str(&format!("+++ b/{}\n", path));

    let mut body_lines: usize = 0;
    let mut truncated_count: usize = 0;

    if let Some(old_text) = old {
        for line in old_text.lines() {
            if body_lines >= DIFF_MAX_LINES {
                truncated_count += 1;
                continue;
            }
            out.push_str(&format!("-{}\n", line));
            body_lines += 1;
        }
    }
    for line in new_.lines() {
        if body_lines >= DIFF_MAX_LINES {
            truncated_count += 1;
            continue;
        }
        out.push_str(&format!("+{}\n", line));
        body_lines += 1;
    }
    if truncated_count > 0 {
        out.push_str(&format!("... ({} more lines)\n", truncated_count));
    }
    out
}

/// Format tool call args for display. Extracts purpose or key args,
/// falls back to truncated JSON.
fn format_tool_args(input: &serde_json::Value) -> String {
    if input.is_null() {
        return String::new();
    }
    if let Some(obj) = input.as_object() {
        if obj.is_empty() {
            return String::new();
        }
        // Kiro includes __tool_use_purpose — use it if available
        if let Some(purpose) = obj.get("__tool_use_purpose").and_then(|v| v.as_str()) {
            return purpose.to_string();
        }
        // Try common meaningful keys
        for key in &["path", "file", "command", "query", "url", "pattern"] {
            if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
                return format!("{}: {}", key, val);
            }
        }
    }
    // Fallback: truncate JSON to single line
    let s = input.to_string();
    truncate_str(&s, 80)
}

/// Truncate a string to max_len chars, respecting UTF-8 boundaries.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}...", &s[..end])
}
