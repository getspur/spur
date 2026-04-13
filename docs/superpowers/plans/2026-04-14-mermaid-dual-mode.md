# Mermaid dual-mode rendering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-04-14-mermaid-dual-mode-design.md`

**Goal:** When the terminal lacks image-protocol support, render ```` ```mermaid ```` fences as ordinary code blocks and keep the mermaid registry / overlay / dispatcher dormant.

**Architecture:** The existing `App.mermaid_picker: Option<Picker>` is already the capability bit. We thread a `mermaid_enabled: bool` derived from `picker.is_some()` through `MarkdownStream` (gating the stage-1 fence discovery) and through `ReactTrace` (so newly-created streams inherit the bit). `session_detail.rs` gates the Alt-v binding on the same bit, and `help_overlay.rs` suppresses mermaid-related rows when disabled. `mermaid.rs` is not modified — per spec, minimal-touch.

**Tech Stack:** Rust, `ratatui`, `ratatui_image`, `pulldown-cmark`, `tui-markdown`. Cargo workspace. Tests via `cargo test -p spur-tui`.

---

## File structure

**Modify:**
- `crates/spur-tui/src/components/markdown_stream.rs` — add `mermaid_enabled` field + constructor; gate stage-1 fence scan in `rebuild`.
- `crates/spur-tui/src/components/react_trace.rs` — track `mermaid_enabled`; plumb into lazily-constructed `MarkdownStream`.
- `crates/spur-tui/src/views/session_detail.rs` — gate Alt-v dispatch; propagate `mermaid_enabled` into `ReactTrace` when `set_render_picker` is called.
- `crates/spur-tui/src/components/help_overlay.rs` — accept a `mermaid_enabled: bool` and conditionally omit Alt-v and the "Mermaid Viewer" section.
- `crates/spur-tui/src/app.rs` — pass `self.mermaid_picker.is_some()` when rendering the help overlay.

**Do not modify:**
- `crates/spur-tui/src/components/mermaid.rs` — dormant in text mode (per spec's minimal-touch decision).
- `crates/spur-tui/src/views/mermaid_viewer.rs` — construction already gated by `NavigateTo(ViewId::MermaidOverlay)`, which only fires from the Alt-v path we're disabling.

---

## Task 1 — `MarkdownStream` gains a `mermaid_enabled` bit

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
- Test: `crates/spur-tui/src/components/markdown_stream.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add at the bottom of the existing `#[cfg(test)] mod tests` block (near line ~375):

```rust
#[test]
fn text_mode_renders_mermaid_fence_as_code_block() {
    use super::StateLookup;
    let mut s = MarkdownStream::new_with_mermaid(false);
    s.append("Intro\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nOutro\n");
    let _ = s.flush_now(&StateLookup::empty());

    // No fence item should be produced — mermaid should fall through to
    // tui-markdown as an ordinary code block.
    let has_fence = s.items().iter().any(|it| matches!(it, StreamItem::Fence(_)));
    assert!(!has_fence, "text mode must not produce Fence items: {:?}", s.items());

    // Body text should appear verbatim in the rendered output.
    let joined = s.cached_lines_debug().join("\n");
    assert!(joined.contains("flowchart LR"), "expected mermaid source in output: {joined}");
    assert!(joined.contains("A-->B"), "expected mermaid source in output: {joined}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --lib text_mode_renders_mermaid_fence_as_code_block`
Expected: FAIL with `no function or associated item named 'new_with_mermaid'`.

- [ ] **Step 3: Add the field, constructor, and gate**

In `crates/spur-tui/src/components/markdown_stream.rs`, modify the struct (around line 64):

```rust
#[derive(Debug, Clone)]
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_items: Vec<StreamItem>,
    fence_placeholders: std::collections::HashMap<MermaidId, Line<'static>>,
    known_fences: Vec<FenceRef>,
    next_fence_id: u64,
    /// When false, ```` ```mermaid ```` fences are not extracted — they flow
    /// through to tui-markdown as ordinary code blocks. Set at construction
    /// time from the terminal's image-protocol capability.
    mermaid_enabled: bool,
}

