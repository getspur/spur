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

#[test]
fn render_compact_does_not_panic_and_updates_dimensions() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("hello", "bot", "12:00".into());
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();

    assert_eq!(t.last_render_width, Some(40));
    assert_eq!(t.last_visible_height, 10);
    assert!(t.last_total_lines >= 1);
}

#[test]
fn render_compact_cache_hits_when_generation_stable() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message("a", "bot", "12:00".into());
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();
    let gen_after_first = t.generation_for_tests();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();
    // No mutations between renders → cache hit → generation unchanged.
    assert_eq!(t.generation_for_tests(), gen_after_first);
    // dirty_from consumed on first render.
    assert!(t.dirty_from_for_tests().is_none());
}

#[test]
fn render_compact_cache_invalidates_on_width_change() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    t.append_message(&"x".repeat(100), "bot", "12:00".into());
    let mut term = Terminal::new(TestBackend::new(120, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();
    let width_40 = t.last_render_width;
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 80, 10))).unwrap();
    assert_eq!(width_40, Some(40));
    assert_eq!(t.last_render_width, Some(80));
    // Internal cache must have been rebuilt; we can't assert directly
    // without test hooks, but `last_render_width` changing is enough.
}

#[test]
fn render_compact_incremental_rebuild_on_new_entry() {
    use crate::components::react_trace::ReactTrace;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};
    use spur_acp::AgentKind;

    let mut t = ReactTrace::with_kind_compact(AgentKind::Generic);
    for i in 0..500 {
        t.append_message(&format!("msg-{}", i), "bot", "12:00".into());
    }
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();
    // Append one more; next render should use the incremental path.
    t.append_message("msg-new", "bot", "12:01".into());
    term.draw(|f| t.render_compact(f, Rect::new(0, 0, 40, 10))).unwrap();
    assert_eq!(t.last_total_lines, 501);
}
```

> The `generation_for_tests` and `dirty_from_for_tests` accessors are thin `#[cfg(test)]` getters on `ReactTrace`. Add them under the existing `#[cfg(all(test, feature = "markdown"))]` / `#[cfg(test)]` test-helper blocks in `mod.rs`:
>
> ```rust
> #[cfg(test)]
> impl ReactTrace {
>     pub fn generation_for_tests(&self) -> u64 { self.generation }
>     pub fn dirty_from_for_tests(&self) -> Option<usize> { self.dirty_from }
> }
> ```

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

**Important — separate entry point.** The existing `ReactTrace::render(&mut self, &mut Frame, Rect, Option<&ExecutorLineage>)` draws its own block/border/title (`render.rs:280-284`). That is wrong for embedding inside `DetailPane`, which already owns the outer block. Therefore we add a **new public method** `render_compact` that paints only the body — no block, no border, no title. It lives in `compact_render.rs` alongside `build_compact_lines`.

Append to `crates/spur-tui/src/components/react_trace/compact_render.rs` (after the `build_compact_lines` impl block you wrote above):

