# Stream Tab Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the DetailPane Stream tab render from the same `SessionNotification` stream and the same `react_trace/builder.rs` dispatch as the brain session view, adding only a `compact` render mode for the narrow pane.

**Architecture:** Extract a shared `dispatch_session_update` function from `session_detail.rs` into the `react_trace` module; add a `compact: bool` field on `ReactTrace` with a `render_compact` branch; maintain a `per_executor_traces: HashMap<String, ReactTrace>` on `App`, populated on every `SpurEventBody::WorkerNotification` and looked up by `DetailPane` during render. The lineage projection narrows from "stream_buffer writer" to "card summary counters" without schema change.

**Tech Stack:** Rust 1.85+, ratatui, ACP (`agent-client-protocol`), `spur-acp` / `spur-core` / `spur-tui` crates, `cargo test` / `cargo clippy -- -D warnings`.

**Spec:** `docs/superpowers/specs/2026-04-19-stream-tab-unification-design.md`

---

## File Structure

**New files:**
- `crates/spur-tui/src/components/react_trace/dispatch.rs` — shared `dispatch_session_update(trace, update, ctx)` function extracted from `session_detail.rs`.
- `crates/spur-tui/src/components/react_trace/compact_render.rs` — compact render branch and width-truncation helper.
- `crates/spur-tui/src/worker_streams.rs` — `WorkerStreams` wrapper owning `HashMap<String, ReactTrace>` on `App`.

**Modified files:**
- `crates/spur-acp/src/types.rs` — add `AgentKind::from_name(&str) -> AgentKind`.
- `crates/spur-tui/src/components/react_trace/mod.rs` — add `compact` field, `with_kind_compact` constructor, `render` branch.
- `crates/spur-tui/src/components/react_trace/render.rs` — delegate to `render_compact` when `self.compact`.
- `crates/spur-tui/src/views/session_detail.rs` — replace inline match with call to `dispatch_session_update`.
- `crates/spur-tui/src/components/detail_pane.rs` — accept optional `&ReactTrace` and delegate stream rendering.
- `crates/spur-tui/src/views/dashboard.rs` — thread `&ReactTrace` lookup into `DetailPane::render`.
- `crates/spur-tui/src/app.rs` — own `WorkerStreams`, route `WorkerNotification`, seed from `stream_buffer` on rehydrate.
- `crates/spur-core/src/lineage/projection.rs` — narrow `WorkerNotification` handling to counters only (Phase 3).

---

## Phase 0 — Landing pad (zero behavior change)

### Task 0.1: Add `AgentKind::from_name` parser

**Files:**
- Modify: `crates/spur-acp/src/types.rs` (append `impl AgentKind` block after line 177)
- Test: `crates/spur-acp/src/types.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add at the end of `crates/spur-acp/src/types.rs`:

```rust
#[cfg(test)]
mod agent_kind_tests {
    use super::AgentKind;

    #[test]
    fn from_name_matches_kebab_case_serde_repr() {
        assert_eq!(AgentKind::from_name("claude-stream-json"), AgentKind::ClaudeStreamJson);
        assert_eq!(AgentKind::from_name("claude-code-acp"), AgentKind::ClaudeCodeAcp);
        assert_eq!(AgentKind::from_name("codex-acp"), AgentKind::CodexAcp);
        assert_eq!(AgentKind::from_name("kiro"), AgentKind::Kiro);
        assert_eq!(AgentKind::from_name("generic"), AgentKind::Generic);
    }

    #[test]
    fn from_name_accepts_human_aliases() {
        assert_eq!(AgentKind::from_name("claude"), AgentKind::ClaudeCodeAcp);
        assert_eq!(AgentKind::from_name("Claude Code"), AgentKind::ClaudeCodeAcp);
        assert_eq!(AgentKind::from_name("codex"), AgentKind::CodexAcp);
    }