impl Default for MarkdownStream {
    fn default() -> Self {
        Self {
            raw_text: String::new(),
            dirty_since: None,
            cached_items: Vec::new(),
            fence_placeholders: std::collections::HashMap::new(),
            known_fences: Vec::new(),
            next_fence_id: 0,
            // Default preserves existing behavior for tests and any caller
            // that pre-dates the dual-mode work.
            mermaid_enabled: true,
        }
    }
}
```

Remove `#[derive(Default)]` from the struct (we're replacing it with the explicit `impl Default` above).

Add next to `new`:

```rust
impl MarkdownStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with explicit mermaid support. `false` makes
    /// ```` ```mermaid ```` fences render as ordinary code blocks.
    pub fn new_with_mermaid(mermaid_enabled: bool) -> Self {
        Self { mermaid_enabled, ..Self::default() }
    }
    // ... existing methods stay ...
}
```

Gate the stage-1 discovery inside `rebuild` (line 158). Wrap the existing stage-1 block:

```rust
fn rebuild(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
    // ── Stage 1: pre-scan raw_text for closed ```mermaid fences ───────
    let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    if self.mermaid_enabled {
        use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
        let parser = Parser::new_ext(&self.raw_text, Options::empty()).into_offset_iter();
        // ... existing loop unchanged ...
    }
    // ... rest of rebuild unchanged ...
}
```

(Remove the `{ ... }` block braces that currently scope the inner `use`; they're no longer needed once the whole loop is inside the `if`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --lib text_mode_renders_mermaid_fence_as_code_block`
Expected: PASS.

- [ ] **Step 5: Run the full `markdown_stream` test suite to confirm no regressions**

Run: `cargo test -p spur-tui --lib markdown_stream::`
Expected: all existing tests still pass (they use `MarkdownStream::new()` which defaults `mermaid_enabled = true`).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs
git commit -m "feat(spur-tui): mermaid_enabled flag on MarkdownStream

Adds new_with_mermaid(bool) and gates the stage-1 fence discovery.
Default constructor keeps existing image-mode behavior."
```

---

## Task 2 — `ReactTrace` tracks and forwards `mermaid_enabled`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs`
- Test: same file (inline tests)

- [ ] **Step 1: Write the failing test**

Add at the bottom of `react_trace`'s test module:

```rust
#[test]
fn text_mode_agent_message_stream_produces_no_fence_items() {
    use crate::components::markdown_stream::{StateLookup, StreamItem};
    let mut trace = ReactTrace::new();
    trace.set_mermaid_enabled(false);
    trace.handle_agent_message(
        "claude",
        "Here's a diagram:\n\n```mermaid\nflowchart LR\nA-->B\n```\n",
        chrono::Utc::now(),
    );
    // Force the stream to flush.
    for entry in trace.entries_mut_for_test() {
        if let Some(stream) = entry.markdown.as_mut() {
            let _ = stream.flush_now(&StateLookup::empty());
            let has_fence = stream.items().iter().any(|it| matches!(it, StreamItem::Fence(_)));
            assert!(!has_fence, "text mode must not produce Fence items");
        }
    }
}
```

Note: `entries_mut_for_test` and `set_mermaid_enabled` don't exist yet — that's what this test forces into existence.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --lib text_mode_agent_message_stream_produces_no_fence_items`
Expected: FAIL with `no method named 'set_mermaid_enabled'` (and/or `entries_mut_for_test`).

- [ ] **Step 3: Add the field + setter + test helper**

In `crates/spur-tui/src/components/react_trace.rs`, add a field to the `ReactTrace` struct (search for the struct definition; it should be near the top alongside `entries`, `is_following`, etc.):

```rust
pub struct ReactTrace {
    // ... existing fields ...
    /// Whether mermaid rendering is available. Set from the session view
    /// when the terminal picker is probed. Forwarded to newly-created
    /// `MarkdownStream` instances.
    mermaid_enabled: bool,
}
```

Initialize it to `true` in `ReactTrace::new` (or `Default::default` if that's how it's built — match the existing pattern).

Add the setter:

```rust
impl ReactTrace {
    pub fn set_mermaid_enabled(&mut self, enabled: bool) {
        self.mermaid_enabled = enabled;
    }