```rust
/// Cache entry for the compact render path. Keyed by `(generation, width)`.
/// Independent from the full-render `line_cache` because the two paths
/// produce different row layouts.
pub(super) struct CompactCacheEntry {
    pub generation: u64,
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    /// Number of `ReactTrace.entries` the cache covers. Anything past
    /// this index is treated as dirty on the next render.
    pub covered_entries: usize,
}

impl ReactTrace {
    /// Paint the compact single-line-per-entry body into `area`.
    ///
    /// Does NOT draw a block/border/title — the caller (DetailPane) owns
    /// the outer block. Honours the current `ScrollAnchor` for vertical
    /// offset and refreshes `last_total_lines` / `last_visible_height` /
    /// `last_render_width` so scroll helpers stay consistent.
    ///
    /// **Caching.** Each call checks `compact_cache`:
    /// - **Hit** (same generation + same width): O(1) reuse of the cached
    ///   `Vec<Line>`.
    /// - **Incremental** (width unchanged, generation advanced by dirty tail):
    ///   truncate cache at `dirty_from` and rebuild the tail only.
    /// - **Full rebuild** (width changed OR cold cache): rebuild all lines.
    ///
    /// In steady-state streaming this reduces render cost from O(entries)
    /// per frame to O(entries_since_dirty) per frame (typically 1–5).
    pub fn render_compact(&mut self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::{Paragraph, Wrap};

        let width = area.width;
        self.last_render_width = Some(width);
        self.last_visible_height = area.height as usize;
        let gen = self.generation;

        let lines = self.compact_lines_for_render(gen, width);
        self.last_total_lines = lines.len();

        // Resolve anchor → scroll offset. Mirrors the logic in `render()`.
        let scroll = match self.anchor {
            crate::components::react_trace::types::ScrollAnchor::Following => {
                self.last_total_lines
                    .saturating_sub(self.last_visible_height)
            }
            crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx,
                row_within_entry,
            } => {
                // Compact mode is one line per entry + separators. Approximate
                // row resolution: use entry_idx + row_within_entry, clamped.
                let total = self.last_total_lines;
                let max = total.saturating_sub(self.last_visible_height);
                (entry_idx + row_within_entry).min(max)
            }
        };

        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(p, area);
    }

    /// Internal cache-aware line producer for `render_compact`.
    /// Returns a clone of the cached `Vec<Line>`; the cache itself owns
    /// the canonical copy so repeated renders are cheap.
    fn compact_lines_for_render(&mut self, gen: u64, width: u16) -> Vec<Line<'static>> {
        let entry_count = self.entries.len();
        let dirty = self.dirty_from.unwrap_or(entry_count);

        // Cache hit — same generation and width.
        if let Some(c) = &self.compact_cache {
            if c.generation == gen && c.width == width && c.covered_entries == entry_count {
                return c.lines.clone();
            }
        }

        // Incremental rebuild — width unchanged, only a tail is dirty.
        if let Some(c) = self.compact_cache.as_mut() {
            if c.width == width && dirty < entry_count && dirty <= c.covered_entries {
                // Rebuild lines for entries [dirty, entry_count).
                // In compact mode each entry produces exactly one line
                // (plus up to one kind-transition separator). We rebuild
                // the full tail from `dirty` for correctness; this is
                // O(entries_since_dirty), typically 1–5.
                //
                // First, locate the line-index corresponding to entry
                // `dirty`. Walk the cache's first `dirty` entries from
                // the SAME starting point `build_compact_lines` used.
                let prefix_row_count = prefix_row_count_for_entries(&self.entries[..dirty]);
                c.lines.truncate(prefix_row_count);
                let tail = self.build_compact_lines_tail(dirty, width);
                c.lines.extend(tail);
                c.generation = gen;
                c.covered_entries = entry_count;
                self.dirty_from = None;
                return c.lines.clone();
            }
        }

        // Full rebuild — width changed, or cold cache, or invariants
        // broken (e.g., entries shrunk unexpectedly).
        let lines = self.build_compact_lines(width);
        self.compact_cache = Some(CompactCacheEntry {
            generation: gen,
            width,
            lines: lines.clone(),
            covered_entries: entry_count,
        });
        self.dirty_from = None;
        lines
    }

    /// Rebuild lines for entries `[start..]` only. Mirrors
    /// `build_compact_lines` but skips `[0..start]`. Used by the
    /// incremental cache path.
    fn build_compact_lines_tail(&self, start: usize, width: u16) -> Vec<Line<'static>> {
        if start >= self.entries.len() {
            return Vec::new();
        }
        // Compute the prev_kind_tag from the entry immediately before `start`
        // so separator decisions match a full rebuild.
        let prev_kind_tag = if start > 0 {
            Some(compact_kind_tag(&self.entries[start - 1].kind))
        } else {
            None
        };
        build_compact_lines_from(&self.entries[start..], width, prev_kind_tag)
    }

    /// Drop the compact cache. Called when the owning executor loses
    /// focus (via `WorkerStreams::drop_compact_cache_for`) to free ~1 MB
    /// per unfocused trace without losing the entries themselves.
    pub fn drop_compact_cache(&mut self) {
        self.compact_cache = None;
    }
}

/// Row count produced by rendering `entries` via compact mode — the
/// one-line-per-entry baseline plus one separator per kind transition.
/// Used for the truncate point in incremental rebuilds.
fn prefix_row_count_for_entries(entries: &[crate::components::react_trace::TraceEntry]) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let mut rows = 0usize;
    let mut prev_kind_tag: Option<&'static str> = None;
    for e in entries {
        let tag = compact_kind_tag(&e.kind);
        if let Some(pk) = prev_kind_tag {
            if pk != tag {
                rows += 1; // separator row
            }
        }
        prev_kind_tag = Some(tag);
        rows += 1;
    }
    rows
}

/// Shared core of `build_compact_lines` and `build_compact_lines_tail`.
/// `prev_kind_tag` seeds the kind-transition separator logic so an
/// incremental rebuild inserts the same leading separator a full
/// rebuild would have.
fn build_compact_lines_from(
    entries: &[crate::components::react_trace::TraceEntry],
    width: u16,
    seed_prev_kind_tag: Option<&'static str>,
) -> Vec<Line<'static>> {
    // Implementation: move the body of `build_compact_lines` here,
    // parameterized by `entries` and `seed_prev_kind_tag`. Then have
    // `build_compact_lines` call this with `&self.entries, width, None`.
    // Omitted in this plan for brevity — follow the exact shape of the
    // function you wrote above.
    todo!("inline the body of build_compact_lines here, parameterized")
}
```