    #[test]
    fn from_name_unknown_defaults_to_generic() {
        assert_eq!(AgentKind::from_name("ollama-wizard"), AgentKind::Generic);
        assert_eq!(AgentKind::from_name(""), AgentKind::Generic);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp agent_kind_tests`
Expected: FAIL with "no function `from_name`" on all three tests.

- [ ] **Step 3: Implement `from_name`**

Add immediately after the `AgentKind` enum definition (after line 177) in `crates/spur-acp/src/types.rs`:

```rust
impl AgentKind {
    /// Parse an agent identifier string (TOML name, display label, or
    /// serde kebab-case form) into an `AgentKind`. Unknown inputs return
    /// `AgentKind::Generic`.
    ///
    /// Used by the TUI to style per-executor traces and session panes
    /// when only the `ExecutorNode.agent: String` is in hand.
    pub fn from_name(name: &str) -> AgentKind {
        let norm = name.trim().to_ascii_lowercase();
        match norm.as_str() {
            "claude-stream-json" => AgentKind::ClaudeStreamJson,
            "claude-code-acp" | "claude" | "claude code" | "claude-code" => AgentKind::ClaudeCodeAcp,
            "codex-acp" | "codex" => AgentKind::CodexAcp,
            "kiro" => AgentKind::Kiro,
            _ => AgentKind::Generic,
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-acp agent_kind_tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/types.rs
git commit -m "feat(spur-acp): AgentKind::from_name parser with kebab + human aliases"
```

---

### Task 0.2: Add `compact` field and `with_kind_compact` constructor on `ReactTrace`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:27-56` (add field) and `:175-199` (add constructor)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

```rust
#[test]
fn with_kind_compact_sets_compact_flag() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let t = ReactTrace::with_kind_compact(AgentKind::Generic);
    assert!(t.is_compact(), "with_kind_compact should set compact = true");

    let full = ReactTrace::with_kind(AgentKind::Generic);
    assert!(!full.is_compact(), "with_kind should leave compact = false");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui with_kind_compact_sets_compact_flag`
Expected: FAIL — `with_kind_compact`, `is_compact` unresolved.

- [ ] **Step 3: Add field + constructor + accessor**

In `crates/spur-tui/src/components/react_trace/mod.rs`, inside the `ReactTrace` struct (add after `pub(super) observe_collapsed: bool,` near line 45):

```rust
    /// When true, `render` uses the single-line compact branch suitable
    /// for narrow panes. Set at construction via `with_kind_compact`.
    pub(super) compact: bool,
```

In the `impl ReactTrace { pub fn new() -> Self { ... } }` initializer, add `compact: false,` in the struct literal (alongside `observe_collapsed: true,`).

In the `impl ReactTrace` block, add after `with_kind`:

```rust
    /// Create a `ReactTrace` with a compact render mode suitable for
    /// narrow panes (≈40 cols). Disables markdown/mermaid implicitly in
    /// the render branch.
    pub fn with_kind_compact(kind: AgentKind) -> Self {
        Self {
            agent_kind: kind,
            compact: true,
            ..Self::new()
        }
    }

    /// True if this trace was constructed with `with_kind_compact`.
    pub fn is_compact(&self) -> bool {
        self.compact
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui with_kind_compact_sets_compact_flag`
Expected: PASS. Also run: `cargo test -p spur-tui` (nothing else should regress).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): ReactTrace compact flag + with_kind_compact constructor"
```

---

### Task 0.3: Implement compact render branch

**Files:**
- Create: `crates/spur-tui/src/components/react_trace/compact_render.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs` (`mod compact_render;` and render delegation)
- Modify: `crates/spur-tui/src/components/react_trace/render.rs` (early-return when `self.compact`)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

```rust
#[test]
fn compact_render_produces_single_line_per_entry() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_think("thinking about the problem", "12:00".into());
    t.append_message("hello", "bot", "12:01".into());
    t.append_user_message("hi", "12:02".into());

    let lines = t.build_compact_lines_for_tests(40);
    // 3 entries + up to 2 kind-transition separators
    assert!(lines.len() >= 3 && lines.len() <= 5, "expected 3–5 lines, got {}", lines.len());
    // Each entry line should fit in <= 40 columns by construction.
    for l in &lines {
        let cols: usize = l.spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
        assert!(cols <= 40, "compact line exceeds width: {} cols", cols);
    }
}

#[test]
fn compact_render_truncates_long_text_with_ellipsis() {
    use crate::components::react_trace::ReactTrace;
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("x".repeat(200).as_str(), "bot", "12:01".into());
    let lines = t.build_compact_lines_for_tests(20);
    let rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(rendered.contains('…'), "long text should be truncated with '…'");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui compact_render_`
Expected: FAIL — `build_compact_lines_for_tests` undefined.

- [ ] **Step 3: Implement compact render**

Create `crates/spur-tui/src/components/react_trace/compact_render.rs`:

```rust
//! Compact single-line-per-entry render path used by the DetailPane
//! Stream tab. Mirrors the visual density of the pre-unification
//! `DetailPane::render_stream` while sharing entry state with the full
//! brain-view render.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::types::{ActStatus, TraceKind};
use super::ReactTrace;

impl ReactTrace {
    /// Build the compact display lines (one row per entry, plus optional
    /// kind-transition separators). Returned lines have `'static`
    /// content.
    pub(super) fn build_compact_lines(&self, width: u16) -> Vec<Line<'static>> {
        let w = width as usize;
        if self.entries.is_empty() {
            return vec![Line::from(Span::styled(
                "(waiting for worker output…)",
                Style::default().fg(Color::DarkGray),
            ))];
        }

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut prev_kind_tag: Option<&'static str> = None;

        for entry in &self.entries {
            let kind_tag = compact_kind_tag(&entry.kind);
            if let Some(pk) = prev_kind_tag {
                if pk != kind_tag {
                    let sep: String = " ─"
                        .chars()
                        .chain(std::iter::repeat_n('─', w.saturating_sub(3)))
                        .collect();
                    lines.push(Line::from(Span::styled(
                        sep,
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            prev_kind_tag = Some(kind_tag);

            let (prefix, style) = compact_prefix_style(&entry.kind);
            let ts = &entry.timestamp;
            let ts_display = format!(" {}", ts);

            let text_single_line: String = entry
                .text
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();

            let prefix_cols = UnicodeWidthStr::width(prefix);
            let ts_cols = UnicodeWidthStr::width(ts_display.as_str());
            let text_budget = w.saturating_sub(prefix_cols + ts_cols + 1);
            let display_text = truncate_to_width(&text_single_line, text_budget);
            let display_cols = UnicodeWidthStr::width(display_text.as_str());
            let pad = w.saturating_sub(prefix_cols + display_cols + ts_cols);
            let padding: String = " ".repeat(pad);

            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(display_text, style),
                Span::raw(padding),
                Span::styled(ts_display, Style::default().fg(Color::DarkGray)),
            ]));
        }

        lines
    }

    #[cfg(test)]
    pub fn build_compact_lines_for_tests(&self, width: u16) -> Vec<Line<'static>> {
        self.build_compact_lines(width)
    }
}

fn compact_kind_tag(k: &TraceKind) -> &'static str {
    match k {
        TraceKind::Think => "think",
        TraceKind::AgentMessage { .. } => "message",
        TraceKind::Act { .. } => "act",
        TraceKind::Observe { .. } => "observe",
        TraceKind::Delegate { .. } => "delegate",
        TraceKind::UserMessage => "user",
        TraceKind::Permission { .. } => "permission",
    }
}

fn compact_prefix_style(k: &TraceKind) -> (&'static str, Style) {
    match k {
        TraceKind::Think => ("  · ", Style::default().fg(Color::DarkGray)),
        TraceKind::AgentMessage { .. } => ("  ▸ ", Style::default().fg(Color::White)),
        TraceKind::Act { status, .. } => {
            let color = match status {
                ActStatus::Pending | ActStatus::InProgress { .. } => Color::Yellow,
                ActStatus::Completed(_) => Color::Green,
                ActStatus::Failed(_) => Color::Red,
            };
            ("  ▶ ", Style::default().fg(color).add_modifier(Modifier::BOLD))
        }
        TraceKind::Observe { .. } => ("  ◂ ", Style::default().fg(Color::DarkGray)),
        TraceKind::Delegate { .. } => ("  ⇲ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        TraceKind::UserMessage => ("  > ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        TraceKind::Permission { .. } => ("  ? ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    }
}

fn truncate_to_width(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let full_width = UnicodeWidthStr::width(s);
    if full_width <= max_cols {
        return s.to_string();
    }
    let target = max_cols.saturating_sub(1);
    let mut cols = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + cw > target {
            break;
        }
        cols += cw;
        end = i + ch.len_utf8();
    }
    format!("{}…", &s[..end])
}
```

In `crates/spur-tui/src/components/react_trace/mod.rs`, add near the other `mod` declarations at the top (after `mod builder;` / `mod render;` / `mod types;`):

```rust
mod compact_render;
```

In `crates/spur-tui/src/components/react_trace/render.rs`, near the top of the main `render` method (locate via `Grep` for `pub fn render`), add at the very start of the method body:

```rust
        if self.compact {
            let width = area.width;
            self.last_render_width = Some(width);
            let lines = self.build_compact_lines(width);
            self.last_total_lines = lines.len();
            self.last_visible_height = area.height as usize;
            let scroll = self.resolve_anchor_offset(self.last_total_lines, self.last_visible_height);
            let p = ratatui::widgets::Paragraph::new(lines)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll as u16, 0));
            frame.render_widget(p, area);
            return;
        }
```

> If `resolve_anchor_offset` is named differently in your repo, substitute the existing anchor-to-offset helper. If none exists as a single-call helper, inline: `let scroll = match self.anchor { ScrollAnchor::Following => self.last_total_lines.saturating_sub(self.last_visible_height), ScrollAnchor::Row { entry_idx, row_within_entry } => ... };` — follow the pattern already present in `render.rs` around `ScrollAnchor` resolution.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui compact_render_`
Expected: PASS (2 tests).

Also run: `cargo test -p spur-tui` and `cargo clippy -p spur-tui -- -D warnings`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/compact_render.rs crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/render.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): ReactTrace compact render branch"
```

---

### Task 0.4: Audit `stream_buffer` readers

**Files:**
- Read-only: run `Grep` / `rg` across the workspace.
- Create: `docs/superpowers/notes/2026-04-19-stream-buffer-audit.md`.

- [ ] **Step 1: Grep for all readers**

Run: `rg -n 'stream_buffer' crates/ --type rust`

Record every hit. Expected locations (verify against your tree):
- `crates/spur-core/src/lineage/types.rs` — declaration.
- `crates/spur-core/src/lineage/projection.rs` — writer (lines ~268, ~309–312).
- `crates/spur-tui/src/components/detail_pane.rs` — reader (`render_stream`).
- Possibly `crates/spur-tui/src/components/inline_executor_card.rs` — verify.

- [ ] **Step 2: Classify each hit**

For each hit, note one of: `declaration`, `writer`, `reader-render`, `reader-summary`, `test`.

- [ ] **Step 3: Write the audit note**

Create `docs/superpowers/notes/2026-04-19-stream-buffer-audit.md` with a short table:

```markdown
# stream_buffer Audit — 2026-04-19

| File | Lines | Role |
|---|---|---|
| crates/spur-core/src/lineage/types.rs | 113 | declaration |
| crates/spur-core/src/lineage/projection.rs | 268, 309–312 | writer |
| crates/spur-tui/src/components/detail_pane.rs | 172–265 | reader-render (to be retired) |
| <other hits> | | |

## Conclusion

- Phase 3 write-removal is safe iff no consumer classified as
  `reader-summary` remains. If a summary consumer is found, retarget it
  to read counters (`tool_call_count`, `latest_tool_call`) before Phase 3.
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/notes/2026-04-19-stream-buffer-audit.md
git commit -m "docs: audit stream_buffer readers ahead of Phase 3 narrowing"
```

---

## Phase 1 — Shared dispatch + per-executor traces (dark launch)

### Task 1.1: Extract `dispatch_session_update` into a shared module

**Files:**
- Create: `crates/spur-tui/src/components/react_trace/dispatch.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs` (`pub mod dispatch;` + re-export)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

```rust
#[test]
fn dispatch_agent_message_chunk_appends_message_entry() {
    use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
    use crate::components::react_trace::{ReactTrace, TraceKind};
    use spur_acp::{AgentKind, ContentChunk, SessionUpdate};

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(
        spur_acp::ContentBlock::Text(spur_acp::TextContent { text: "hello".into(), ..Default::default() }),
    ));
    let mut ctx = DispatchCtx {
        agent_name: "claude",
        agent_kind: AgentKind::ClaudeCodeAcp,
        now_stamp: || "12:00".to_string(),
        tool_depth: &mut std::collections::HashMap::new(),
    };
    dispatch_session_update(&mut trace, &update, &mut ctx);

    assert_eq!(trace.entry_count(), 1);
    let entries = trace.entries();
    assert!(matches!(entries[0].kind, TraceKind::AgentMessage { .. }));
}

#[test]
fn dispatch_tool_call_then_update_merges_status() {
    use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
    use crate::components::react_trace::{ActStatus, ReactTrace, TraceKind};
    use spur_acp::{AgentKind, SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate};

    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
    let mut tool_depth = std::collections::HashMap::new();
    let now = || "12:00".to_string();

    let tc = ToolCall {
        tool_call_id: ToolCallId("abc".into()),
        title: "read_file".into(),
        status: ToolCallStatus::Pending,
        raw_input: None,
        raw_output: None,
        content: vec![],
        ..Default::default()
    };
    {
        let mut ctx = DispatchCtx { agent_name: "x", agent_kind: AgentKind::Generic, now_stamp: &now, tool_depth: &mut tool_depth };
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCall(tc), &mut ctx);
    }
    assert_eq!(trace.entry_count(), 1);