    #[cfg(test)]
    pub fn entries_mut_for_test(&mut self) -> &mut [TraceEntry] {
        &mut self.entries
    }
}
```

Change the stream construction at line 307:

```rust
let mut stream = super::markdown_stream::MarkdownStream::new_with_mermaid(
    self.mermaid_enabled,
);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --lib text_mode_agent_message_stream_produces_no_fence_items`
Expected: PASS.

- [ ] **Step 5: Run the full `react_trace` test suite**

Run: `cargo test -p spur-tui --lib react_trace::`
Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "feat(spur-tui): ReactTrace forwards mermaid_enabled to streams

New set_mermaid_enabled setter propagates terminal capability into
lazily-created MarkdownStream instances for agent-message rendering."
```

---

## Task 3 — `SessionDetailView` propagates + gates Alt-v

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Write the failing test**

Add to the bottom of the existing test module in `session_detail.rs` (find `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
#[test]
fn alt_v_is_inert_when_render_picker_is_none() {
    use crate::action::Action;
    use crate::views::ViewId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::SessionId;

    let mut view = SessionDetailView::new(SessionId::from("s1"));
    view.set_render_picker(None);

    let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
    let action = view.handle_key(key);

    // Must not navigate into the mermaid overlay.
    match action {
        Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {
            panic!("Alt-v must not navigate to mermaid overlay when picker is None");
        }
        _ => {}
    }
}

#[cfg(test)]
#[test]
fn alt_v_opens_overlay_when_render_picker_is_some() {
    use crate::action::Action;
    use crate::views::ViewId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::SessionId;

    // Build a dummy Picker via the font-size constructor to avoid touching stdio.
    let picker = ratatui_image::picker::Picker::from_fontsize((8, 16));
    let mut view = SessionDetailView::new(SessionId::from("s1"));
    view.set_render_picker(Some(picker));

    let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
    match view.handle_key(key) {
        Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
        other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
    }
}
```

If `ratatui_image::picker::Picker::from_fontsize` isn't the exact constructor name in the version in `Cargo.toml`, substitute the available non-IO constructor (check `ratatui_image`'s docs with `cargo doc --open -p ratatui_image` or grep its source; common alternatives are `Picker::new((w, h))` or `Picker::from_font_size`). The goal is a `Picker` that doesn't touch terminal stdio.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --lib alt_v_is_inert_when_render_picker_is_none alt_v_opens_overlay_when_render_picker_is_some`
Expected: `alt_v_is_inert_when_render_picker_is_none` FAILS (Alt-v currently always returns the Navigate action); the other may fail or pass depending on current behavior.

- [ ] **Step 3: Gate Alt-v in `handle_key`**

Modify `crates/spur-tui/src/views/session_detail.rs` around line 561:

```rust
#[cfg(feature = "markdown")]
if matches!(key.code, KeyCode::Char('v')) && key.modifiers.contains(KeyModifiers::ALT) {
    if self.render_picker.is_some() {
        return Some(Action::NavigateTo(ViewId::MermaidOverlay(
            self.session_id.clone(),
        )));
    }
    // No image protocol → Alt-v is inert. Do not consume the key; let it
    // fall through to the normal input-bar path so typing 'v' still works
    // if the user combined Alt+v with text-entry focus (rare, but benign).
    // Returning `None` here means the caller treats the key as unhandled.
}
```

Also propagate the capability into `ReactTrace`. Find `set_render_picker` (line ~205) and update:

```rust
pub fn set_render_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
    let enabled = picker.is_some();
    self.render_picker = picker;
    self.react_trace.set_mermaid_enabled(enabled);
}
```