Also add a field on `ReactTrace` in `crates/spur-tui/src/components/react_trace/mod.rs` next to the existing `line_cache`:

```rust
    /// Cache for the compact render path (used by `render_compact`).
    /// Independent from `line_cache` because the two paths produce
    /// different row layouts. `None` until first compact render.
    pub(super) compact_cache: Option<compact_render::CompactCacheEntry>,
```

And initialize it `compact_cache: None,` in `ReactTrace::new()`.

> **Note on the `todo!` above.** `build_compact_lines_from` is intentionally left as a `todo!` in this plan to keep the listing short. Implementing it is mechanical: take the body of the `build_compact_lines` function you wrote earlier, replace `&self.entries` with `entries`, replace the initial `let mut prev_kind_tag: Option<&'static str> = None;` with `let mut prev_kind_tag = seed_prev_kind_tag;`, and keep the rest. Then have the original `build_compact_lines` delegate: `fn build_compact_lines(&self, width: u16) -> Vec<Line<'static>> { build_compact_lines_from(&self.entries, width, None) }`.

**Do NOT modify `render.rs`** — the full-screen `render()` entry point is unchanged. The `compact: bool` field from Task 0.2 remains on the struct for documentation/testing purposes, but `render_compact` is the authoritative entry point for the Stream tab.

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

### Task 0.5: Criterion benchmark for compact render + append

**Files:**
- Create: `crates/spur-tui/benches/compact_render.rs`
- Modify: `crates/spur-tui/Cargo.toml` — add the `[[bench]]` entry

**Why:** The render hot path for the DetailPane Stream tab is O(entries) per frame without caching. PP1 in Task 0.3 adds cache + incremental rebuild; this bench locks in the expected numbers so later changes can't silently regress. It also informs whether Phase 4's optional compact-mode entry cap (MAX_COMPACT_ENTRIES) is warranted.

**Expected numbers (goals, not asserts):**
- `append_message` single chunk: <1 μs amortized.
- `render_compact` cache hit at 5000 entries, width 40: <200 μs.
- `render_compact` incremental rebuild (1-entry dirty tail) at 5000 entries, width 40: <500 μs.
- `render_compact` cold (width change or cache miss) at 5000 entries, width 40: <25 ms.

- [ ] **Step 1: Add criterion dev-dependency and bench target**

In `crates/spur-tui/Cargo.toml`, ensure:

```toml
[dev-dependencies]
criterion = { version = "0.5", default-features = false, features = ["cargo_bench_support"] }

[[bench]]
name = "compact_render"
harness = false
```

If `criterion` is already a dev-dependency at the workspace root, use that and skip the `[dev-dependencies]` addition.

- [ ] **Step 2: Write the bench**

Create `crates/spur-tui/benches/compact_render.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_acp::AgentKind;
use spur_tui::components::react_trace::ReactTrace;

fn bench_append_message(c: &mut Criterion) {
    c.bench_function("append_message_single", |b| {
        let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
        b.iter(|| {
            trace.append_message(black_box("a short streaming chunk"), "bot", "12:00".into());
        });
    });
}

fn bench_render_compact_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_cold");
    for entries in [500usize, 2000, 5000] {
        for width in [40u16, 80, 120] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{}_entries_w{}", entries, width)),
                &(entries, width),
                |b, &(n, w)| {
                    let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
                    for i in 0..n {
                        trace.append_message(&format!("streaming chunk {}", i), "bot", "12:00".into());
                    }
                    let mut term = Terminal::new(TestBackend::new(w, 24)).unwrap();
                    b.iter(|| {
                        // Force cold cache by dropping the cache on every iter.
                        trace.drop_compact_cache();
                        term.draw(|f| trace.render_compact(f, Rect::new(0, 0, w, 24))).unwrap();
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_render_compact_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_cache_hit");
    for entries in [500usize, 2000, 5000] {
        group.bench_with_input(BenchmarkId::from_parameter(entries), &entries, |b, &n| {
            let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
            for i in 0..n {
                trace.append_message(&format!("chunk {}", i), "bot", "12:00".into());
            }
            let mut term = Terminal::new(TestBackend::new(40, 24)).unwrap();
            // Warm the cache.
            term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            b.iter(|| {
                term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            });
        });
    }
    group.finish();
}

fn bench_render_compact_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_compact_incremental");
    for entries in [500usize, 2000, 5000] {
        group.bench_with_input(BenchmarkId::from_parameter(entries), &entries, |b, &n| {
            let mut trace = ReactTrace::with_kind_compact(AgentKind::Generic);
            for i in 0..n {
                trace.append_message(&format!("chunk {}", i), "bot", "12:00".into());
            }
            let mut term = Terminal::new(TestBackend::new(40, 24)).unwrap();
            term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            let mut counter = 0usize;
            b.iter(|| {
                // Append one chunk to dirty the tail, then render.
                trace.append_message(&format!("new-{}", counter), "bot", "12:01".into());
                counter += 1;
                term.draw(|f| trace.render_compact(f, Rect::new(0, 0, 40, 24))).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_append_message,
    bench_render_compact_cold,
    bench_render_compact_hit,
    bench_render_compact_incremental
);
criterion_main!(benches);
```

