# Session Picker UX Refresh — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce friction on the dominant session-picker journey ("get me back to my last session"), bring projection rules in line with the data model, and align the footer/StatusBar hint surface with the actual keybinds for each picker mode.

**Architecture:** Surgical changes within `crates/spur-tui` — projection rules in `set_sessions`, row layout reordering in `render_populated`, context-sensitive `footer_hint` function, single new `Action::CopySessionId` variant handled by `App` via OSC 52 escape, render goldens via existing `TestBackend` pattern. No metadata schema migration. No new crate dependencies.

**Tech Stack:** Rust, ratatui (`TestBackend` for golden tests), crossterm key events, `nucleo_matcher` (already wired), `base64` (already in workspace lockfile via `spur-acp`), OSC 52 terminal escape.

**Spec:** `docs/superpowers/specs/2026-04-25-session-picker-ux-refresh-design.md`

---

## File Structure

**Modified files:**
- `crates/spur-tui/src/views/session_picker.rs` — projection rules P1/P2 in `set_sessions`, `brains_are_heterogeneous` helper, row layout in `render_populated`, `footer_hint` function (replaces `FOOTER_HINT` const), `y` keybind in `handle_key`.
- `crates/spur-tui/src/components/status_bar.rs` — add `view_hint_override: Option<&'a str>` to `StatusBarProps`, prefer it over the hardcoded view hint when `Some`.
- `crates/spur-tui/src/action.rs` — add `Action::CopySessionId(String)` variant.
- `crates/spur-tui/src/app.rs` — handle `Action::CopySessionId` by writing the OSC 52 escape to stdout.
- `crates/spur-tui/tests/session_picker_interactions.rs` — update existing tests for new cursor-default semantics; add new behavioral tests.

**New files:**
- `crates/spur-tui/tests/session_picker_render_snapshots.rs` — render goldens via `TestBackend` and inline `expected: &[&str]`.

---

## Task 1: Render-test harness with one current-layout golden

**Files:**
- Create: `crates/spur-tui/tests/session_picker_render_snapshots.rs`

Establishes the harness pattern. Captures the *current* layout for `populated_single_brain_no_filter` so subsequent layout changes show as plain string diffs in the PR.

- [ ] **Step 1: Create the test file with one failing golden**

Create `crates/spur-tui/tests/session_picker_render_snapshots.rs`:

```rust
//! Render goldens for SessionPickerView. Inline `expected: &[&str]` per branch.
//! No external snapshot crate — diffs review as plain strings in PRs.

use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
use spur_acp::SessionInfo;
use spur_tui::session_metadata::SessionMetadata;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

const W: u16 = 80;
const H: u16 = 24;

fn buffer_to_lines(buf: &Buffer) -> Vec<String> {
    let mut out = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        out.push(row.trim_end().to_string());
    }
    out
}

static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
    std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);

fn assert_render(picker: &mut SessionPickerView, expected: &[&str]) {
    let backend = TestBackend::new(W, H);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect::new(0, 0, W, H);
        let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
        picker.render(f, area, &ctx);
    })
    .unwrap();
    let lines = buffer_to_lines(term.backend().buffer());
    assert_eq!(
        lines.len(),
        expected.len(),
        "row count mismatch: actual {} vs expected {}",
        lines.len(),
        expected.len()
    );
    for (i, (got, want)) in lines.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "row {i} mismatch:\n  got:  {got:?}\n  want: {want:?}");
    }
}

fn session(id: &str, title: &str, cwd: &str) -> SessionInfo {
    SessionInfo::new(
        std::sync::Arc::<str>::from(id),
        std::path::PathBuf::from(cwd),
    )
    .title(title.to_string())
}

#[test]
fn populated_single_brain_no_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "claude".into(),
        vec![session("a1b2c3d4e5", "Refactor auth flow", "/work/spur")],
    );

    // Inline golden — capture current layout exactly. Any visual change to
    // session_picker.rs invalidates this and is reviewed as a plain string diff.
    let expected: &[&str] = &[
        "Sessions (claude)",
        "  Search",
        "",
        "▸ + Start new session",
        "  ────",
        "  a1b2c3d4 · Refactor auth flow  just now",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        " [↑↓]navigate [Enter]select [Esc]back",
        " j/k nav · Enter resume · / search · n new · R rename · d archive · a show-archived · p pin · P preview · r refresh · Esc back",
    ];
    assert_render(&mut picker, expected);
}
```

- [ ] **Step 2: Run the test to observe the actual output**

Run: `cargo test -p spur-tui --test session_picker_render_snapshots populated_single_brain_no_filter 2>&1 | head -80`
Expected: FAIL with row-mismatch messages. The first failure tells you the actual current layout strings.

- [ ] **Step 4: Update the inline `expected` array to match the actual current output**