(If the field holding `ReactTrace` on `SessionDetailView` is named differently, substitute the actual name — it should appear in the struct definition near the top of the file.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --lib alt_v_is_inert_when_render_picker_is_none alt_v_opens_overlay_when_render_picker_is_some`
Expected: both PASS.

- [ ] **Step 5: Run the full session_detail suite**

Run: `cargo test -p spur-tui --lib session_detail::`
Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): gate Alt-v on image-protocol support

Alt-v is inert when render_picker is None. set_render_picker also
forwards the capability into ReactTrace so child streams render
mermaid fences as plain code blocks."
```

---

## Task 4 — Help overlay hides mermaid entries in text mode

**Files:**
- Modify: `crates/spur-tui/src/components/help_overlay.rs`
- Modify: `crates/spur-tui/src/app.rs` — caller threads the capability bit.

- [ ] **Step 1: Write the failing test**

Add at the bottom of `help_overlay.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the raw text content of all lines produced for the help
    /// overlay, given a mermaid-capable flag. We avoid `Frame` by calling
    /// the helper below, which the production `render` fn also uses.
    fn help_lines(mermaid_enabled: bool) -> Vec<String> {
        HelpOverlay::lines(mermaid_enabled)
            .into_iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn image_mode_mentions_alt_v_and_mermaid_viewer() {
        let joined = help_lines(true).join("\n");
        assert!(joined.contains("Alt-v"), "expected Alt-v in image-mode help: {joined}");
        assert!(joined.contains("Mermaid Viewer"), "expected Mermaid Viewer section: {joined}");
    }

    #[test]
    fn text_mode_omits_alt_v_and_mermaid_viewer() {
        let joined = help_lines(false).join("\n");
        assert!(!joined.contains("Alt-v"), "Alt-v must be hidden in text mode: {joined}");
        assert!(
            !joined.contains("Mermaid Viewer"),
            "Mermaid Viewer section must be hidden: {joined}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --lib help_overlay::tests::`
Expected: FAIL with `no function or associated item named 'lines'`.

- [ ] **Step 3: Refactor `HelpOverlay` to expose the line list and accept the flag**

Rewrite the body of `crates/spur-tui/src/components/help_overlay.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn render(frame: &mut Frame, area: Rect, mermaid_enabled: bool) {
        let width = 66u16.min(area.width.saturating_sub(4));
        let height = 42u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let paragraph = Paragraph::new(Self::lines(mermaid_enabled)).block(block);
        frame.render_widget(paragraph, popup_area);
    }

    /// Builds the help text. Exposed so tests can assert on contents
    /// without constructing a `Frame`.
    pub fn lines(mermaid_enabled: bool) -> Vec<Line<'static>> {
        let header = |t: &'static str| {
            Line::from(Span::styled(
                t,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))
        };

        let mut out: Vec<Line<'static>> = vec![
            header(" Dashboard — Lineage Tree"),
            Line::from("  j / k              Move selection in lineage tree"),
            Line::from("  Enter              Focus selected node"),
            Line::from("  Esc                Unfocus (return to log) / quit"),
            Line::from("  \u{2190} / \u{2192}               Cycle detail tabs (when focused)"),
            Line::from("  c                  Toggle collapse on selected subtree"),
            Line::from("  r                  Jump to next pending review"),
            Line::from("  a / d / m / R      Approve / deny / modify / retry (review tab)"),
            Line::from(""),
            header(" Dashboard — General"),
            Line::from("  j/k, Up/Down       Scroll activity log"),
            Line::from("  g / G              Jump to top / bottom"),
            Line::from("  Tab                Cycle panel focus"),
            Line::from("  v                  Toggle verbose mode"),
            Line::from("  s                  Open session picker"),
            Line::from("  q, Esc             Quit"),
            Line::from(""),
            header(" Session Picker"),
            Line::from("  j/k, Up/Down       Navigate list"),
            Line::from("  Enter              Resume / create (on [+ New])"),
            Line::from("  /                  Focus search field"),
            Line::from("  n                  New session"),
            Line::from("  R                  Rename selected"),
            Line::from("  d                  Archive (or unarchive)"),
            Line::from("  p                  Toggle pin"),
            Line::from("  a                  Toggle show-archived"),
            Line::from("  P                  Toggle preview pane"),
            Line::from("  r                  Refresh list"),
            Line::from("  Esc                Clear filter → back"),
            Line::from(""),
            header(" Session Detail"),
            Line::from("  (type)             Input goes to chat bar"),
            Line::from("  Enter              Send message"),
            Line::from("  ! + Enter          Interrupt & send"),
            Line::from("  Esc                Back to Dashboard"),
            Line::from("  y / n / a          Permission: yes/no/always"),
            Line::from("  Alt-m              Toggle plan mode"),
        ];

        if mermaid_enabled {
            out.push(Line::from("  Alt-v              Open mermaid diagram viewer"));
        }

        out.push(Line::from(""));

        if mermaid_enabled {
            out.push(header(" Mermaid Viewer (overlay)"));
            out.push(Line::from("  [ / ]              Cycle diagrams"));
            out.push(Line::from("  q / Esc            Close overlay"));
            out.push(Line::from(""));
        }

        out.push(Line::from(Span::styled(
            " Press ? or Esc to close",
            Style::default().fg(Color::DarkGray),
        )));

        out
    }
}
```

- [ ] **Step 4: Update the caller in `app.rs`**

Find the site that calls `HelpOverlay::render(frame, area)` (there should be one — `grep -n "HelpOverlay::render" crates/spur-tui/src/app.rs`). Change it to:

```rust
HelpOverlay::render(frame, area, self.mermaid_picker.is_some());
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p spur-tui --lib help_overlay::tests::`
Expected: both new tests PASS.

- [ ] **Step 6: Build the full crate to catch any caller we missed**

Run: `cargo build -p spur-tui`
Expected: clean build (no "missing argument" errors).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/help_overlay.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): help overlay hides mermaid entries in text mode