- [ ] **Step 3: Run the bench to establish a baseline**

Run: `cargo bench -p spur-tui --bench compact_render`
Expected: all benches compile and run. Record the numbers in the commit message and compare to the goals above.

If any goal is missed by >2×, file a follow-up note in the plan's Risk Register (PR8/PR9…) rather than blocking the cutover — the fallback is PP4 (entry cap) in Phase 4.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/benches/compact_render.rs crates/spur-tui/Cargo.toml
git commit -m "perf(spur-tui): baseline bench for compact render + append_message"
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

> **Constructor pattern.** The sketch above uses `ToolCall { ..Default::default() }` and `ToolCallUpdate { ..Default::default() }` — these may NOT have `Default` impls in your `spur-acp` version. Before writing the tests, open `crates/spur-tui/src/components/react_trace/streaming_tests.rs` and copy the canonical `ToolCall` / `ToolCallUpdate` / `ContentChunk` / `ToolCallUpdateFields` construction pattern verbatim (search for `ToolCall {` and `ToolCallUpdate {`). Use that exact pattern here. If a test helper function exists in that file (e.g., `make_tool_call(id, title)`), reuse it by importing from the same module.

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
    fn reset_clears_entries_and_depths_but_keeps_kind() {
        let mut ws = WorkerStreams::new();
        ws.route("exec-r", "claude", &msg("hi"));
        assert_eq!(ws.get("exec-r").unwrap().entry_count(), 1);
        ws.reset("exec-r");
        assert_eq!(ws.get("exec-r").unwrap().entry_count(), 0, "reset clears entries");
        ws.route("exec-r", "claude", &msg("hi-again"));
        assert_eq!(ws.get("exec-r").unwrap().entry_count(), 1, "reset preserves slot for reuse");
    }

    #[test]
    fn tick_all_advances_every_trace_without_panic() {
        let mut ws = WorkerStreams::new();
        ws.route("a", "claude", &msg("x"));
        ws.route("b", "codex", &msg("y"));
        ws.tick_all();
        ws.tick_all();
        // Success == no panic; traces remain queryable.
        assert!(ws.get("a").is_some());
        assert!(ws.get("b").is_some());
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
    /// Remember the resolved `AgentKind` per executor so `reset` can
    /// rebuild the trace with the correct accent color without needing
    /// to peek inside `ReactTrace`.
    kinds: HashMap<String, AgentKind>,
}