Copy the actual rows from the `got:` lines in the test failure output into the `expected` array. Re-run.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p spur-tui --test session_picker_render_snapshots populated_single_brain_no_filter`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/tests/session_picker_render_snapshots.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): add session picker render-golden harness (current layout)

One inline-string golden for populated_single_brain_no_filter against the
current layout — proves the TestBackend harness works before any visual
change. Subsequent layout changes will update this golden as plain string
diffs reviewable in the PR.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Projection rule P1 — cursor default lands on `last_active_session_id`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs:162-180` (`set_sessions`)
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs` (add new tests, update existing if affected)

When the picker transitions Loading → Populated, cursor lands on the row whose `session_id` equals `metadata.last_active_session_id`. Fallback chain: last-active → first visible row → row 0 (`[+ New]`).

- [ ] **Step 1: Write failing test for cursor-default = last_active**

Add to `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
#[test]
fn cursor_default_lands_on_last_active_when_present() {
    let mut picker = SessionPickerView::new();
    let mut meta = spur_tui::session_metadata::SessionMetadata::default();
    meta.last_active_session_id = Some("a2".to_string());
    picker.set_metadata(meta);
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta"), session("a3", "gamma")],
    );
    // a2 is the second session; in virtual cursor space [+ New]=0, a1=1, a2=2.
    assert_eq!(picker.cursor(), 2);
}

#[test]
fn cursor_default_falls_back_to_first_row_when_last_active_absent() {
    let mut picker = SessionPickerView::new();
    let meta = spur_tui::session_metadata::SessionMetadata::default();
    picker.set_metadata(meta);
    picker.set_sessions("t".into(), vec![session("a1", "alpha"), session("a2", "beta")]);
    assert_eq!(picker.cursor(), 1);
}

#[test]
fn cursor_default_falls_back_to_zero_when_no_sessions() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(spur_tui::session_metadata::SessionMetadata::default());
    picker.set_sessions("t".into(), vec![]);
    assert_eq!(picker.cursor(), 0);
}

#[test]
fn cursor_default_falls_back_when_last_active_not_in_visible_list() {
    let mut picker = SessionPickerView::new();
    let mut meta = spur_tui::session_metadata::SessionMetadata::default();
    meta.last_active_session_id = Some("does-not-exist".to_string());
    picker.set_metadata(meta);
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    // last_active id is unknown → fall back to row 1.
    assert_eq!(picker.cursor(), 1);
}
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p spur-tui --test session_picker_interactions cursor_default 2>&1 | tail -20`
Expected: FAIL — current `set_sessions` always defaults cursor to 0 (or to `prev_cursor`).

- [ ] **Step 3: Replace `set_sessions` body with P1 fallback chain**

In `crates/spur-tui/src/views/session_picker.rs`, replace the body of `set_sessions` (currently at lines 162-180):

```rust
pub fn set_sessions(&mut self, agent: String, sessions: Vec<SessionInfo>) {
    // P2 (cursor preservation by session_id) — only meaningful when we
    // were already Populated. Captured here so it sees the *previous*
    // state before we overwrite it.
    let prev_highlight = self.highlighted_session_id();
    let prev_cursor_was_new = matches!(
        &self.state,
        PickerState::Populated { cursor: 0, .. }
    );
    let prev_filter = match &self.state {
        PickerState::Populated { filter, .. } => filter.clone(),
        _ => String::new(),
    };

    let indices = Self::filtered_indices(
        &sessions,
        &prev_filter,
        &self.metadata,
        self.show_archived,
    );

    let cursor = if prev_cursor_was_new {
        // P2 special case: user explicitly selected [+ New] before refresh — don't move them.
        0
    } else if let Some(c) = prev_highlight.as_ref().and_then(|id| {
        indices
            .iter()
            .position(|&i| sessions[i].session_id.0.as_ref() == id.as_str())
            .map(|p| p + 1)
    }) {
        // P2: preserve highlight by session_id when possible.
        c
    } else if let Some(c) = self
        .metadata
        .last_active_session_id
        .as_deref()
        .and_then(|id| {
            indices
                .iter()
                .position(|&i| sessions[i].session_id.0.as_ref() == id)
                .map(|p| p + 1)
        })
    {
        // P1: fall back to last-active session.
        c
    } else if !indices.is_empty() {
        // P1: fall back to the first visible row.
        1
    } else {
        // P1: no sessions at all — cursor on [+ New].
        0
    };

    self.state = PickerState::Populated {
        agent,
        sessions,
        cursor,
        search_focused: false,
        filter: prev_filter,
    };
    self.scroll_offset.set(0);
}
```

This implementation handles both P1 and P2 in one pass — see Task 3 for P2-specific tests.

- [ ] **Step 4: Run new tests to verify they pass**

Run: `cargo test -p spur-tui --test session_picker_interactions cursor_default`
Expected: PASS — all four new tests.

- [ ] **Step 5: Run the full picker test suite to catch breakage in existing tests**

Run: `cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -40`
Expected: All existing tests still pass. If any fail (likely candidates: tests that asserted `cursor == 0` after first `set_sessions` without a `last_active`), update them to match the new contract — they should now expect `cursor == 1` when there is at least one session.

For each failing existing test, the fix is one of:
- Add `picker.set_metadata(SessionMetadata::default())` before `set_sessions` if metadata wasn't set (defensive — `set_metadata` is no-op for default).
- If the test legitimately depends on cursor=0 default, change the assertion to `assert_eq!(picker.cursor(), 1)`.

- [ ] **Step 6: Run the inline `current_session_shortcut_tests` module too**

Run: `cargo test -p spur-tui session_picker::current_session_shortcut_tests`
Expected: PASS — these tests use `Down` keypresses to move the cursor before assertion, so the new cursor-default doesn't affect them.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): picker cursor lands on last_active_session_id (P1)

set_sessions now resolves initial cursor via fallback chain:
  last_active_session_id (if visible) → first visible row → [+ New].

Eliminates the two-keystroke (j+Enter) friction on the dominant journey
"get me back to my last session" — Enter alone now resumes the last
session whenever it exists in the visible list.

Also folds in the P2 cursor-preservation rule (preserve highlighted
session_id across refresh) — Task 3 adds dedicated tests for P2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Projection rule P2 — cursor preservation by session_id

**Files:**
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs` (add P2 tests)