    let tcu = ToolCallUpdate {
        tool_call_id: ToolCallId("abc".into()),
        fields: spur_acp::ToolCallUpdateFields {
            status: Some(ToolCallStatus::Completed),
            raw_output: None,
            title: None,
            kind: None,
            ..Default::default()
        },
    };
    {
        let mut ctx = DispatchCtx { agent_name: "x", agent_kind: AgentKind::Generic, now_stamp: &now, tool_depth: &mut tool_depth };
        dispatch_session_update(&mut trace, &SessionUpdate::ToolCallUpdate(tcu), &mut ctx);
    }
    let entries = trace.entries();
    assert_eq!(entries.len(), 1);
    match &entries[0].kind {
        TraceKind::Act { status, .. } => assert!(matches!(status, ActStatus::Completed(_))),
        other => panic!("expected Act, got {:?}", other),
    }
}
```

> If the exact field names of `ToolCall` / `ToolCallUpdate` / `ContentChunk` differ in your `spur-acp` version, adjust the test constructor accordingly — open one existing test in `streaming_tests.rs` for a canonical shape.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui dispatch_`
Expected: FAIL — `dispatch::{dispatch_session_update, DispatchCtx}` unresolved.

- [ ] **Step 3: Create the dispatch module**

Create `crates/spur-tui/src/components/react_trace/dispatch.rs`:

```rust
//! Shared dispatch from `SessionUpdate` to `ReactTrace` mutations.
//!
//! Both the brain session view (`SessionDetailView::handle_spur_event`)
//! and the per-executor worker-stream router (`App::route_worker_notification`)
//! call this module, guaranteeing the Stream tab and brain view derive
//! from the same protocol interpretation.

use std::collections::HashMap;

use spur_acp::{adapter, AgentKind, SessionUpdate, ToolCallId, ToolCallStatus};

use super::{map_initial_status, merge_status, ActStatus, ReactTrace, TraceEntry, TraceKind};
use crate::components::trace_format::format_tool_args;

/// Caller-provided state needed to construct a `TraceEntry` from a
/// `SessionUpdate`. Fields are passed by reference so the dispatcher
/// remains agnostic to where they live (`SessionDetailView` holds
/// them on `self`; `App::route_worker_notification` constructs them
/// per-call).
pub struct DispatchCtx<'a, F: Fn() -> String> {
    pub agent_name: &'a str,
    pub agent_kind: AgentKind,
    pub now_stamp: F,
    pub tool_depth: &'a mut HashMap<String, u8>,
}

/// Mutate `trace` in response to a single `SessionUpdate`.
///
/// Handles: AgentThoughtChunk, AgentMessageChunk, UserMessageChunk,
/// ToolCall, ToolCallUpdate, Plan. Everything else is a no-op (the
/// caller may still handle session-scoped state before calling).
pub fn dispatch_session_update<F: Fn() -> String>(
    trace: &mut ReactTrace,
    update: &SessionUpdate,
    ctx: &mut DispatchCtx<'_, F>,
) {
    match update {
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    trace.append_think(&text, (ctx.now_stamp)());
                }
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    trace.append_message(&text, ctx.agent_name, (ctx.now_stamp)());
                }
            }
        }
        SessionUpdate::UserMessageChunk(chunk) => {
            if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
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
                let new_status = if let TraceKind::Act { status, .. } = &act_entry.kind {
                    merge_status(status, tcu.fields.status, tcu.fields.raw_output.as_ref(), ctx.agent_kind)
                } else {
                    return;
                };
                if let TraceKind::Act { status, .. } = &mut act_entry.kind {
                    *status = new_status;
                }
                trace.mark_dirty_from_for_update(idx);
            } else if tcu.fields.title.is_some() || tcu.fields.kind.is_some() {
                let tool = tcu.fields.title.clone().unwrap_or_else(|| "unknown".into());
                let status = map_initial_status(
                    tcu.fields.status.unwrap_or(ToolCallStatus::Pending),
                    tcu.fields.raw_output.as_ref(),
                    ctx.agent_kind,
                );
                trace.push(TraceEntry {
                    kind: TraceKind::Act {
                        tool,
                        family: adapter::ToolFamily::Unknown,
                        input: adapter::ToolInputDisplay::Empty,
                        tool_call_id: Some(tcu.tool_call_id.clone()),
                        status,
                    },
                    text: String::new(),
                    timestamp: (ctx.now_stamp)(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
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

fn extract_text(chunk: &spur_acp::ContentChunk) -> Option<String> {
    match &chunk.content {
        spur_acp::ContentBlock::Text(tc) => Some(tc.text.clone()),
        _ => None,
    }
}

fn extract_tool_call_text(blocks: &[spur_acp::ContentBlock]) -> Option<String> {
    for b in blocks {
        if let spur_acp::ContentBlock::Text(tc) = b {
            if !tc.text.is_empty() {
                return Some(tc.text.clone());
            }
        }
    }
    None
}
```

> If `find_act_by_id_mut` is not public to this module, change its visibility to `pub(super)` in `react_trace/mod.rs`. Same for `map_initial_status`, `merge_status`, `mark_dirty_from_for_update` — verify each is reachable from `dispatch.rs`.

In `crates/spur-tui/src/components/react_trace/mod.rs` add near the other module declarations:

```rust
pub mod dispatch;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui dispatch_`
Expected: PASS (2 tests). Also: `cargo test -p spur-tui` should stay green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): shared dispatch_session_update in react_trace"
```

---

### Task 1.2: Refactor `SessionDetailView` to call the shared dispatcher

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:1096-1250` (replace inline match)

- [ ] **Step 1: Pre-change check**

Run the full brain-view test suite to baseline green:

```bash
cargo test -p spur-tui --test '*' session_detail
cargo test -p spur-tui streaming_tests
```

Record pass/fail counts.

- [ ] **Step 2: Replace the inline match**

In `crates/spur-tui/src/views/session_detail.rs`, inside `handle_spur_event`, locate the branch:

```rust
SpurEventBody::AgentNotification { session, notification } => {
    if session.0 != self.session_id.0 { return; }
    crate::app::apply_session_update(self, &notification.update);
    match &notification.update {
        // ... ~160 lines of arms ...
    }
}
```

Replace the inner `match &notification.update { ... }` block with:

```rust
                // Flag streaming state for non-UserMessage variants — this
                // preserves the pre-refactor `stream_in_flight` behavior.
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(_)
                    | spur_acp::SessionUpdate::AgentMessageChunk(_) => {
                        self.stream_in_flight = true;
                    }
                    _ => {}
                }

                let agent_name = self.agent_name.clone();
                let agent_kind = self.agent_kind();
                let mut ctx = crate::components::react_trace::dispatch::DispatchCtx {
                    agent_name: agent_name.as_str(),
                    agent_kind,
                    now_stamp: || Self::now_stamp(),
                    tool_depth: &mut self.tool_depth,
                };
                crate::components::react_trace::dispatch::dispatch_session_update(
                    &mut self.react_trace,
                    &notification.update,
                    &mut ctx,
                );
```