HelpOverlay::render now takes a mermaid_enabled flag; app.rs passes
mermaid_picker.is_some(). Exposes HelpOverlay::lines for testability."
```

---

## Task 5 — Full-workspace verification

**Files:** none modified.

- [ ] **Step 1: Run the spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 2: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 3: Lint**

Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
Expected: no warnings. Fix any `dead_code`, `unused_variables`, or visibility warnings introduced by the new `pub fn lines` / `pub fn set_mermaid_enabled` / `#[cfg(test)] pub fn entries_mut_for_test` inline before proceeding.

- [ ] **Step 4: Manual smoke test — image-capable terminal**

Run: `cargo run -p spur-tui -- --dashboard`
From a kitty / iTerm2 / WezTerm session, open a session that emits a mermaid fence. Confirm: diagram renders inline as an image, Alt-v opens the overlay, help (`?`) lists Alt-v + "Mermaid Viewer" section.

- [ ] **Step 5: Manual smoke test — text-mode terminal**

Run: `TERM=xterm-256color cargo run -p spur-tui -- --dashboard` inside a plain xterm or `tmux` session without image passthrough, or force text mode with `script /dev/null -q -c "cargo run -p spur-tui -- --dashboard"` to strip tty features. Open a session with a mermaid fence. Confirm: fence renders as a code block with verbatim source, Alt-v does nothing, help (`?`) omits Alt-v and the "Mermaid Viewer" section.

If either smoke test can't be performed in the current environment, document which in the final commit message so a reviewer knows.

- [ ] **Step 6: Commit the verification note (only if tests/clippy produced changes)**

If Step 3 forced any inline fixes, commit them:

```bash
git add -p
git commit -m "chore(spur-tui): clippy cleanups around dual-mode gating"
```

Otherwise, skip this commit.

---

## Self-Review Checklist

- **Spec coverage:**
  - Capability model (`Option<Picker>` as the mode): Task 3 reads it at the session-view layer; Task 4 reads it at the app layer; both plumb from `App.mermaid_picker`. ✓
  - `markdown_stream.rs` fence classification gating: Task 1. ✓
  - `app.rs` / `MermaidViewerView` gating: `MermaidViewerView` is only constructed via `NavigateTo(MermaidOverlay)`, which only fires from Alt-v — Task 3 gates that. ✓
  - `help_overlay.rs` suppression: Task 4. ✓
  - `mermaid.rs` untouched: confirmed — no task modifies it. ✓
  - Tests 1/2/3 from spec: Task 1 covers "markdown_stream treats `mermaid` as code block"; Task 2 covers "`render_mermaid` never called" indirectly (no Fence item means no id means nothing enqueued on `mermaid_tx`); Task 4 covers "help hides Alt-v in text mode". ✓
- **Placeholder scan:** no TBD/TODO; all code blocks contain real code; test blocks are complete. ✓
- **Type consistency:** `mermaid_enabled` is a `bool` everywhere; `new_with_mermaid`, `set_mermaid_enabled`, `HelpOverlay::lines` / `HelpOverlay::render(.., bool)` are consistent across call sites. ✓