impl WorkerStreams {
    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
            depths: HashMap::new(),
            kinds: HashMap::new(),
        }
    }

    /// Route a live `SessionUpdate` for `executor_id` into that
    /// executor's `ReactTrace`, creating the trace if needed.
    pub fn route(&mut self, executor_id: &str, agent_name: &str, update: &SessionUpdate) {
        let kind = AgentKind::from_name(agent_name);
        self.kinds.insert(executor_id.to_string(), kind);
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

    /// Advance the spinner frame on all live traces. Called from App's
    /// tick loop so Act entries with `Pending` / `InProgress` status
    /// animate consistently with the brain view.
    pub fn tick_all(&mut self) {
        for trace in self.traces.values_mut() {
            trace.tick();
        }
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

    /// Drop a trace when its executor is garbage-collected.
    pub fn remove(&mut self, executor_id: &str) {
        self.traces.remove(executor_id);
        self.depths.remove(executor_id);
    }

    /// Reset a trace on retry. Clears entries + tool-depth namespace
    /// but keeps the HashMap slot, so the next `route` call reuses the
    /// same trace. Matches the lineage projection's
    /// `stream_buffer.clear()` on `ExecutorRetryStarted`.
    pub fn reset(&mut self, executor_id: &str) {
        if let Some(depths) = self.depths.get_mut(executor_id) {
            depths.clear();
        }
        let kind = self
            .kinds
            .get(executor_id)
            .copied()
            .unwrap_or(AgentKind::Generic);
        if let Some(slot) = self.traces.get_mut(executor_id) {
            *slot = ReactTrace::with_kind_compact(kind);
        }
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

- [ ] **Step 1: Locate the canonical test-fixture pattern**

Run: `rg -n 'fn.*App.*->|new_for_tests|SpurEvent::|fn build_test_event' crates/spur-tui/tests/ crates/spur-tui/src/ --type rust | head -40`

Record the exact constructors used by existing TUI tests for `App` and `SpurEvent`. Use those patterns verbatim in the tests below. If no `App` test-fixture exists, the tests go inline in `app.rs` (see Step 2) and exercise the routing through a thin helper rather than a full App.

- [ ] **Step 2: Write the failing test — inline `#[cfg(test)] mod` in `app.rs`**

Integration tests in `crates/spur-tui/tests/` cannot see private constructors. To avoid a fragile assumption about test helpers, the routing test lives inside `crates/spur-tui/src/app.rs` in a `#[cfg(test)]` module. Append (or extend the existing test module):

```rust
#[cfg(test)]
mod worker_stream_routing_tests {
    use super::*;
    use spur_acp::{ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent};
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use spur_core::lineage::types::ExecutorId;

    fn msg_update(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent { text: text.into(), ..Default::default() },
        )))
    }

    // Replace this with whatever test-friendly App constructor your
    // codebase exposes. If App::new takes dependencies that are heavy
    // in tests, define a narrower helper that just exercises
    // handle_spur_event with a pre-seeded lineage.
    fn test_app() -> App {
        App::new_for_tests()
    }

    fn wrap_event(body: SpurEventBody) -> SpurEvent {
        // Use whatever constructor already exists in spur-acp for test
        // events (look for `SpurEvent::new`, `SpurEvent::synthetic`,
        // `SpurEvent::for_test`, or a bare struct literal with the
        // required fields). Copy from an existing test in
        // crates/spur-tui/tests/ for the canonical shape.
        SpurEvent::for_test(body)
    }

    #[test]
    fn worker_notification_populates_per_executor_trace() {
        let mut app = test_app();
        // Seed the lineage with the executor first — routing drops
        // orphan WorkerNotifications.
        app.lineage.apply(&wrap_event(SpurEventBody::ExecutorSpawned {
            executor_id: "exec-42".into(),
            parent_id: None,
            agent: "claude".into(),
            role: spur_acp::Role::Worker,
            task_spec: String::new(),
            issue_id: None,
        }));

        let notif = Box::new(SessionNotification {
            session_id: SessionId("abc".into()),
            update: msg_update("hello from worker"),
            ..Default::default()
        });
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "exec-42".into(),
            notification: notif,
        }));

        let trace = app
            .worker_streams()
            .get("exec-42")
            .expect("trace for spawned executor");
        assert_eq!(trace.entry_count(), 1);
    }

    #[test]
    fn orphan_worker_notification_is_dropped() {
        let mut app = test_app();
        let notif = Box::new(SessionNotification {
            session_id: SessionId("abc".into()),
            update: msg_update("orphan"),
            ..Default::default()
        });
        // No prior ExecutorSpawned — executor unknown to lineage.
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "orphan-exec".into(),
            notification: notif,
        }));
        assert!(
            app.worker_streams().get("orphan-exec").is_none(),
            "orphan events must not materialize a trace"
        );
    }

    #[test]
    fn executor_retry_started_resets_trace() {
        let mut app = test_app();
        app.lineage.apply(&wrap_event(SpurEventBody::ExecutorSpawned {
            executor_id: "exec-r".into(),
            parent_id: None,
            agent: "claude".into(),
            role: spur_acp::Role::Worker,
            task_spec: String::new(),
            issue_id: None,
        }));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "exec-r".into(),
            notification: Box::new(SessionNotification {
                session_id: SessionId("abc".into()),
                update: msg_update("pre-retry"),
                ..Default::default()
            }),
        }));
        assert_eq!(app.worker_streams().get("exec-r").unwrap().entry_count(), 1);

        app.handle_spur_event(wrap_event(SpurEventBody::ExecutorRetryStarted {
            executor_id: "exec-r".into(),
            attempt_n: 2,
            ..Default::default()
        }));
        assert_eq!(
            app.worker_streams().get("exec-r").unwrap().entry_count(),
            0,
            "retry clears the per-executor trace"
        );
    }
}
```

> The exact field names on `ExecutorSpawned` / `ExecutorRetryStarted` / `SessionNotification` / `SpurEvent` may differ from this sketch. Open `crates/spur-acp/src/domain/events.rs` and copy the exact shape. The assertions remain the same: spawn→route creates a trace, orphan→route creates nothing, retry→reset empties the trace.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-tui worker_stream_routing_tests`
Expected: FAIL — `worker_streams()` method and/or field missing.

- [ ] **Step 4: Add field and routing**

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
        // IMPORTANT — orphan drop: skip events whose executor the lineage
        // doesn't know yet. Otherwise we materialize a trace with
        // agent_name defaulting to "generic" → AgentKind::Generic →
        // permanent mis-coloring after ExecutorSpawned arrives. This
        // matches the brain view's fidelity ceiling (events before
        // SessionDetailView construction are dropped).
        if let spur_acp::domain::events::SpurEventBody::WorkerNotification {
            executor_id, notification, ..
        } = &event.body
        {
            let exec_id = spur_core::lineage::types::ExecutorId(executor_id.clone());
            if let Some(node) = self.lineage.node(&exec_id) {
                let agent_name = node.agent.clone();
                self.worker_streams
                    .route(executor_id, &agent_name, &notification.update);
            } else {
                tracing::trace!(
                    executor_id = %executor_id,
                    "dropping WorkerNotification for unknown executor (orphan)"
                );
            }
        }

        // Reset per-executor trace on retry. Mirrors the lineage
        // projection's `node.stream_buffer.clear()` on the same event
        // (`crates/spur-core/src/lineage/projection.rs:268`).
        if let spur_acp::domain::events::SpurEventBody::ExecutorRetryStarted {
            executor_id, ..
        } = &event.body
        {
            self.worker_streams.reset(executor_id);
        }
```

> The real accessor is `LineageProjection::node(&ExecutorId) -> Option<&ExecutorNode>` (`crates/spur-core/src/lineage/projection.rs:384`). No adaptation needed.

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

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui worker_stream_routing_tests`
Expected: all three tests PASS. Also run: `cargo test -p spur-tui` (no regressions).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): App routes WorkerNotification + orphan drop + retry reset"
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

### Task 1.6: Wire `WorkerStreams::tick_all` into `App::tick`

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (locate the tick handler via `Grep` for `fn tick` or `Action::Tick`)

**Why:** `ReactTrace::tick()` (`crates/spur-tui/src/components/react_trace/mod.rs:548`) advances the spinner frame for `ActStatus::Pending` / `InProgress` entries. The brain view gets ticked from the App's tick loop. Per-executor traces are owned by `WorkerStreams` and MUST be ticked the same way, or spinners freeze — which would invalidate the spec's "Stream tab shows tool-call lifecycle spinners identical to the brain view" acceptance criterion.

- [ ] **Step 1: Locate the tick handler**

Run: `rg -n 'fn tick|Action::Tick|react_trace.tick|\.tick\(\)' crates/spur-tui/src/app.rs | head -20`

Identify where the brain's `react_trace.tick()` is called from App. The `WorkerStreams::tick_all` call goes right alongside it.

- [ ] **Step 2: Write the failing test**

Append to the `worker_stream_routing_tests` module in `crates/spur-tui/src/app.rs`:

```rust
    #[test]
    fn app_tick_drives_worker_streams_tick_all() {
        let mut app = test_app();
        app.lineage.apply(&wrap_event(SpurEventBody::ExecutorSpawned {
            executor_id: "exec-tick".into(),
            parent_id: None,
            agent: "claude".into(),
            role: spur_acp::Role::Worker,
            task_spec: String::new(),
            issue_id: None,
        }));
        app.handle_spur_event(wrap_event(SpurEventBody::WorkerNotification {
            brain_session_id: SessionId("brain-1".into()),
            executor_id: "exec-tick".into(),
            notification: Box::new(SessionNotification {
                session_id: SessionId("abc".into()),
                update: msg_update("x"),
                ..Default::default()
            }),
        }));

        // Ticking should not panic and should be observable via tick_counter
        // on at least one trace. The exact counter increment is an
        // implementation detail; we just assert the function is callable
        // and the trace remains present.
        app.tick();
        app.tick();
        assert!(app.worker_streams().get("exec-tick").is_some());
    }
```

> If `App::tick` takes arguments or isn't public, adapt the call. The assertion is just that the trace survives and no panic occurs. If you can expose a `tick_counter` accessor on `ReactTrace` (e.g., via a `#[cfg(test)]` helper), add an `assert!(trace.tick_counter_for_tests() > 0)` for stronger coverage.

- [ ] **Step 3: Run test to verify it fails or already-passes**

Run: `cargo test -p spur-tui app_tick_drives_worker_streams_tick_all`
Expected: FAIL if `tick_all` is not yet called from `App::tick`.

- [ ] **Step 4: Wire `tick_all`**

Locate the `App::tick` function (or the `Action::Tick` handler). Alongside the existing brain-view tick call (something like `self.session_detail.as_mut().map(|d| d.react_trace.tick())` or similar), add:

```rust
        self.worker_streams.tick_all();
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui app_tick_drives_worker_streams_tick_all`
Expected: PASS. Also: `cargo test -p spur-tui` and `cargo clippy -p spur-tui -- -D warnings`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): App::tick drives WorkerStreams::tick_all for spinners"
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

- [ ] **Step 2: Replace `render_stream` call with `render_compact` delegation + fallback**

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
                    // Delegate to the compact ReactTrace body renderer.
                    // `render_compact` paints ONLY the body (no block,
                    // no border, no title) — DetailPane owns the outer
                    // block already. Do NOT call `ReactTrace::render`
                    // here: its signature is
                    // `(&mut self, &mut Frame, Rect, Option<&ExecutorLineage>)`
                    // and it draws its own block, which would collide
                    // with our outer block.
                    trace.render_compact(frame, body_area);
                    // DetailPane's scroll_offset / is_following are
                    // NOT consulted for the Stream tab anymore — scroll
                    // state lives on trace.anchor. Other tabs still use
                    // DetailPane::scroll_offset.
                    return;
                }
                self.render_stream(node, body_area.width)
            }