> Keep the `crate::app::apply_session_update(self, &notification.update);` call BEFORE the dispatch — it handles session-scoped state (mode, usage, commands) that the dispatcher deliberately ignores.

- [ ] **Step 3: Run the baseline suite**

Run: `cargo test -p spur-tui` and `cargo clippy -p spur-tui -- -D warnings`
Expected: same pass counts as Step 1; zero new clippy warnings.

- [ ] **Step 4: Manual smoke test (if local env available)**

If you can run the TUI locally, spawn a brain session and verify:
- Thoughts render (`· THINK`).
- Messages render.
- Tool calls show spinner → ✓/✗ on completion.
- Plan updates render as a Think block with `[x]/[~]/[ ]` markers.

If you cannot run the TUI locally, document in the commit body that manual smoke was deferred.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "refactor(spur-tui): session_detail uses shared dispatch_session_update"
```

---

### Task 1.3: Create `WorkerStreams` wrapper

**Files:**
- Create: `crates/spur-tui/src/worker_streams.rs`
- Modify: `crates/spur-tui/src/lib.rs` (or `main.rs`) — `mod worker_streams;` declaration

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/worker_streams.rs` (a test module; write the struct tests first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{AgentKind, ContentBlock, ContentChunk, SessionUpdate, TextContent};

    fn msg(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent { text: text.into(), ..Default::default() },
        )))
    }

    #[test]
    fn route_creates_trace_on_first_notification() {
        let mut ws = WorkerStreams::new();
        ws.route("exec-1", "claude", &msg("hi"));
        assert!(ws.get("exec-1").is_some());
        assert_eq!(ws.get("exec-1").unwrap().entry_count(), 1);
    }

    #[test]
    fn route_multiple_executors_are_isolated() {
        let mut ws = WorkerStreams::new();
        ws.route("a", "claude", &msg("hi-a"));
        ws.route("b", "codex", &msg("hi-b1"));
        ws.route("b", "codex", &msg("hi-b2"));
        assert_eq!(ws.get("a").unwrap().entry_count(), 1);
        assert_eq!(ws.get("b").unwrap().entry_count(), 2);
    }

    #[test]
    fn seed_from_stream_buffer_hydrates_pre_existing_entries() {
        use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};
        use std::time::SystemTime;

        let mut ws = WorkerStreams::new();
        let entries = vec![
            WorkerStreamEntry { kind: WorkerStreamKind::Thought, text: "plan".into(), occurred_at: SystemTime::now() },
            WorkerStreamEntry { kind: WorkerStreamKind::Message, text: "hi".into(), occurred_at: SystemTime::now() },
        ];
        ws.seed_from_stream_buffer("exec-1", "claude", entries.iter());
        let t = ws.get("exec-1").expect("seeded trace");
        assert_eq!(t.entry_count(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui worker_streams`
Expected: FAIL — `WorkerStreams` unresolved.

- [ ] **Step 3: Implement `WorkerStreams`**

Create `crates/spur-tui/src/worker_streams.rs`:

```rust
//! App-level owner of per-executor `ReactTrace` instances. Receives
//! every `SpurEventBody::WorkerNotification` via `App::handle_spur_event`
//! and routes the `SessionUpdate` through the shared dispatcher.
//!
//! Key invariant: this is the ONLY place per-executor streams are
//! materialized. `ExecutorNode.stream_buffer` is retained for card
//! summary compatibility but is not a rendering input for the Stream tab.

use std::collections::HashMap;

use spur_acp::{AgentKind, SessionUpdate};
use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};

use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
use crate::components::react_trace::ReactTrace;

pub struct WorkerStreams {
    traces: HashMap<String, ReactTrace>,
    depths: HashMap<String, HashMap<String, u8>>,
}

impl WorkerStreams {
    pub fn new() -> Self {
        Self { traces: HashMap::new(), depths: HashMap::new() }
    }

    /// Route a live `SessionUpdate` for `executor_id` into that
    /// executor's `ReactTrace`, creating the trace if needed.
    pub fn route(&mut self, executor_id: &str, agent_name: &str, update: &SessionUpdate) {
        let kind = AgentKind::from_name(agent_name);
        let trace = self
            .traces
            .entry(executor_id.to_string())
            .or_insert_with(|| ReactTrace::with_kind_compact(kind));
        let depths = self.depths.entry(executor_id.to_string()).or_default();
        let mut ctx = DispatchCtx {
            agent_name,
            agent_kind: kind,
            now_stamp: now_stamp_hhmm,
            tool_depth: depths,
        };
        dispatch_session_update(trace, update, &mut ctx);
    }

    /// Seed a trace from persisted `stream_buffer` entries. Used on
    /// startup for executors that pre-date the current process.
    /// Produces coarse entries only — full fidelity resumes once live
    /// `WorkerNotification` events flow.
    pub fn seed_from_stream_buffer<'a, I>(&mut self, executor_id: &str, agent_name: &str, entries: I)
    where
        I: IntoIterator<Item = &'a WorkerStreamEntry>,
    {
        use crate::components::react_trace::{TraceEntry, TraceKind};
        let kind = AgentKind::from_name(agent_name);
        let trace = self
            .traces
            .entry(executor_id.to_string())
            .or_insert_with(|| ReactTrace::with_kind_compact(kind));
        for e in entries {
            let (kind, text) = match e.kind {
                WorkerStreamKind::Thought => (TraceKind::Think, e.text.clone()),
                WorkerStreamKind::Message => (
                    TraceKind::AgentMessage { agent: agent_name.to_string() },
                    e.text.clone(),
                ),
                WorkerStreamKind::ToolCall => {
                    use crate::components::react_trace::ActStatus;
                    (
                        TraceKind::Act {
                            tool: e.text.clone(),
                            family: spur_acp::adapter::ToolFamily::Unknown,
                            input: spur_acp::adapter::ToolInputDisplay::Empty,
                            tool_call_id: None,
                            status: ActStatus::Completed(None),
                        },
                        String::new(),
                    )
                }
            };
            trace.push(TraceEntry {
                kind,
                text,
                timestamp: format_system_time(&e.occurred_at),
                #[cfg(feature = "markdown")]
                markdown: None,
            });
        }
    }

    pub fn get(&self, executor_id: &str) -> Option<&ReactTrace> {
        self.traces.get(executor_id)
    }

    pub fn get_mut(&mut self, executor_id: &str) -> Option<&mut ReactTrace> {
        self.traces.get_mut(executor_id)
    }

    /// Drop a trace when its executor is garbage-collected or retried.
    pub fn remove(&mut self, executor_id: &str) {
        self.traces.remove(executor_id);
        self.depths.remove(executor_id);
    }
}

impl Default for WorkerStreams {
    fn default() -> Self { Self::new() }
}

fn now_stamp_hhmm() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{:02}:{:02}", h, m)
}