The P2 implementation already landed in Task 2's `set_sessions` rewrite. This task adds dedicated tests proving cursor follows the highlighted *session* (not the index) across pin/archive/refresh.

- [ ] **Step 1: Write failing test for cursor preservation across reorder**

Add to `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
#[test]
fn cursor_preserved_by_session_id_after_set_sessions_reorders_list() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(spur_tui::session_metadata::SessionMetadata::default());
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta"), session("a3", "gamma")],
    );

    // Move cursor to a3 (cursor 3 = third session row).
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &test_ctx());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &test_ctx());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &test_ctx());
    assert_eq!(picker.cursor(), 3);

    // Simulate refresh that reorders the list (a3 first now).
    picker.set_sessions(
        "t".into(),
        vec![session("a3", "gamma"), session("a1", "alpha"), session("a2", "beta")],
    );

    // Cursor should follow a3, which is now at row 1.
    assert_eq!(picker.cursor(), 1);
    assert_eq!(picker.visible_session_at(0).map(|s| s.session_id.0.as_ref()), Some("a3"));
}

#[test]
fn cursor_preserves_new_session_row_across_refresh() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata({
        let mut m = spur_tui::session_metadata::SessionMetadata::default();
        m.last_active_session_id = Some("a1".to_string());
        m
    });
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    // last_active=a1 → cursor lands on row 1 by P1.
    assert_eq!(picker.cursor(), 1);

    // User explicitly moves to [+ New].
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    assert_eq!(picker.cursor(), 0);

    // Refresh.
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);

    // P2 special case: cursor==0 stays at 0 (don't yank the user away from [+ New]).
    assert_eq!(picker.cursor(), 0);
}

#[test]
fn cursor_falls_through_to_p1_when_highlighted_session_disappears() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata({
        let mut m = spur_tui::session_metadata::SessionMetadata::default();
        m.last_active_session_id = Some("a1".to_string());
        m
    });
    picker.set_sessions("t".into(), vec![session("a1", "alpha"), session("a2", "beta")]);
    // Move to a2.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &test_ctx());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &test_ctx());
    assert_eq!(picker.cursor(), 2);

    // Refresh with a2 missing — P2 finds nothing; falls through to P1, which lands on a1.
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    assert_eq!(picker.cursor(), 1);
    assert_eq!(picker.visible_session_at(0).map(|s| s.session_id.0.as_ref()), Some("a1"));
}
```

- [ ] **Step 2: Run new tests to verify they pass**

Run: `cargo test -p spur-tui --test session_picker_interactions cursor_preserved cursor_preserves cursor_falls_through`
Expected: PASS — Task 2's `set_sessions` rewrite already implemented P2.

- [ ] **Step 3: Run the full picker test suite**

Run: `cargo test -p spur-tui --test session_picker_interactions`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): cursor preservation by session_id across set_sessions (P2)

Three new behavioral tests covering:
  1. cursor follows the highlighted session_id when the list reorders;
  2. cursor==0 ([+ New]) is preserved across refresh — never silently
     yanked away from the user's explicit selection;
  3. when the highlighted session disappears from the new list, P2 falls
     through to P1 (last_active → first row → [+ New]).

P2 implementation landed in the prior commit's set_sessions rewrite.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `brains_are_heterogeneous` helper

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (add helper near `cwds_are_heterogeneous` at line 329)

Pure-function helper used by Task 5's row layout to decide whether to render the brain column. Mirrors the established `cwds_are_heterogeneous` pattern.

- [ ] **Step 1: Add the helper function**

In `crates/spur-tui/src/views/session_picker.rs`, immediately below `cwds_are_heterogeneous` (currently line 329-335), add:

```rust
fn brains_are_heterogeneous(
    sessions: &[SessionInfo],
    metadata: &SessionMetadata,
) -> bool {
    if sessions.len() <= 1 {
        return false;
    }
    let first = metadata
        .sessions
        .get(sessions[0].session_id.0.as_ref())
        .and_then(|e| e.brain_name.as_deref());
    sessions.iter().any(|s| {
        let b = metadata
            .sessions
            .get(s.session_id.0.as_ref())
            .and_then(|e| e.brain_name.as_deref());
        b != first
    })
}
```

Note: takes `&[SessionInfo]` and `&SessionMetadata`. Same shape as `cwds_are_heterogeneous` so the call site (Task 5) stays symmetric.

- [ ] **Step 2: Verify the file still compiles**

Run: `cargo check -p spur-tui`
Expected: Clean compile.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add brains_are_heterogeneous helper for picker row layout