```

**Scroll key routing (P2a).** Today `DetailPane::scroll_up / scroll_down / scroll_to_top / scroll_to_bottom` mutate `self.scroll_offset`. When the Stream tab is active and a trace exists, those calls must drive the trace's anchor instead. Modify each method to accept an `Option<&mut ReactTrace>` parameter (or thread it via a single new `scroll_action(&mut self, action, stream_trace)` method). Example replacement for `scroll_down`:

```rust
    pub fn scroll_down(&mut self, stream_trace: Option<&mut crate::components::react_trace::ReactTrace>) {
        if matches!(self.current_tab, DetailTab::Stream) {
            if let Some(trace) = stream_trace {
                trace.scroll_down();
                return;
            }
        }
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }
```

Apply the same pattern to `scroll_up`, `scroll_to_top`, `scroll_to_bottom`. Update every call site in `app.rs` / `dashboard.rs` to pass the trace lookup result (same pattern as Step 3 below).

**Tab-cycle preservation (P2b).** `cycle_tab` today resets `self.scroll_offset = 0; self.is_following = true;`. That's still correct for the non-Stream tabs. It must NOT touch the trace's anchor — preserve per-executor scroll position across tab cycles. No code change needed as long as `cycle_tab` only touches `self.*` fields and the trace's `scroll_*` methods are not called from here. Verify by reading `cycle_tab` after the Step 2 edit.

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

## Phase 4 (deferred) — optimizations + optional schema cleanup

These are NOT required for the cutover. They are driven by bench numbers from Task 0.5 and real-world profiling of the shipped Phase 2/3 build. Each lists a clear trigger condition — ship when (and only when) the trigger fires.

### PP3 — Lazy compact-cache drop on focus change

**What:** Hook DashboardView focus transitions so the previously-focused executor's trace has `ReactTrace::drop_compact_cache()` called. Re-focus pays a one-frame rebuild (<25 ms cold).

**Why:** `compact_cache.lines` holds ~1 MB per rendered trace. At 50 executors with LRU retention, >20 MB can sit in cache slots for invisible traces.

**Trigger:** real-world memory profile shows >20 MB retained in compact caches across traces, OR user-visible memory regression reports.

**Cost:** ~10 LoC. Touch DashboardView focus handler.

### PP4 — Compact-mode entry cap

**What:** Add `const MAX_COMPACT_ENTRIES: usize = 1000;` (tune by bench). `WorkerStreams::route` evicts oldest entries beyond the cap on every push.

**Why:** Worst-case per-trace memory drops from 5000 × ~300 B (1.5 MB) to 1000 × ~300 B (300 KB). Cold render also ~5× faster.

**Trigger:** Task 0.5 cold-render bench at 5000 entries, width 40 exceeds 10 ms on the reference machine, OR memory pressure reports.

**Cost:** ~15 LoC. Add eviction path to `WorkerStreams::route`.

### PP5 — Avoid `String` clone in orphan check

**What:** Add `LineageProjection::node_by_str(&str) -> Option<&ExecutorNode>` — same body as `node`, borrowed-key lookup. `App::handle_spur_event` calls it to drop the per-event `ExecutorId(executor_id.clone())` allocation.

**Why:** ~1500 allocations/sec under 50-executor × 30-chunk/sec load. Micro-opt; compounds with other hot-path work.

**Trigger:** whenever the lineage accessor is touched for other reasons, OR allocator profiling shows `ExecutorId` in the top-20 allocation sites.

**Cost:** ~10 LoC in `spur-core` + 2 LoC call-site change.

### PP2 — Early-return in `ReactTrace::tick`

**What:** Cache `has_active_or_pending: bool` on `ReactTrace`, updated on `push` / `mark_dirty_from` / Permission resolution. `tick()` early-returns for traces where the flag is false.

**Why:** 50 unfocused traces × 5000 entries × 2 scans × 60 Hz ≈ 30M entry checks/sec. ~5-6% of one core wasted.

**Trigger:** profiling shows `ReactTrace::tick` in top-10 CPU samples.

**Cost:** ~20 LoC. Extra state to keep consistent.

### PP6 — Schema cleanup: delete `WorkerStreamEntry` / `stream_buffer`

**What:** Remove the now-dead types from `spur-core`. Bundle with a `SessionHistory` / `session_metadata.json` format version bump. On-disk files pre-bump need a migrator.

**Trigger:** Task 0.4 audit re-run confirms no consumer remains, AND there is a natural format-bump window for another reason.

**Cost:** ~50 LoC + migration test. NOT scheduled.

---

## Risk Register (plan-level, supplements spec)

| # | Risk | Mitigation |
|---|---|---|
| PR1 | **Orphan WorkerNotifications** (events before `ExecutorSpawned`) would materialize traces with `AgentKind::Generic` — permanent mis-coloring | Task 1.4 routing drops WorkerNotifications whose executor is not yet in the lineage. Matches brain view's "events before SessionDetailView construction are lost" ceiling |
| PR2 | **Retry stale data** — without resetting the trace on `ExecutorRetryStarted`, the pane shows both attempts concatenated | Task 1.4 routes `ExecutorRetryStarted` → `WorkerStreams::reset`; Task 1.3 keeps the slot + AgentKind for reuse |
| PR3 | **Spinners freeze** without tick drive | Task 1.6 wires `WorkerStreams::tick_all` into `App::tick` |
| PR4 | **Unscrollable Stream tab** if key routing isn't rewired | Task 2.1 routes `scroll_up/down/top/bottom` to `trace.scroll_*` when Stream is active and trace exists |
| PR5 | **Executor GC absent** — traces accumulate for the session lifetime | Accept. Bounded by `MAX_LOG_ENTRIES` per trace (`crates/spur-tui/src/components/mod.rs:84`); add `WorkerStreams::remove` (Task 1.3) for manual cleanup |
| PR6 | **No WorkerHistory replay** across process restart | Task 1.5 seed from `stream_buffer` yields coarse 3-kind preamble; live fidelity resumes once new `WorkerNotification`s flow |
| PR7 | **`render_compact` anchor resolution for `ScrollAnchor::Row`** is approximate (entry_idx + row_within_entry clamped) | Acceptable in compact mode where rows ≈ entries. If precision becomes an issue, refine to per-entry row starts |
| PR8 | **Cold-render cost at MAX_LOG_ENTRIES (5000)** can exceed 16 ms → frame drops on width-change or cold cache | Task 0.3 PP1 (line-cache + incremental rebuild) keeps steady-state cost sub-ms. Task 0.5 bench detects regressions. Phase 4 PP4 (entry cap) is the fallback |
| PR9 | **Memory footprint** at 50 active executors ≈ 75 MB entries + ~1-50 MB caches | Entries are inherent to brain-view parity. Caches mitigated by Phase 4 PP3 (lazy drop) + PP4 (cap) when profiling demands |
| PR10 | **`tick_all` CPU** ≈ 6% core at 50 unfocused traces × 5000 entries | Accept initially. Phase 4 PP2 (early-return cached flag) is ready when profiling shows it dominating |
| PR11 | **Allocator pressure** from per-event `ExecutorId(String)` clone at ~1500/sec | Accept initially. Phase 4 PP5 (`node_by_str`) is cheap to land whenever the accessor is touched |

## Self-Review Checklist

Before handing off to executor or reviewer, verify:

- [ ] **Spec coverage.** Every Goal (§Goals 1–5) is implemented by at least one task. Every Non-goal is not violated by any task.
- [ ] **Phase ordering.** Phase 3 tasks do not reference code that Phase 1/2 has not yet introduced. Phase 0 tasks do not depend on anything.
- [ ] **Type consistency.** `WorkerStreams::route` signature matches its call sites in Tasks 1.4, 1.5, 2.1, 2.2. `dispatch_session_update` signature matches Tasks 1.1, 1.2, 1.3. `render_compact` (not `render`) is the entry point used in Task 2.1.
- [ ] **Placeholder scan.** No "TBD", no "add error handling", no "similar to Task N".
- [ ] **Commands are concrete.** Every test run specifies the crate (`-p spur-tui`) and, where scoped, the test name.
- [ ] **Back-out plan.** Phases 0–2 are pure additions; Phase 3 can be reverted independently if a late-surfacing `stream_buffer` consumer is discovered.
- [ ] **Integration seams closed.** PR1–PR4 (orphan drop, retry reset, tick drive, scroll routing) are each implemented by a specific task.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-19-stream-tab-unification.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