fn format_system_time(t: &std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{:02}:{:02}", h, m)
}
```

In `crates/spur-tui/src/lib.rs` (or whichever file declares `pub mod app;`), add:

```rust
pub mod worker_streams;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui worker_streams`
Expected: PASS (3 tests). Also: `cargo clippy -p spur-tui -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/worker_streams.rs crates/spur-tui/src/lib.rs
git commit -m "feat(spur-tui): WorkerStreams — per-executor ReactTrace router"
```

---

### Task 1.4: Route `WorkerNotification` in `App::handle_spur_event`

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (add `worker_streams: WorkerStreams` field + routing after `self.lineage.apply`)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/worker_stream_routing.rs`:

```rust
//! Integration test: App routes WorkerNotification events into
//! per-executor ReactTraces.

// NOTE: this test relies on test helpers constructing App; follow the
// pattern used in other crates/spur-tui/tests/*.rs files. If your
// `App::new_for_tests` signature differs, adapt accordingly.

#[test]
fn worker_notification_populates_per_executor_trace() {
    use spur_acp::{ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent};
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};

    let mut app = spur_tui::app::App::new_for_tests();
    let exec_id = "exec-42".to_string();
    let notif = Box::new(SessionNotification {
        session_id: SessionId("abc".into()),
        update: SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent { text: "hello from worker".into(), ..Default::default() },
        ))),
        ..Default::default()
    });
    let event = SpurEvent::for_test(SpurEventBody::WorkerNotification {
        brain_session_id: SessionId("brain-1".into()),
        executor_id: exec_id.clone(),
        notification: notif,
    });
    app.handle_spur_event(event);

    let trace = app.worker_streams().get(&exec_id).expect("trace for executor");
    assert_eq!(trace.entry_count(), 1);
}
```

> If `App::new_for_tests` and `SpurEvent::for_test` do not exist, add minimal shims in the same style as other tests in `crates/spur-tui/tests/`. If there is NO test helper for constructing `App`, move this test to a `#[cfg(test)]` inline module in `app.rs` that uses whatever internal constructors are available, but keep the assertion the same.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test worker_stream_routing`
Expected: FAIL — `worker_streams()` method and/or field missing.

- [ ] **Step 3: Add field and routing**

In `crates/spur-tui/src/app.rs`, at the top of the file:

```rust
use crate::worker_streams::WorkerStreams;
```

In the `App` struct (locate via `Grep` for `pub struct App`), add a new field alongside `lineage: LineageProjection`:

```rust
    /// Per-executor `ReactTrace` instances rendered by the Stream tab.
    /// Populated on every `SpurEventBody::WorkerNotification`.
    pub(crate) worker_streams: WorkerStreams,
```

In every `App` constructor (`new`, `new_for_tests`, anything that produces an `App`), initialize:

```rust
            worker_streams: WorkerStreams::new(),
```

In `App::handle_spur_event` (crates/spur-tui/src/app.rs:503), immediately after `self.lineage.apply(&event);` (line 506), add:

```rust
        // Route worker stream updates into per-executor ReactTraces.
        if let spur_acp::domain::events::SpurEventBody::WorkerNotification {
            executor_id, notification, ..
        } = &event.body
        {
            let agent_name = self
                .lineage
                .get(&spur_core::lineage::types::ExecutorId(executor_id.clone()))
                .map(|n| n.agent.as_str())
                .unwrap_or("generic");
            self.worker_streams
                .route(executor_id, agent_name, &notification.update);
        }
```

> Adapt `self.lineage.get(...)` to the actual LineageProjection accessor name (look in `crates/spur-core/src/lineage/mod.rs` for the getter; candidates: `get`, `node`, `executor_node`). Default `agent_name` to `"generic"` if unknown.

Add a public accessor (integration tests under `tests/` cannot see `#[cfg(test)]` items, so this is unconditionally `pub`):

```rust
    /// Accessor for the per-executor trace map. Used by DashboardView
    /// during render and by integration tests.
    pub fn worker_streams(&self) -> &WorkerStreams {
        &self.worker_streams
    }

    pub fn worker_streams_mut(&mut self) -> &mut WorkerStreams {
        &mut self.worker_streams
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test worker_stream_routing`
Expected: PASS. Also run: `cargo test -p spur-tui` (no regressions).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/tests/worker_stream_routing.rs
git commit -m "feat(spur-tui): App routes WorkerNotification into WorkerStreams"
```

---

### Task 1.5: Seed traces from `stream_buffer` on restart

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (find the path that rehydrates lineage from disk on startup)

- [ ] **Step 1: Locate the restart path**

Run: `rg -n 'load_history|rehydrate|restore|LineageProjection::from|SessionHistory' crates/spur-tui/src/app.rs`

Identify the function that walks the persisted `ExecutorNode`s on startup. Record the function name and line range.

- [ ] **Step 2: Write the failing test**

Create `crates/spur-tui/tests/worker_stream_seed.rs`:

```rust
#[test]
fn seeded_trace_preserves_stream_buffer_entry_count() {
    use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};
    use std::time::SystemTime;

    let mut ws = spur_tui::worker_streams::WorkerStreams::new();
    let entries = vec![
        WorkerStreamEntry { kind: WorkerStreamKind::Message, text: "seeded".into(), occurred_at: SystemTime::now() },
        WorkerStreamEntry { kind: WorkerStreamKind::Thought, text: "seeded2".into(), occurred_at: SystemTime::now() },
    ];
    ws.seed_from_stream_buffer("exec-seed", "claude", entries.iter());
    let t = ws.get("exec-seed").unwrap();
    assert_eq!(t.entry_count(), 2);
}
```

- [ ] **Step 3: Wire the seeding call**

In the function identified in Step 1 (where `ExecutorNode`s are loaded from disk and inserted into `self.lineage`), after each executor is restored, add:

```rust
        self.worker_streams
            .seed_from_stream_buffer(&node.id.0, &node.agent, node.stream_buffer.iter());