Mirrors cwds_are_heterogeneous. Used by render_populated to decide
whether to render the brain_name column on each session row — when all
sessions share a brain, the column is hidden to keep rows compact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Refactor row layout — title-dominant, brain column, demoted ID

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs:522-595` (the per-row span builder inside `render_populated`)
- Modify: `crates/spur-tui/tests/session_picker_render_snapshots.rs` (golden update happens in Task 10)

Reorders spans so title is the visually dominant element. Brain column appears only when `brains_are_heterogeneous`. Short ID demoted to muted DarkGray suffix.

Brain-column visibility is assertion is consolidated in Task 10's render goldens (`populated_single_brain_no_filter` does NOT contain a brain column; `populated_multi_brain_no_filter` DOES). No standalone behavioral test for column visibility is added here — render-level differences are best asserted at the render layer.

- [ ] **Step 1: Replace the per-row span builder in `render_populated`**

In `crates/spur-tui/src/views/session_picker.rs`, find the loop that builds session-row spans (currently around lines 522-595, starting with `for (display_i, real_i) in indices.iter().enumerate().skip(scroll).take(visible_height)`). Replace it with:

```rust
let show_brain = Self::brains_are_heterogeneous(sessions, &self.metadata);

for (display_i, real_i) in indices.iter().enumerate().skip(scroll).take(visible_height) {
    let session = &sessions[*real_i];
    let is_selected = cursor == display_i + 1;
    let prefix = if is_selected { "\u{25b8} " } else { "  " };
    let raw_id = session.session_id.0.as_ref();
    let short_id = &raw_id[..8.min(raw_id.len())];
    let display = Self::resolved_title(session, &self.metadata, show_cwd);
    let time_str = session
        .updated_at
        .as_deref()
        .map(Self::relative_time)
        .unwrap_or_default();

    let cwd_suffix = if show_cwd {
        format!("  {}/", Self::cwd_basename(&session.cwd))
    } else {
        String::new()
    };

    let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
    let archived = entry.map(|e| e.archived).unwrap_or(false);
    let pinned = entry.map(|e| e.pinned).unwrap_or(false);
    let brain = entry.and_then(|e| e.brain_name.as_deref()).unwrap_or("");

    // Style: archived → DarkGray everything; selected → Bold White title; otherwise default.
    let title_style = if archived {
        Style::default().fg(Color::DarkGray)
    } else if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let muted_style = Style::default().fg(Color::DarkGray);

    let mut spans: Vec<Span> = Vec::with_capacity(10);
    spans.push(Span::styled(
        prefix,
        if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        },
    ));
    if pinned {
        spans.push(Span::styled("\u{2b50} ", Style::default().fg(Color::Yellow)));
    }
    // Title leads — recognition data dominant.
    spans.push(Span::styled(display, title_style));
    spans.push(Span::styled(cwd_suffix, muted_style));
    if show_brain {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(brain.to_string(), muted_style));
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(time_str, muted_style));
    // Short ID is reference data — last, muted.
    spans.push(Span::raw("  "));
    spans.push(Span::styled(short_id.to_string(), muted_style));
    if archived {
        spans.push(Span::styled(" [archived]", muted_style));
    }
    lines.push(Line::from(spans));
}
```

Key differences from the previous loop:
- Title is `Bold White` when selected, default style otherwise (was: Gray/White and bold cyan id).
- Short ID moved to the end of the row, in DarkGray (was: prominent cyan-bold at the start).
- New brain column appears between cwd_suffix and time_str when `show_brain` is true.
- Pinned star ⭐ stays before the title.

- [ ] **Step 2: Run `cargo check` to confirm compile**

Run: `cargo check -p spur-tui`
Expected: Clean compile.

- [ ] **Step 3: Run the existing render-golden test (will fail because layout changed)**

Run: `cargo test -p spur-tui --test session_picker_render_snapshots populated_single_brain_no_filter 2>&1 | tail -30`
Expected: FAIL — the golden in Task 1 captured the old layout. Task 10 updates it.

This failure is expected and intentional. Don't fix it now; the snapshot update is consolidated in Task 10 along with multi-brain and other branches.

- [ ] **Step 4: Run all other picker tests**

Run: `cargo test -p spur-tui --test session_picker_interactions`
Expected: PASS — behavioral tests don't assert pixel layout.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): picker row layout — title-dominant, ID demoted, brain column

Reorder per-row spans so the session title leads (the actual recognition
handle) and the 8-char short_id ends the row in DarkGray (reference data,
not recognition). Add brain_name column when brains_are_heterogeneous —
hidden for the common single-brain setup.

Render-golden in session_picker_render_snapshots.rs is intentionally
broken by this commit and will be updated to the new layout in Task 10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Context-sensitive `footer_hint` function

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs:20-30` (replace `FOOTER_HINT` const + `render_footer_hint` fn) and the three render sites (`render_loading`, `render_error`, `render_populated`).

Replaces the static `FOOTER_HINT` const with a function that returns `&'static str` based on picker mode. Keeps render alloc-free.

- [ ] **Step 1: Replace the `FOOTER_HINT` const and `render_footer_hint` helper**

In `crates/spur-tui/src/views/session_picker.rs`, replace lines 20-30 (which currently contain `const FOOTER_HINT: &str = ...;` and `fn render_footer_hint(...)`) with:

```rust
fn footer_hint(
    state: &PickerState,
    rename_active: bool,
    confirm_active: bool,
) -> &'static str {
    if confirm_active {
        return "y/Enter confirm \u{00b7} n/Esc cancel";
    }
    if rename_active {
        return "type new title \u{00b7} Enter save \u{00b7} Esc cancel";
    }
    match state {
        PickerState::Loading => "Esc back",
        PickerState::Error { .. } => "r retry \u{00b7} Esc back",
        PickerState::Populated {
            search_focused: true,
            ..
        } => "type to filter \u{00b7} Enter commit \u{00b7} Esc exit search",
        PickerState::Populated { .. } => {
            "j/k nav \u{00b7} Enter resume \u{00b7} / search \u{00b7} n new \u{00b7} R rename \u{00b7} d archive \u{00b7} y yank-id \u{00b7} P preview \u{00b7} Esc back"
        }
    }
}

fn render_footer_hint(frame: &mut Frame, area: Rect, hint: &str) {
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
        area,
    );
}
```

Note: `render_footer_hint` now takes the hint string as a parameter rather than reading from a global const. The hint variant for the populated/list mode dropped `a show-archived` and `r refresh` from the visible hint to keep it under one terminal row at typical widths — those keys still work; they're now considered second-tier and discoverable via help.

- [ ] **Step 2: Update the three call sites to pass the hint**

In `render_loading` (around line 425), find the call to `render_footer_hint(frame, chunks[2])` and replace with:

```rust
render_footer_hint(frame, chunks[2], footer_hint(&self.state, false, false));
```

In `render_populated`, find the call to `render_footer_hint(frame, chunks[footer_idx])` (around line 727) and replace with:

```rust
let hint = footer_hint(
    &self.state,
    self.rename_state.is_some(),
    self.confirm_switch.is_some(),
);
render_footer_hint(frame, chunks[footer_idx], hint);
```

In `render_error` (around line 789), find the call to `render_footer_hint(frame, chunks[2])` and replace with:

```rust
render_footer_hint(frame, chunks[2], footer_hint(&self.state, false, false));
```

- [ ] **Step 3: Add a behavioral test for hint variants**

Add to `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
#[test]
fn footer_hint_changes_with_mode() {
    // We don't have a direct accessor for the hint string, but we can assert
    // mode-detection through the public is_* getters and infer correct hint
    // routing via Task 10's render goldens. This test pins the public mode-
    // detection getters used by footer_hint().
    let mut picker = SessionPickerView::new();
    picker.set_metadata(spur_tui::session_metadata::SessionMetadata::default());
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);

    assert!(!picker.is_rename_active());
    assert!(!picker.is_confirm_switch_visible());

    // Enter rename mode.
    let _ = picker.handle_key(key('R'), &test_ctx());
    assert!(picker.is_rename_active());

    // Cancel rename.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(!picker.is_rename_active());
}
```

- [ ] **Step 4: Run `cargo check` and the test**

Run: `cargo check -p spur-tui && cargo test -p spur-tui --test session_picker_interactions footer_hint_changes_with_mode`
Expected: Clean compile + PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): context-sensitive footer hint in picker

Replace the static FOOTER_HINT const with footer_hint(state, rename, confirm)
that returns a &'static str matching the keys that actually work in the
current mode:

  Loading        → "Esc back"
  Error          → "r retry · Esc back"
  Populated/list → "j/k nav · Enter resume · / search · n new · R rename · d archive · y yank-id · P preview · Esc back"
  Search-focused → "type to filter · Enter commit · Esc exit search"
  Rename mode    → "type new title · Enter save · Esc cancel"
  Confirm-switch → "y/Enter confirm · n/Esc cancel"

All variants are &'static str — no allocation in render hot path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: StatusBar hint alignment via `view_hint_override`

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs:71-95` (`StatusBarProps`)
- Modify: `crates/spur-tui/src/components/status_bar.rs:99-113` (`StatusBar::render` view-match)
- Modify: `crates/spur-tui/src/views/session_picker.rs` (three sites that call `StatusBar::render`)

Adds a single optional override field. When set, the StatusBar uses that hint instead of the hardcoded view default. The picker passes its `footer_hint(...)` result so the StatusBar and footer always show the same string.

- [ ] **Step 1: Extend `StatusBarProps` with `view_hint_override`**

In `crates/spur-tui/src/components/status_bar.rs`, add a new field to `StatusBarProps` (immediately after `flag_summary` at line 94):

```rust
    /// When `Some`, overrides the hardcoded per-view hint string.
    /// Used by `SessionPickerView` to keep the StatusBar hint in sync
    /// with `footer_hint(...)` for the current picker mode.
    pub view_hint_override: Option<&'a str>,
