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
//! - `ToolCallUpdate` → mutates existing `Act`'s replacement fields
//!   (title/content/raw input/status), or synthesizes an `Act` if the
//!   update arrives before the creation
//! - `Plan` → `TraceKind::Think` (summary)
//!
//! ## Deliberate no-ops
//! - `CurrentModeUpdate`, `AvailableCommandsUpdate`, `UsageUpdate` —
//!   these are session-scoped and handled by the caller (for brain
//!   view) or irrelevant (for per-executor traces).
//! - Any future variant not listed above is a no-op.

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};
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
    pub skip_plan_trace: bool,
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
            let status = map_initial_status(tc.status, tc.raw_output.as_ref(), ctx.agent_kind);
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
                let update_text = tcu
                    .fields
                    .content
                    .as_ref()
                    .and_then(|content| extract_tool_call_text(content))
                    .or_else(|| tcu.fields.raw_input.as_ref().map(format_tool_args));
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
                if let TraceKind::Act {
                    tool,
                    input,
                    status,
                    ..
                } = &mut act_entry.kind
                {
                    if let Some(title) = tcu.fields.title.as_deref() {
                        *tool = replace_tool_title_preserving_indent(tool, title);
                    }
                    if let Some(raw_input) = tcu.fields.raw_input.as_ref() {
                        *input = adapter::format_input(raw_input, ctx.agent_kind);
                    }
                    *status = new_status;
                }
                if let Some(text) = update_text {
                    act_entry.text = text;
                }
                trace.mark_dirty_from_for_update(idx);
            } else if tcu.fields.title.is_some() || tcu.fields.kind.is_some() {
                tracing::debug!(
                    id = ?tcu.tool_call_id,
                    "ToolCallUpdate before ToolCall; synthesizing Act"
                );
                let tool = tcu.fields.title.clone().unwrap_or_else(|| "unknown".into());
                let family = match (tcu.fields.title.as_deref(), tcu.fields.kind) {
                    (Some(title), Some(kind)) => {
                        adapter::classify_tool_parts(title, kind, ctx.agent_kind)
                    }
                    _ => adapter::ToolFamily::Unknown,
                };
                let input = tcu
                    .fields
                    .raw_input
                    .as_ref()
                    .map(|raw_input| adapter::format_input(raw_input, ctx.agent_kind))
                    .unwrap_or(adapter::ToolInputDisplay::Empty);
                let status = map_initial_status(
                    tcu.fields.status.unwrap_or(ToolCallStatus::Pending),
                    tcu.fields.raw_output.as_ref(),
                    ctx.agent_kind,
                );
                let text = tcu
                    .fields
                    .content
                    .as_ref()
                    .and_then(|content| extract_tool_call_text(content))
                    .or_else(|| tcu.fields.raw_input.as_ref().map(format_tool_args))
                    .unwrap_or_default();
                trace.push(TraceEntry {
                    kind: TraceKind::Act {
                        tool,
                        family,
                        input,
                        tool_call_id: Some(tcu.tool_call_id.clone()),
                        status,
                    },
                    text,
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
            if ctx.skip_plan_trace {
                return;
            }
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

fn replace_tool_title_preserving_indent(existing: &str, title: &str) -> String {
    let indent_len = existing.len() - existing.trim_start().len();
    format!("{}{}", &existing[..indent_len], title)
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
            ToolCallContent::Content(cb) => match &cb.content {
                spur_acp::ContentBlock::Text(tc) => out.push_str(&tc.text),
                spur_acp::ContentBlock::Image(image) => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&format_image_content(image));
                }
                _ => {}
            },
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

fn format_image_content(image: &agent_client_protocol::schema::ImageContent) -> String {
    let decoded_len = STANDARD.decode(&image.data).ok().map(|bytes| bytes.len());
    let size = decoded_len
        .map(|len| format!("{len} bytes"))
        .unwrap_or_else(|| format!("{} base64 chars", image.data.len()));

    match image.uri.as_deref().filter(|uri| !uri.trim().is_empty()) {
        Some(uri) => format!("[image: {}, uri: {}, data: {}]", image.mime_type, uri, size),
        None => format!("[image: {}, data: {}]", image.mime_type, size),
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        Content, ImageContent, ToolCall, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    };
    use spur_acp::{
        ContentBlock, SessionNotification, SessionUpdate, TextContent, ToolCallContent, ToolCallId,
        ToolCallStatus,
    };
    use std::collections::HashMap;

    fn ctx_for<'a>(
        tool_depth: &'a mut HashMap<String, u8>,
        agent_kind: AgentKind,
    ) -> DispatchCtx<'a, impl Fn() -> String> {
        DispatchCtx {
            agent_name: "kimi",
            agent_kind,
            now_stamp: || "10:00".to_string(),
            tool_depth,
            skip_plan_trace: false,
        }
    }

    fn ctx<'a>(tool_depth: &'a mut HashMap<String, u8>) -> DispatchCtx<'a, impl Fn() -> String> {
        ctx_for(tool_depth, AgentKind::Generic)
    }

    fn text_tool_content(text: &str) -> ToolCallContent {
        ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new(text))))
    }

    fn image_tool_content(data: &str, mime_type: &str, uri: Option<&str>) -> ToolCallContent {
        let mut image = ImageContent::new(data, mime_type);
        if let Some(uri) = uri {
            image = image.uri(uri.to_string());
        }
        ToolCallContent::Content(Content::new(ContentBlock::Image(image)))
    }

    #[test]
    fn tool_call_update_replaces_existing_act_title_and_content() {
        let id = ToolCallId::new("call-kimi");
        let mut trace = ReactTrace::new();
        let mut tool_depth = HashMap::new();
        let mut ctx = ctx(&mut tool_depth);

        let mut call = ToolCall::new(id.clone(), "ReadFile");
        call.status = ToolCallStatus::InProgress;
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCall(call), &mut ctx);

        let update = ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("ReadFile: Cargo.toml")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content(
                    r#"{"path": "/Volumes/Projects/spur/Cargo.toml"}"#,
                )]),
        );
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCallUpdate(update), &mut ctx);

        let entries = trace.entries_for_test();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0].kind, TraceKind::Act { tool, .. } if tool == "ReadFile: Cargo.toml"),
            "expected Kimi's later ToolCallUpdate title to replace the initial tool title"
        );
        assert!(
            entries[0].text.contains("Cargo.toml"),
            "expected Kimi's later ToolCallUpdate content to be displayed, got {:?}",
            entries[0].text
        );
    }

    #[test]
    fn tool_call_update_before_tool_call_synthesizes_act_with_content() {
        let id = ToolCallId::new("call-kimi-before");
        let mut trace = ReactTrace::new();
        let mut tool_depth = HashMap::new();
        let mut ctx = ctx(&mut tool_depth);

        let update = ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("ReadFile: Cargo.toml")
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content(
                    r#"{"path": "/Volumes/Projects/spur/Cargo.toml"}"#,
                )]),
        );
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCallUpdate(update), &mut ctx);

        let entries = trace.entries_for_test();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0].kind, TraceKind::Act { tool, .. } if tool == "ReadFile: Cargo.toml"),
            "expected synthesized Act to use ToolCallUpdate title"
        );
        assert!(
            entries[0].text.contains("Cargo.toml"),
            "expected synthesized Act to keep ToolCallUpdate content, got {:?}",
            entries[0].text
        );
    }

    #[test]
    fn tool_call_update_before_tool_call_uses_update_kind_for_family() {
        let id = ToolCallId::new("call-before-kind");
        let mut trace = ReactTrace::new();
        let mut tool_depth = HashMap::new();
        let mut ctx = ctx(&mut tool_depth);

        let update = ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("Delegating to agent 'generalist'")
                .kind(ToolKind::Think)
                .status(ToolCallStatus::InProgress)
                .content(vec![text_tool_content("agent: generalist")]),
        );
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCallUpdate(update), &mut ctx);

        let entries = trace.entries_for_test();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(
                &entries[0].kind,
                TraceKind::Act {
                    family: adapter::ToolFamily::Think,
                    ..
                }
            ),
            "expected synthesized Act to preserve ToolCallUpdate kind"
        );
    }

    #[test]
    fn tool_call_update_with_image_content_renders_image_reference() {
        let id = ToolCallId::new("ig-1");
        let mut trace = ReactTrace::new();
        let mut tool_depth = HashMap::new();
        let mut ctx = ctx(&mut tool_depth);

        let update = ToolCallUpdate::new(
            id,
            ToolCallUpdateFields::new()
                .title("Image generation")
                .kind(ToolKind::Other)
                .status(ToolCallStatus::Completed)
                .content(vec![image_tool_content(
                    "Zm9v",
                    "image/png",
                    Some("/tmp/ig-1.png"),
                )]),
        );
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCallUpdate(update), &mut ctx);

        let entries = trace.entries_for_test();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].text.contains("image/png"),
            "expected image mime type to be visible, got {:?}",
            entries[0].text
        );
        assert!(
            entries[0].text.contains("/tmp/ig-1.png"),
            "expected image URI to be visible, got {:?}",
            entries[0].text
        );
        assert!(
            entries[0].text.contains("3 bytes"),
            "expected decoded image data size to be visible, got {:?}",
            entries[0].text
        );
    }

    #[test]
    fn gemini_delegate_act_keeps_compact_display_text_after_standardization() {
        let mut standardizer = adapter::SessionEventStandardizer::for_agent(AgentKind::Gemini);
        let standardized = standardizer.standardize(SessionNotification::new(
            "session",
            SessionUpdate::ToolCall(
                ToolCall::new("invoke-agent", "Delegating to agent 'generalist'")
                    .kind(ToolKind::Think)
                    .status(ToolCallStatus::InProgress),
            ),
        ));

        let mut trace = ReactTrace::new();
        let mut tool_depth = HashMap::new();
        let mut ctx = ctx_for(&mut tool_depth, AgentKind::Gemini);
        dispatch_session_update(&mut trace, &standardized.update, &mut ctx);

        let entries = trace.entries_for_test();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].text, "agent: generalist",
            "compact/activity trace uses TraceEntry.text for Act rows"
        );
        assert!(
            matches!(&entries[0].kind, TraceKind::Act { input: adapter::ToolInputDisplay::Text(text), .. } if text == "agent: generalist"),
            "expected Gemini adapter input to carry the delegated agent name"
        );

        let rendered: String = trace
            .build_compact_lines_for_tests(80)
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref().to_string())
            })
            .collect();
        assert!(
            rendered.contains("agent: generalist"),
            "compact/activity row should not be blank: {rendered:?}"
        );
    }
}