```

> If the restart path constructs nodes via a helper function, make sure the seeding runs AFTER that helper returns and the node is observable by id.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test worker_stream_seed`
Expected: PASS. Also `cargo test -p spur-tui`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/tests/worker_stream_seed.rs
git commit -m "feat(spur-tui): seed WorkerStreams from persisted stream_buffer on restart"
```

---

## Phase 2 — Cutover the Stream tab

### Task 2.1: Pipe `&ReactTrace` into `DetailPane::render`

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs:89-169` (extend `render` signature)
- Modify: `crates/spur-tui/src/views/dashboard.rs` (call site at line 491)

- [ ] **Step 1: Extend `DetailPane::render` signature**

In `crates/spur-tui/src/components/detail_pane.rs`, change:

```rust
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        node: &ExecutorNode,
        issue_badge: Option<&str>,
    ) {
```

to:

```rust
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        node: &ExecutorNode,
        issue_badge: Option<&str>,
        stream_trace: Option<&mut crate::components::react_trace::ReactTrace>,
    ) {
```

- [ ] **Step 2: Replace `render_stream` call with delegation + fallback**

Inside `render`, locate:

```rust
        let body_lines = match self.current_tab {
            DetailTab::Stream => self.render_stream(node, body_area.width),
            ...
        };
```

Replace the `DetailTab::Stream` arm with a branch:

```rust
            DetailTab::Stream => {
                if let Some(trace) = stream_trace {
                    // Delegate to compact ReactTrace render. The trace
                    // owns its own Paragraph + scroll machinery; we hand
                    // it our body area and return early.
                    trace.render(frame, body_area);
                    // scroll_offset / is_following on DetailPane are
                    // unused in this branch; follow mode is managed by
                    // the trace's ScrollAnchor.
                    return;
                }
                self.render_stream(node, body_area.width)
            }
```

> `ReactTrace::render` takes `(&mut self, &mut Frame, Rect)`. Verify the exact signature in `crates/spur-tui/src/components/react_trace/render.rs`. If it also takes a spinner frame or lineage, pass placeholders consistent with compact-mode (e.g., `""` spinner, `None` lineage).

- [ ] **Step 3: Update call site in `dashboard.rs`**

In `crates/spur-tui/src/views/dashboard.rs`, locate line 491:

```rust
                            .render(frame, chunks[log_chunk], node, badge.as_deref());
```

Change `DashboardView::render_main_content` (or whichever method holds this call) to accept a `&mut WorkerStreams` parameter, and change the call to:

```rust
                            .render(
                                frame,
                                chunks[log_chunk],
                                node,
                                badge.as_deref(),
                                worker_streams.get_mut(&node.id.0),
                            );
```

Then thread `&mut self.worker_streams` through from `App` into the dashboard render call site.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p spur-tui && cargo clippy -p spur-tui -- -D warnings`
Expected: PASS. If a test hits a render path that now takes an extra param, update the call site and its construction.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/detail_pane.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): DetailPane delegates Stream tab to per-executor ReactTrace"
```

---

### Task 2.2: Parity snapshot test

**Files:**
- Create: `crates/spur-tui/tests/stream_tab_parity.rs`

- [ ] **Step 1: Write the parity test**

Create `crates/spur-tui/tests/stream_tab_parity.rs`:

```rust
//! Parity check: the new Stream tab render path produces at least the
//! entries the old path did, and additionally surfaces fidelity (e.g.
//! tool-call lifecycle) the old path dropped.

#[test]
fn new_path_covers_old_kinds_and_adds_lifecycle() {
    use spur_acp::{AgentKind, ContentBlock, ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate};
    use spur_tui::components::react_trace::{ReactTrace, TraceKind};
    use spur_tui::worker_streams::WorkerStreams;

    let mut ws = WorkerStreams::new();
    let exec = "exec-parity";
    let msg = |t: &str| SessionUpdate::AgentMessageChunk(ContentChunk::new(
        ContentBlock::Text(TextContent { text: t.into(), ..Default::default() })
    ));
    ws.route(exec, "claude", &msg("one"));
    ws.route(exec, "claude", &SessionUpdate::AgentThoughtChunk(ContentChunk::new(
        ContentBlock::Text(TextContent { text: "thinking".into(), ..Default::default() })
    )));
    let tc = ToolCall {
        tool_call_id: ToolCallId("t1".into()),
        title: "read".into(),
        status: ToolCallStatus::Pending,
        raw_input: None, raw_output: None, content: vec![],
        ..Default::default()
    };
    ws.route(exec, "claude", &SessionUpdate::ToolCall(tc));
    let tcu = ToolCallUpdate {
        tool_call_id: ToolCallId("t1".into()),
        fields: spur_acp::ToolCallUpdateFields {
            status: Some(ToolCallStatus::Completed),
            ..Default::default()
        },
    };
    ws.route(exec, "claude", &SessionUpdate::ToolCallUpdate(tcu));

    let trace = ws.get(exec).unwrap();
    assert_eq!(trace.entry_count(), 3, "message + think + act");

    // Verify the Act entry advanced from Pending to Completed via
    // ToolCallUpdate — the fidelity the old render path dropped.
    let act = trace.entries().iter().find(|e| matches!(e.kind, TraceKind::Act { .. })).unwrap();
    if let TraceKind::Act { status, .. } = &act.kind {
        assert!(matches!(status, spur_tui::components::react_trace::ActStatus::Completed(_)),
            "Act should be Completed, got {:?}", status);
    }
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p spur-tui --test stream_tab_parity`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/tests/stream_tab_parity.rs
git commit -m "test(spur-tui): parity check — Stream tab gains tool-call lifecycle"
```

---

## Phase 3 — Cleanup

### Task 3.1: Narrow `lineage/projection.rs` — stop writing `stream_buffer`

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs:274-316`
- Modify: existing projection tests that assert on `stream_buffer` contents