```

- [ ] **Step 2: Use the override in `StatusBar::render`**

In `crates/spur-tui/src/components/status_bar.rs`, replace the `let hints = match props.view { ... };` block (lines 99-113) with:

```rust
let hints = if let Some(s) = props.view_hint_override {
    s
} else {
    match props.view {
        ViewId::Dashboard => {
            " [i]nput [Enter]focus [r]eview [s]essions [Esc]back [Ctrl+C]quit [?]help"
        }
        ViewId::IssueBrowser => {
            " [j/k]navigate [Enter]detail [o/w/b/d]status [W]work [Esc]back [?]help"
        }
        ViewId::SessionDetail(_) => {
            hint_for_session_detail(props.stream_in_flight, props.esc_consumed_by_composer)
        }
        ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
        ViewId::PlanInspector(_) => " [Esc]back [Alt-p]close",
        #[cfg(feature = "markdown")]
        ViewId::MermaidOverlay(_) => " [Esc]close",
    }
};
```

The `SessionPicker` arm stays as a fallback for callers that don't pass `view_hint_override`, but the picker itself always sets the override (Step 4).

- [ ] **Step 3: Find every existing `StatusBarProps { ... }` literal and add `view_hint_override: None`**

Run: `grep -rn "StatusBarProps {" crates/spur-tui/src/`
For each match other than the picker's three `render_*` functions, add `view_hint_override: None,` to the struct literal.

- [ ] **Step 4: In the picker, pass `footer_hint(...)` as the override**

In `crates/spur-tui/src/views/session_picker.rs`, find the three `StatusBarProps { ... }` literals — one in `render_loading` (around line 408), one in `render_populated` (around line 706), one in `render_error` (around line 769). For each, add:

```rust
view_hint_override: Some(footer_hint(&self.state, /* rename */ false, /* confirm */ false)),
```

For `render_populated`, pass the actual mode flags:

```rust
view_hint_override: Some(footer_hint(
    &self.state,
    self.rename_state.is_some(),
    self.confirm_switch.is_some(),
)),
```

Note: in `render_populated`, the `StatusBar::render` is *only* called in the `else` branch (when neither rename nor confirm-switch banners are showing — see lines 705-727). In those cases, `rename_active` and `confirm_active` are both false. Pass `false, false` here. The `if let Some(ref target) = self.confirm_switch` and `if let Some(ref rs) = self.rename_state` arms above render the prompt manually and don't call StatusBar — they're unaffected.

- [ ] **Step 5: Run `cargo check` and the full test suite**

Run: `cargo check -p spur-tui && cargo test -p spur-tui 2>&1 | tail -20`
Expected: Clean compile. Existing render goldens for `status_bar_palette_badge.rs` should still pass; if any fail because they constructed `StatusBarProps` literally, fix them by adding `view_hint_override: None,`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/
git commit -m "$(cat <<'EOF'
feat(spur-tui): StatusBar view_hint_override; picker passes footer_hint

Adds StatusBarProps::view_hint_override (Option<&str>). When Some, the
StatusBar uses that hint instead of the hardcoded per-view default.

The session picker passes footer_hint(state, rename, confirm) in via the
override, eliminating the dual-source-of-truth between the StatusBar's
hardcoded "[↑↓]navigate [Enter]select [Esc]back" and the footer's full
keymap. Both lines now show the same mode-aware string.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `Action::CopySessionId` + App handler with OSC 52

**Files:**
- Modify: `crates/spur-tui/src/action.rs:13-137` (`Action` enum)
- Modify: `crates/spur-tui/src/app.rs:1279` (action match)

Adds the action variant and the App-side handler that emits the OSC 52 escape sequence. No new dependency — `base64` is already in the workspace lockfile.

- [ ] **Step 1: Confirm `base64` is in the spur-tui dependency tree**

Run: `cargo tree -p spur-tui --no-default-features 2>/dev/null | grep -E "^(│   )*[├└]── base64" | head -3`
If empty, run: `grep "^name = \"base64\"" Cargo.lock` — confirms it's at least transitively present.

If `base64` is not in `crates/spur-tui/Cargo.toml`'s `[dependencies]`, add it:

```toml
base64 = { workspace = true }
```

(Use the workspace dep style if `Cargo.toml` workspace section already declares `base64`. Otherwise add `base64 = "0.22"` matching the version in `Cargo.lock`.)

- [ ] **Step 2: Add the action variant**

In `crates/spur-tui/src/action.rs`, add to the `Action` enum (after `RefreshSessions` at line 73 is a natural place):

```rust
    /// Copy a session id to the system clipboard via OSC 52.
    /// Emitted by the picker's `y` keybind.
    CopySessionId(String),
```

- [ ] **Step 3: Add the App handler**

In `crates/spur-tui/src/app.rs`, add a new arm to the `match action { ... }` block (around line 1279). A natural place is alongside the other picker-emitted actions like `RefreshSessions` near line 1633:

```rust
            Action::CopySessionId(session_id) => {
                use base64::{engine::general_purpose::STANDARD, Engine};
                use std::io::Write;
                let payload = STANDARD.encode(session_id.as_bytes());
                // OSC 52 ; c ; <base64> ST — sets the system clipboard on
                // terminals that opt in (kitty, wezterm, alacritty, iterm2,
                // foot, ghostty). Silently ignored elsewhere.
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{payload}\x1b\\");
                let _ = out.flush();
                tracing::debug!(target: "spur_tui::picker", session_id = %session_id, "OSC 52 copy emitted");
            }
```

- [ ] **Step 4: Confirm `cargo check` is clean**

Run: `cargo check -p spur-tui`
Expected: Clean compile.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(spur-tui): Action::CopySessionId emits OSC 52 to system clipboard

Adds Action::CopySessionId(String). App handler base64-encodes the id and
writes the OSC 52 "set clipboard" escape (ESC ] 52 ; c ; <b64> ESC \) to
stdout. Modern terminals (kitty, wezterm, alacritty, iterm2, foot,
ghostty) consume the sequence; legacy terminals ignore the bytes —
graceful degradation, no error path needed.

No new crate dependency — base64 is already in the workspace via spur-acp.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `y` keybind in picker

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs:1002-1024` (list-mode key arms in `handle_key`)
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs` (test)

Adds the `y` arm to the picker's list-mode key handler. Emits `Action::CopySessionId(highlighted_id)`.

- [ ] **Step 1: Write failing test**

Add to `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
#[test]
fn y_emits_copy_session_id_for_highlighted_row() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata({
        let mut m = spur_tui::session_metadata::SessionMetadata::default();
        m.last_active_session_id = Some("a1".to_string());
        m
    });
    picker.set_sessions("t".into(), vec![session("a1", "alpha"), session("a2", "beta")]);
    // Cursor lands on a1 by P1.
    let action = picker.handle_key(key('y'), &test_ctx());
    match action {
        Some(Action::CopySessionId(id)) => assert_eq!(id, "a1"),
        other => panic!("expected CopySessionId(a1), got {other:?}"),
    }
}

#[test]
fn y_on_new_session_row_emits_nothing() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(spur_tui::session_metadata::SessionMetadata::default());
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    // Move cursor to [+ New] row (cursor=0).
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    assert_eq!(picker.cursor(), 0);
    // y on [+ New] is a no-op.
    let action = picker.handle_key(key('y'), &test_ctx());
    assert!(action.is_none(), "expected None on [+ New] row, got {action:?}");
}
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p spur-tui --test session_picker_interactions y_emits y_on_new`
Expected: FAIL — no `y` handler yet.

- [ ] **Step 3: Add the `y` arm in `handle_key`**

In `crates/spur-tui/src/views/session_picker.rs`, find the list-mode key arms inside the `else` branch of `handle_key` (around line 920-1024 — the arms after `if *search_focused { ... } else {`). Add a new arm after the `KeyCode::Char('R')` arm (line 1010) and before `_ => None`:

```rust
                            KeyCode::Char('y') => hl_session_id
                                .clone()
                                .map(Action::CopySessionId),
```

Important: do NOT add this arm inside the `search_focused` branch — when search is focused, `y` should type into the filter, not yank. The `if *search_focused` branch already routes all `KeyCode::Char(c)` to `filter.push(c)`, so this arm's placement in the list-mode branch is correct.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test session_picker_interactions y_emits y_on_new`
Expected: PASS — both tests.

- [ ] **Step 5: Run the full test suite to catch any regressions**

Run: `cargo test -p spur-tui --test session_picker_interactions`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): picker 'y' yanks highlighted session id via OSC 52

Press 'y' on a session row in the picker → Action::CopySessionId(id) →
App emits OSC 52 to set the system clipboard.

Press 'y' on the [+ New session] row is a no-op (no id to copy).
'y' inside the search box continues to type into the filter, unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Update render goldens to new layout

**Files:**
- Modify: `crates/spur-tui/tests/session_picker_render_snapshots.rs` (rewrite the existing golden + add 8 new ones)

The Task 1 golden was captured against the *old* layout. Tasks 5/6/7 changed it. This task updates the golden to the *new* layout and adds coverage for the other 8 render branches.

- [ ] **Step 1: Run the existing failing golden to capture the new actual output**

Run: `cargo test -p spur-tui --test session_picker_render_snapshots populated_single_brain_no_filter 2>&1 | head -60`
Expected: FAIL with row-mismatch output. Each `got:` line is the new layout.

- [ ] **Step 2: Update the inline `expected` array in `populated_single_brain_no_filter`**

Copy the actual rows from the failure output into the `expected` array. The test should now PASS.

Run: `cargo test -p spur-tui --test session_picker_render_snapshots populated_single_brain_no_filter`
Expected: PASS.

- [ ] **Step 3: Add 8 additional render goldens**

Append to `crates/spur-tui/tests/session_picker_render_snapshots.rs`:

```rust
#[test]
fn loading_state() {
    let mut picker = SessionPickerView::new();
    let expected: &[&str] = &[
        // capture-and-paste from first run failure
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn error_state() {
    let mut picker = SessionPickerView::new();
    picker.set_error("agent connection refused".into());
    let expected: &[&str] = &[
        // capture-and-paste from first run failure
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_multi_brain_no_filter() {
    let mut picker = SessionPickerView::new();
    let mut meta = SessionMetadata::default();
    meta.sessions.entry("a1".into()).or_default().brain_name = Some("claude".into());
    meta.sessions.entry("a2".into()).or_default().brain_name = Some("gpt-5".into());
    picker.set_metadata(meta);
    picker.set_sessions(
        "claude".into(),
        vec![session("a1xxxxxx", "Refactor auth", "/work/spur"), session("a2xxxxxx", "Tier 1 fixes", "/work/spur")],
    );
    let expected: &[&str] = &[
        // capture-and-paste — should include "claude" and "gpt-5" brain column entries
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "claude".into(),
        vec![session("a1xxxxxx", "alpha", "/tmp"), session("a2xxxxxx", "beta", "/tmp")],
    );
    // Focus search and type 'b'.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE), &ctx);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE), &ctx);
    let expected: &[&str] = &[
        // capture-and-paste — should show only beta row, footer hint = "type to filter · ..."
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_rename_active() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions("t".into(), vec![session("a1xxxxxx", "alpha", "/tmp")]);
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    // Cursor on a1 by P1; press R to enter rename mode.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE), &ctx);
    assert!(picker.is_rename_active());
    let expected: &[&str] = &[
        // capture-and-paste — rename prompt visible at status row, footer hint = "type new title · ..."
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_confirm_switch() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions(
        "t".into(),
        vec![session("a1xxxxxx", "alpha", "/tmp"), session("a2xxxxxx", "beta", "/tmp")],
    );
    picker.set_current_session_has_draft(Some("a1".into()));
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    // Move cursor to a2 and press Enter — opens confirm-switch banner.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx);
    assert!(picker.is_confirm_switch_visible());
    let expected: &[&str] = &[
        // capture-and-paste — confirm banner visible, footer hint = "y/Enter confirm · n/Esc cancel"
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_preview_visible() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(SessionMetadata::default());
    picker.set_sessions("t".into(), vec![session("a1xxxxxx", "alpha", "/tmp")]);
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &ctx);
    assert!(picker.is_preview_visible());
    let expected: &[&str] = &[
        // capture-and-paste — preview pane visible above footer
    ];
    assert_render(&mut picker, expected);
}

#[test]
fn populated_with_archived_shown() {
    let mut picker = SessionPickerView::new();
    let mut meta = SessionMetadata::default();
    meta.sessions.entry("a1".into()).or_default().archived = true;
    picker.set_metadata(meta);
    picker.set_sessions("t".into(), vec![session("a1xxxxxx", "alpha-archived", "/tmp")]);
    picker.toggle_show_archived();
    let expected: &[&str] = &[
        // capture-and-paste — header shows "[showing archived]", row has "[archived]" tag
    ];
    assert_render(&mut picker, expected);
}
```