**Precondition:** Phase 2 is merged and soak-tested for at least one release cycle. The audit from Task 0.4 shows no remaining `reader-summary` consumers.

- [ ] **Step 1: Locate projection tests that read `stream_buffer`**

Run: `rg -n 'stream_buffer' crates/spur-core/`. Note test files.

- [ ] **Step 2: Update expectations**

Edit the tests so they assert on `tool_call_count` / `latest_tool_call` / `last_event_at` (the counters the projection still maintains), NOT on `stream_buffer.len()` or `stream_buffer[i].kind`.

- [ ] **Step 3: Narrow the projection's match arm**

In `crates/spur-core/src/lineage/projection.rs`, locate the `SpurEventBody::WorkerNotification { executor_id, notification, .. }` arm. Replace the body with:

```rust
                if let Some(node) = self.nodes.get_mut(&ExecutorId(executor_id.clone())) {
                    node.last_event_at = Some(event.occurred_at);
                    if let spur_acp::SessionUpdate::ToolCall(tc) = &notification.update {
                        node.tool_call_count += 1;
                        node.latest_tool_call = Some(tc.title.clone());
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
```

This removes the `WorkerStreamEntry` synthesis and the `stream_buffer.push_back` calls. The field stays declared in `types.rs` and remains serde-compatible.

- [ ] **Step 4: Run all affected tests**

Run:
```
cargo test -p spur-core
cargo test -p spur-tui
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs crates/spur-core/src/lineage/
git commit -m "refactor(spur-core): projection no longer writes stream_buffer; counters retained"
```

---

### Task 3.2: Remove legacy `DetailPane::render_stream` fallback

**Files:**
- Modify: `crates/spur-tui/src/components/detail_pane.rs`

- [ ] **Step 1: Remove fallback branch**

In `DetailPane::render`'s `DetailTab::Stream` arm, remove the `None` fallback:

```rust
            DetailTab::Stream => {
                if let Some(trace) = stream_trace {
                    trace.render(frame, body_area);
                    return;
                }
                // No live trace yet — render placeholder.
                vec![ratatui::text::Line::from(ratatui::text::Span::styled(
                    "(no stream yet)",
                    ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
                ))]
            }
```

- [ ] **Step 2: Delete the `render_stream` method**

Remove `fn render_stream(&self, node: &ExecutorNode, width: u16) -> Vec<Line<'static>>` (lines ~171-265) and the `truncate_to_width` helper at the bottom. Both are now dead.

- [ ] **Step 3: Remove unused imports**

If `WorkerStreamKind`, `unicode_width::UnicodeWidthStr`, or other imports become unused, remove them. Let `cargo clippy -- -D warnings` catch them.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui && cargo clippy -p spur-tui -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/detail_pane.rs
git commit -m "refactor(spur-tui): drop legacy render_stream; trace is authoritative"
```

---

### Task 3.3: Document the new architecture

**Files:**
- Modify: existing architecture doc (find via `rg -n 'stream_buffer|DetailPane' docs/`) OR create `docs/superpowers/architecture/stream-pipeline.md`

- [ ] **Step 1: Pick the document home**

Prefer updating an existing doc that already describes the TUI event pipeline. If none exists, create `docs/superpowers/architecture/stream-pipeline.md`.

- [ ] **Step 2: Write the doc**

```markdown
# Stream Pipeline (post-unification)

`WorkerNotification` is the single source of truth for what a worker
executor is doing. Both the brain session view and the DetailPane Stream
tab consume it through the same dispatcher
(`crates/spur-tui/src/components/react_trace/dispatch.rs`).

## Flow

1. Event arrives in `App::handle_spur_event`.
2. `LineageProjection::apply` updates card summary counters on the
   matching `ExecutorNode` (`tool_call_count`, `latest_tool_call`,
   `last_event_at`).
3. `App::handle_spur_event` routes the notification to
   `WorkerStreams::route(executor_id, agent_name, &update)`, which
   materializes a per-executor `ReactTrace` on first contact and
   dispatches the update through the shared dispatcher.
4. `DashboardView::render` looks up the trace for the focused executor
   via `WorkerStreams::get_mut` and passes it into `DetailPane::render`.
5. `DetailPane` delegates the Stream tab to `ReactTrace::render` with
   `compact = true`.

## `stream_buffer` is not a rendering input.

It remains declared on `ExecutorNode` for serde backward compatibility
with `session_metadata.json` files written pre-unification. No code
path currently writes to it; no render path reads it. Removal is
deferred to a future change.
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/architecture/stream-pipeline.md
git commit -m "docs: document post-unification stream pipeline"
```

---

## Self-Review Checklist

Before handing off to executor or reviewer, verify:

- [ ] **Spec coverage.** Every Goal (§Goals 1–5) is implemented by at least one task. Every Non-goal is not violated by any task.
- [ ] **Phase ordering.** Phase 3 tasks do not reference code that Phase 1/2 has not yet introduced. Phase 0 tasks do not depend on anything.
- [ ] **Type consistency.** `WorkerStreams::route` signature matches its call sites in Tasks 1.4, 1.5, 2.1, 2.2. `dispatch_session_update` signature matches Tasks 1.1, 1.2, 1.3.
- [ ] **Placeholder scan.** No "TBD", no "add error handling", no "similar to Task N".
- [ ] **Commands are concrete.** Every test run specifies the crate (`-p spur-tui`) and, where scoped, the test name.
- [ ] **Back-out plan.** Phases 0–2 are pure additions; Phase 3 can be reverted independently if a late-surfacing `stream_buffer` consumer is discovered.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-19-stream-tab-unification.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