- [ ] **Step 4: Run each new test, capture failure output, paste into the `expected` array**

For each new test, the workflow is:

1. Run the test with empty `expected: &[&str] = &[]`.
2. Read the `got:` lines from the failure output.
3. Paste them as string literals into the `expected` array.
4. Re-run to confirm PASS.

Run them one at a time:

```
cargo test -p spur-tui --test session_picker_render_snapshots loading_state 2>&1 | tail -50
cargo test -p spur-tui --test session_picker_render_snapshots error_state 2>&1 | tail -50
cargo test -p spur-tui --test session_picker_render_snapshots populated_multi_brain_no_filter 2>&1 | tail -50
# ...etc
```

- [ ] **Step 5: Run the full snapshot suite to confirm all pass**

Run: `cargo test -p spur-tui --test session_picker_render_snapshots`
Expected: All 9 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/tests/session_picker_render_snapshots.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): render goldens for picker — 9 branches against new layout

Inline-string goldens for:
  loading, error, populated single-brain, populated multi-brain,
  populated with filter, populated with rename active,
  populated with confirm-switch banner, populated with preview pane,
  populated with archived sessions visible.

Each golden is a Vec<String> of trimmed visible rows from an 80x24
TestBackend, asserted against an inline expected: &[&str]. Layout
changes show as plain string diffs in PRs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Final verification — full test suite + manual smoke

**Files:**
- None (verification only).

- [ ] **Step 1: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui 2>&1 | tail -30`
Expected: All tests pass. No warnings about unused code.

- [ ] **Step 2: Run `cargo clippy` for the crate**

Run: `cargo clippy -p spur-tui --all-targets -- -D warnings 2>&1 | tail -30`
Expected: Clean, no warnings. If warnings appear about the new code, fix them in this task before commit.

- [ ] **Step 3: Manual smoke (if a local agent setup is available)**

Run: `cargo run --bin spur -- --tui` (or whatever the existing TUI launch command is — check `crates/spur-tui/README.md` if unsure).

Verify:
- Open the picker (`Esc` from a session, or however the picker is reached). Cursor lands on the last-active session row, not on `[+ New]`.
- Press `j` to move down — cursor moves.
- Press `y` on a session row — verify the id is on your clipboard (paste in another window).
- Press `R` to rename — confirm the footer hint changes to `type new title · Enter save · Esc cancel`.
- Press `Esc` to cancel rename — footer returns to default list-mode hint.
- Pin a session with `p` — cursor stays on that session even though list reorders.
- The StatusBar hint matches the footer hint in every mode.

If any of these fails, file a follow-up bug; do not block the merge unless the failure indicates a regression in existing behavior.

- [ ] **Step 4: No commit needed for verification.**

---

## Spec Coverage

Cross-reference each spec section against this plan:

| Spec section | Plan task |
|--------------|-----------|
| Problem — UJ1 cursor default | Task 2 |
| Problem — UJ3 visual hierarchy | Task 5 |
| Problem — UJ7 brain disambiguation | Tasks 4, 5 |
| Problem — UJ8 hint surface | Tasks 6, 7 |
| Problem — projection bug (cursor by index) | Task 3 |
| Problem — clipboard affordance | Tasks 8, 9 |
| Projection rule P1 | Task 2 |
| Projection rule P2 | Tasks 2 (impl) + 3 (tests) |
| Row layout BEFORE/AFTER ASCII | Task 5 |
| Brains heterogeneous helper | Task 4 |
| `footer_hint` function with 5 variants | Task 6 |
| StatusBar hint alignment | Task 7 |
| `Action::CopySessionId` + OSC 52 | Tasks 8, 9 |
| Render goldens (9 branches) | Tasks 1, 10 |
| Updated existing tests | Task 2 (Step 5) |
| New behavioral tests (8 listed in spec) | Tasks 2, 3, 5, 6, 9 |

All spec sections covered. No gaps.
