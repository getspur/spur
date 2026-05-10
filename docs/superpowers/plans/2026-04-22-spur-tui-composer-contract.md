# SPUR TUI Composer Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make composer key ownership deterministic, preserve `ProtectedRange` semantics across Vim edits, and align status hints with effective runtime behavior in `spur-tui`.

**Architecture:** Keep `InputBar` as the editing engine and keep `SessionDetailView` / `DashboardView` as the routing layer, but move both views onto an explicit pre-key ownership contract. Fix semantic token preservation inside `InputBar` without introducing a new shared controller in this pass, then align hint text with the corrected ownership model.

**Tech Stack:** Rust 2021, `ratatui`, `crossterm`, `tui_textarea`, workspace tests via `cargo test`

---

## File Map

### Existing files to modify

- `crates/spur-tui/src/views/session_detail.rs`
  Purpose: Session-detail key routing, picker ownership, stream-cancel behavior, and session-detail unit tests.
- `crates/spur-tui/src/views/dashboard.rs`
  Purpose: Dashboard key routing and dashboard-specific action ownership.
- `crates/spur-tui/src/components/input_bar.rs`
  Purpose: Text editing, Vim mode editing, protected-range maintenance, and local unit tests.
- `crates/spur-tui/src/components/status_bar.rs`
  Purpose: Session-detail hint rendering and local hint tests.

### Existing test files to modify

- `crates/spur-tui/tests/picker_shell_ctrl_r.rs`
  Purpose: Integration tests for history picker ownership and InputBar restoration.
- `crates/spur-tui/tests/picker_shell_trigger_parity.rs`
  Purpose: Trigger-driven popup parity for slash and mention flows.
- `crates/spur-tui/tests/input_bar_protected_ranges.rs`
  Purpose: Protected-range invariants under direct editing.
- `crates/spur-tui/tests/input_bar_editing.rs`
  Purpose: Direct InputBar editing and history behavior.

### New test files

- `crates/spur-tui/tests/dashboard_composer_contract.rs`
  Purpose: Dashboard-level routing behavior and pre-key ownership regressions.

## Implementation Notes

- Do not introduce a shared `ComposerRouter` module in this change.
- Prefer a small local enum per view:

```rust
enum KeyOwner {
    Composer,
    Picker,
    View,
}
```

- Decide ownership from pre-key state only.
- Eliminate post-edit single-character reinterpretation branches such as
  `if self.input_bar.text().len() == 1 { ... clear(); ... }`.
- Any `InputBar` edit that currently calls `self.protected_ranges.clear()` must
  be replaced with targeted span bookkeeping or insertion rebasing.
- `Ctrl+C` and `Ctrl+K` remain globally owned by `App`; local plan work should
  align with that reality rather than trying to take those chords back.

---

### Task 1: Lock SessionDetail Routing to Pre-Key Ownership

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Test: `crates/spur-tui/tests/picker_shell_ctrl_r.rs`

- [ ] **Step 1: Write the failing session-detail routing tests**

Add unit tests near the existing `make_view()` / `test_ctx()` helpers in
`crates/spur-tui/src/views/session_detail.rs` that prove ownership is decided
before mutation:

```rust
#[test]
fn empty_emacs_j_scrolls_without_typing() {
    let mut v = make_view();
    let action = <SessionDetailView as crate::views::View>::handle_key(
        &mut v,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(matches!(action, Some(Action::ScrollDown)));
    assert_eq!(v.input_bar_text_for_test(), "");
}

#[test]
fn nonempty_emacs_j_stays_in_composer() {
    let mut v = make_view();
    v.input_bar_mut_for_test().set_text("x".into(), 1);

    let action = <SessionDetailView as crate::views::View>::handle_key(
        &mut v,
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(action.is_none());
    assert_eq!(v.input_bar_text_for_test(), "xj");
}

#[test]
fn nonempty_up_moves_composer_cursor_instead_of_scrolling_trace() {
    let mut v = make_view();
    v.input_bar_mut_for_test().set_text("hello\nworld".into(), 8);

    let action = <SessionDetailView as crate::views::View>::handle_key(
        &mut v,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(action.is_none());
    assert!(v.input_bar_mut_for_test().cursor() < 8);
}
```

- [ ] **Step 2: Run the focused session-detail test target and verify failure**

Run:

```bash
cargo test -p spur-tui empty_emacs_j_scrolls_without_typing -- --nocapture
```

Expected: FAIL because current routing mutates the composer first, then rescues
from a one-character buffer.

- [ ] **Step 3: Introduce a pre-key ownership helper in `SessionDetailView`**

Add a local ownership helper above `handle_key_inner` in
`crates/spur-tui/src/views/session_detail.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOwner {
    Composer,
    Picker,
    View,
}

fn session_detail_key_owner(&self, key: KeyEvent) -> KeyOwner {
    let was_empty = self.input_bar.is_empty();

    if let Some(shell) = self.picker_shell.as_ref() {
        use crate::components::query_source::QueryMode;
        let mode = shell.query_mode();
        if mode == QueryMode::OwnedByShell {
            return KeyOwner::Picker;
        }
        let picker_owns = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
        ) || (key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL));
        return if picker_owns {
            KeyOwner::Picker
        } else {
            KeyOwner::Composer
        };
    }

    if was_empty && self.input_bar.is_vim_normal() {
        if let KeyCode::Char(ch) = key.code {
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                return match ch {
                    'i' | 'a' | 'A' | 'I' | 'o' | 'O' => KeyOwner::Composer,
                    'j' | 'k' | 'g' | 'G' => KeyOwner::View,
                    _ => KeyOwner::View,
                };
            }
        }
    }

    let composer_key = matches!(
        key.code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Enter
            | KeyCode::Up
            | KeyCode::Down
    ) || (key.code == KeyCode::Esc && self.input_bar.wants_esc());

    if composer_key && !was_empty {
        KeyOwner::Composer
    } else if composer_key {
        match key.code {
            KeyCode::Up | KeyCode::Down => KeyOwner::View,
            _ => KeyOwner::Composer,
        }
    } else {
        KeyOwner::View
    }
}
```

- [ ] **Step 4: Rewire `handle_key_inner` to dispatch by owner and delete rescue logic**

Update the middle of `handle_key_inner` in
`crates/spur-tui/src/views/session_detail.rs` so the control flow matches the
ownership helper:

```rust
match self.session_detail_key_owner(key) {
    KeyOwner::Picker => {
        return self.handle_picker_key(key);
    }
    KeyOwner::Composer => {
        match self.input_bar.handle_key(key) {
            HandleOutcome::Submit(_, _) => {
                self.dispatch_intent(IntentEvent::Submitted);
                return self.route_submit_from_input_bar();
            }
            HandleOutcome::Key(intent) => {
                self.dispatch_intent(intent);
                return None;
            }
        }
    }
    KeyOwner::View => {
        return self.handle_trace_or_navigation_key(key);
    }
}
```

The helper names above are illustrative. Implement them inline inside
`handle_key_inner` or as private methods, but preserve this ownership split:
picker-only keys never reach history navigation, composer-owned keys never get
reinterpreted after mutation, and view-owned navigation runs only after the
composer has declined the key.

Delete the old post-edit reinterpretation block:

```rust
if self.input_bar.text().len() == 1 {
    let ch = self.input_bar.text().chars().next().unwrap();
    if matches!(ch, 'j' | 'k' | 'g' | 'G') {
        self.input_bar.clear();
        // ...
    }
}
```

- [ ] **Step 5: Make picker ownership block `Ctrl+P` / `Ctrl+N` bypass**

Move the `Ctrl+P` / `Ctrl+N` handling in `session_detail.rs` so it runs only
when no picker owns the key:

```rust
if self.picker_shell.is_none()
    && matches!(key.code, KeyCode::Char('p'))
    && key.modifiers.contains(KeyModifiers::CONTROL)
{
    self.input_bar.history_prev();
    self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
    return None;
}
```

Add the symmetric `Ctrl+N` case immediately after it.

- [ ] **Step 6: Extend integration coverage for active picker ownership**

Add a regression test to `crates/spur-tui/tests/picker_shell_ctrl_r.rs`:

```rust
#[test]
fn ctrl_p_does_not_mutate_hidden_input_bar_while_history_picker_is_open() {
    let mut v = mk_view();
    seed_history(
        &mut v,
        vec![InputHistoryEntry::new(InputStateSnapshot::from_text("older"))],
    );

    type_str(&mut v, "draft");
    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    let _ = press_mod(&mut v, KeyCode::Char('p'), KeyModifiers::CONTROL);

    assert_eq!(v.input_bar_text_for_test(), "draft");
}
```

- [ ] **Step 7: Run the session-detail and picker tests**

Run:

```bash
cargo test -p spur-tui empty_emacs_j_scrolls_without_typing -- --nocapture
cargo test -p spur-tui nonempty_emacs_j_stays_in_composer -- --nocapture
cargo test -p spur-tui nonempty_up_moves_composer_cursor_instead_of_scrolling_trace -- --nocapture
cargo test -p spur-tui --test picker_shell_ctrl_r -- --nocapture
cargo test -p spur-tui --test picker_shell_trigger_parity -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/tests/picker_shell_ctrl_r.rs \
        crates/spur-tui/tests/picker_shell_trigger_parity.rs
git commit -m "fix(spur-tui): C1 route session composer by pre-key ownership"
```

### Task 2: Apply the Same Ownership Contract to Dashboard

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Test: `crates/spur-tui/tests/review_submission.rs`
- Test: `crates/spur-tui/tests/delegation_status_rendering.rs`
- Create or Modify: `crates/spur-tui/tests/dashboard_composer_contract.rs`

- [ ] **Step 1: Write failing dashboard routing coverage**

Create `crates/spur-tui/tests/dashboard_composer_contract.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_core::lineage::projection::ExecutorLineage;
use spur_tui::views::dashboard::DashboardView;

fn press_char(dash: &mut DashboardView, ch: char) -> Option<spur_tui::action::Action> {
    let lineage = ExecutorLineage::new();
    let mut streams = spur_tui::worker_streams::WorkerStreams::default();
    dash.handle_key_with_worker_streams(
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        &lineage,
        &mut streams,
    )
}

#[test]
fn empty_dashboard_j_routes_to_view_action() {
    let mut dash = DashboardView::new();
    let action = press_char(&mut dash, 'j');
    assert!(matches!(action, Some(spur_tui::action::Action::ScrollDown) | Some(spur_tui::action::Action::SelectNext)));
}

#[test]
fn nonempty_dashboard_j_stays_in_input_bar() {
    let mut dash = DashboardView::new();
    dash.input_bar_mut_for_test().set_text("x".into(), 1);

    let action = press_char(&mut dash, 'j');

    assert!(action.is_none());
    assert_eq!(dash.input_bar_text_for_test(), "xj");
}
```

- [ ] **Step 2: Run the new dashboard contract test and verify failure**

Run:

```bash
cargo test -p spur-tui --test dashboard_composer_contract -- --nocapture
```

Expected: FAIL because current dashboard routing still uses post-edit
one-character reinterpretation.

- [ ] **Step 3: Add local ownership logic to `DashboardView`**

Mirror the session-detail approach in `crates/spur-tui/src/views/dashboard.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOwner {
    Composer,
    View,
}

fn dashboard_key_owner(&self, key: KeyEvent) -> KeyOwner {
    let was_empty = self.input_bar.is_empty();

    if was_empty && self.input_bar.is_vim_normal() {
        if let KeyCode::Char(ch) = key.code {
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                return match ch {
                    'i' | 'a' | 'A' | 'I' | 'o' | 'O' => KeyOwner::Composer,
                    _ => KeyOwner::View,
                };
            }
        }
    }

    let composer_key = matches!(
        key.code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Enter
            | KeyCode::Up
            | KeyCode::Down
    ) || (key.code == KeyCode::Esc && self.input_bar.wants_esc());

    if composer_key && !was_empty {
        KeyOwner::Composer
    } else if composer_key {
        match key.code {
            KeyCode::Up | KeyCode::Down => KeyOwner::View,
            _ => KeyOwner::Composer,
        }
    } else {
        KeyOwner::View
    }
}
```

- [ ] **Step 4: Remove dashboard post-edit rescue logic**

Delete the two post-edit reinterpretation blocks in
`crates/spur-tui/src/views/dashboard.rs`:

```rust
if self.input_bar.text().len() == 1
    && self.focused_node.is_some()
    && self.detail_pane.current_tab == DetailTab::Review
{
    // ...
}

if self.input_bar.text().len() == 1 {
    let ch = self.input_bar.text().chars().next().unwrap();
    match ch {
        // ...
    }
}
```

Replace them with a pre-key `KeyOwner` split plus a separate view-owned review
decision branch that runs before composer dispatch:

```rust
if self.input_bar.is_empty()
    && self.focused_node.is_some()
    && self.detail_pane.current_tab == DetailTab::Review
{
    if let KeyCode::Char(ch) = key.code {
        if let Some(decision) = crate::components::review_card::decision_for_key(ch, None) {
            if let Some(id) = self.focused_node.clone() {
                let attempt_n = lineage
                    .and_then(|l| l.node(&id))
                    .and_then(|n| n.pending_review.as_ref().map(|r| r.attempt_n))
                    .unwrap_or(1);
                return Some(Action::SubmitReview {
                    executor_id: id.0,
                    attempt_n,
                    decision,
                });
            }
        }
    }
}
```

- [ ] **Step 5: Ensure non-empty `Up` / `Down` reach `InputBar`**

In the dashboard editing-key match, include `KeyCode::Up` and `KeyCode::Down`
in composer-owned keys so multiline drafts move the cursor instead of always
scrolling the surrounding panes:

```rust
let is_editing_key = matches!(
    key.code,
    KeyCode::Char(_)
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::Enter
        | KeyCode::Up
        | KeyCode::Down
) || (key.code == KeyCode::Esc && self.input_bar.wants_esc());
```

- [ ] **Step 6: Add dashboard test helpers under `cfg(test)`**

Add these under `#[cfg(any(test, debug_assertions))]` in
`crates/spur-tui/src/views/dashboard.rs`:

```rust
#[cfg(any(test, debug_assertions))]
pub fn input_bar_mut_for_test(&mut self) -> &mut crate::components::input_bar::InputBar {
    &mut self.input_bar
}

#[cfg(any(test, debug_assertions))]
pub fn input_bar_text_for_test(&self) -> String {
    self.input_bar.text()
}
```

- [ ] **Step 7: Run dashboard routing tests**

Run:

```bash
cargo test -p spur-tui --test dashboard_composer_contract -- --nocapture
cargo test -p spur-tui dashboard_reads_attempt_n_from_lineage_on_submit -- --nocapture
cargo test -p spur-tui --test delegation_status_rendering -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
git add crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/tests/dashboard_composer_contract.rs \
        crates/spur-tui/tests/review_submission.rs
git commit -m "fix(spur-tui): C2 route dashboard composer by pre-key ownership"
```

### Task 3: Preserve ProtectedRange Semantics Across Vim Edits

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`
- Modify: `crates/spur-tui/tests/input_bar_protected_ranges.rs`
- Modify: `crates/spur-tui/tests/input_bar_editing.rs`

- [ ] **Step 1: Write failing protected-range regression tests for Vim edits**

Extend `crates/spur-tui/tests/input_bar_protected_ranges.rs` with explicit Vim
edit regressions:

```rust
use spur_tui::components::input_bar::{EditMode, VimMode};

#[test]
fn vim_d_preserves_atom_outside_deleted_span() {
    let mut b = InputBar::new();
    b.set_mode(EditMode::Vim(VimMode::Normal));
    type_str(&mut b, "abc ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " tail");

    b.set_text_cursor_for_test(0);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));

    assert!(b.text().contains("@a.rs"));
    assert_eq!(b.protected_ranges().len(), 1);
}

#[test]
fn vim_p_rebases_existing_atom_instead_of_clearing_all_ranges() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " tail");
    b.set_mode(EditMode::Vim(VimMode::Normal));

    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));

    assert!(!b.protected_ranges().is_empty());
}
```

- [ ] **Step 2: Run the focused protected-range target and verify failure**

Run:

```bash
cargo test -p spur-tui vim_d_preserves_atom_outside_deleted_span -- --nocapture
```

Expected: FAIL because the current Vim edit path clears all
`protected_ranges`.

- [ ] **Step 3: Add targeted helpers in `InputBar` for coarse `tui_textarea` edits**

In `crates/spur-tui/src/components/input_bar.rs`, add helpers that explicitly
rebase or delete by span:

```rust
fn replace_full_text_preserving_ranges<F>(&mut self, mutate: F)
where
    F: FnOnce(&str, usize) -> (String, usize, Vec<(usize, usize)>),
{
    let snapshot = self.snapshot();
    let cursor = self.cursor_to_byte();
    let (new_text, new_cursor, deleted_spans) = mutate(&snapshot.text, cursor);

    let mut new_ranges = snapshot.protected_ranges.clone();
    for (start, end) in deleted_spans {
        new_ranges.retain(|r| r.end <= start || r.start >= end);
        let deleted = end - start;
        for r in &mut new_ranges {
            if r.start >= end {
                r.start -= deleted;
                r.end -= deleted;
            }
        }
    }

    self.restore_snapshot(
        &InputStateSnapshot::new(new_text, new_ranges),
        new_cursor,
    );
}
```

Do not keep this exact helper if a narrower span-based helper yields cleaner
code, but preserve the same semantics.

- [ ] **Step 4: Replace blanket clears in Vim destructive paths**

Update the destructive branches in `handle_vim_normal_input` and
`vim_complete_operator` so they preserve unaffected ranges.

For example, replace:

```rust
self.textarea.delete_line_by_end();
self.rebuild_line_cache();
self.protected_ranges.clear();
```

with logic shaped like:

```rust
let start = self.cursor_to_byte();
let text = self.text();
let end = text[start..]
    .find('\n')
    .map(|i| start + i)
    .unwrap_or(text.len());
self.delete_span(start, end);
```

For visual selections, compute the selected absolute byte span before cutting
and then apply `delete_span(start, end)` semantics to the metadata.

- [ ] **Step 5: Replace Vim `p` blanket-clear behavior with insertion rebasing**

Where current code does:

```rust
self.textarea.paste();
self.rebuild_line_cache();
self.protected_ranges.clear();
```

capture the pre-paste cursor, perform the paste, compute the inserted byte
delta, and shift later ranges without deleting unaffected ones:

```rust
let before = self.cursor_to_byte();
self.textarea.paste();
self.rebuild_line_cache();
let after = self.cursor_to_byte();
let delta = after as isize - before as isize;
if delta != 0 {
    self.shift_ranges(before, delta);
}
```

If the paste operation can overwrite a selection, adjust the implementation so
the removed span is applied through the same targeted delete bookkeeping first.

- [ ] **Step 6: Run the InputBar regression suite**

Run:

```bash
cargo test -p spur-tui --test input_bar_protected_ranges -- --nocapture
cargo test -p spur-tui --test input_bar_editing -- --nocapture
cargo test -p spur-tui input_bar -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

Run:

```bash
git add crates/spur-tui/src/components/input_bar.rs \
        crates/spur-tui/tests/input_bar_protected_ranges.rs \
        crates/spur-tui/tests/input_bar_editing.rs
git commit -m "fix(spur-tui): C3 preserve protected ranges across vim edits"
```

### Task 4: Align Status Hints and Dead-Chord Semantics

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/picker_shell.rs`
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Write failing hint truthfulness tests**

Replace the current bool-only hint tests in
`crates/spur-tui/src/components/status_bar.rs` with mode-aware cases:

```rust
#[test]
fn hint_shows_back_when_streaming_but_esc_is_consumed_by_vim_mode() {
    let hint = hint_for_session_detail(true, true);
    assert!(hint.contains("[Esc]back"), "got: {hint}");
    assert!(!hint.contains("[Esc]stop"));
}

#[test]
fn hint_shows_stop_when_streaming_and_esc_can_cancel() {
    let hint = hint_for_session_detail(true, false);
    assert!(hint.contains("[Esc]stop"), "got: {hint}");
}
```

- [ ] **Step 2: Run the focused status-bar target and verify failure**

Run:

```bash
cargo test -p spur-tui hint_shows_back_when_streaming_but_esc_is_consumed_by_vim_mode -- --nocapture
```

Expected: FAIL because `hint_for_session_detail` currently only takes
`stream_in_flight: bool`.

- [ ] **Step 3: Make `hint_for_session_detail` mode-aware**

Change `crates/spur-tui/src/components/status_bar.rs` from:

```rust
pub(crate) fn hint_for_session_detail(stream_in_flight: bool) -> &'static str
```

to:

```rust
pub(crate) fn hint_for_session_detail(
    stream_in_flight: bool,
    esc_consumed_by_composer: bool,
) -> &'static str
```

Implement it like:

```rust
pub(crate) fn hint_for_session_detail(
    stream_in_flight: bool,
    esc_consumed_by_composer: bool,
) -> &'static str {
    if stream_in_flight && !esc_consumed_by_composer {
        " [Enter]send [Esc]stop [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-g]toggle [Ctrl-r]history [?]help"
    } else {
        " [Enter]send [Esc]back [j/k]scroll [Alt-m]plan [Alt-d]panel [Alt-g]toggle [Ctrl-r]history [?]help"
    }
}
```

- [ ] **Step 4: Thread `wants_esc()` into session-detail status-bar rendering**

Update the `StatusBarProps` call site in `crates/spur-tui/src/views/session_detail.rs`:

```rust
StatusBar::render(
    frame,
    chunks[4],
    StatusBarProps {
        view: &ViewId::SessionDetail(self.session_id.clone()),
        stream_in_flight: self.stream_in_flight && !self.cancelling_in_flight,
        esc_consumed_by_composer: self.input_bar.wants_esc(),
        // ...
    },
);
```

Add the new field to `StatusBarProps` in `status_bar.rs`:

```rust
pub esc_consumed_by_composer: bool,
```

and pass it through the session-detail branch:

```rust
ViewId::SessionDetail(_) => hint_for_session_detail(
    props.stream_in_flight,
    props.esc_consumed_by_composer,
),
```

- [ ] **Step 5: Remove unreachable local `Ctrl+C` paths and leave `Ctrl+K` global**

Delete the unreachable local `Ctrl+C` handlers that contradict `App`
ownership:

```rust
KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    PickerAction::Cancel
}
```

and:

```rust
KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    self.set_mode(EditMode::Vim(VimMode::Normal));
    return HandleOutcome::Key(IntentEvent::NoOp);
}
```

Do not add local fallback logic for `Ctrl+K`; `App` remains the only owner of
that chord.

- [ ] **Step 6: Run the hint and cancel tests**

Run:

```bash
cargo test -p spur-tui hint_shows_stop_when_streaming_and_esc_can_cancel -- --nocapture
cargo test -p spur-tui hint_shows_back_when_streaming_but_esc_is_consumed_by_vim_mode -- --nocapture
cargo test -p spur-tui esc_with_stream_in_flight -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

Run:

```bash
git add crates/spur-tui/src/components/status_bar.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/components/picker_shell.rs \
        crates/spur-tui/src/components/input_bar.rs
git commit -m "fix(spur-tui): C4 align composer hints with effective esc behavior"
```

### Task 5: Run the Full Verification Sweep

**Files:**
- Modify: none
- Test: workspace and crate-level verification only

- [ ] **Step 1: Run focused crate tests**

Run:

```bash
cargo test -p spur-tui --test input_bar_protected_ranges -- --nocapture
cargo test -p spur-tui --test input_bar_editing -- --nocapture
cargo test -p spur-tui --test picker_shell_ctrl_r -- --nocapture
cargo test -p spur-tui --test picker_shell_trigger_parity -- --nocapture
cargo test -p spur-tui --test dashboard_composer_contract -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run the broader `spur-tui` suite**

Run:

```bash
cargo test -p spur-tui -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Run formatting and lint checks for touched crate**

Run:

```bash
cargo fmt --all --check
cargo clippy -p spur-tui -- -D warnings
```

Expected: both commands succeed with no diffs and no warnings.

- [ ] **Step 4: Final commit if verification required a follow-up fix**

If any verification-only fix was required, run:

```bash
git add crates/spur-tui
git commit -m "chore(spur-tui): C5 finish composer contract verification"
```

If no fix was required, skip this commit.

---

## Spec Coverage Check

- Pre-key ownership contract: covered by Tasks 1 and 2.
- Empty-bar navigation and multiline cursor movement: covered by Tasks 1 and 2.
- Picker ownership and history bypass: covered by Task 1.
- `ProtectedRange` preservation: covered by Task 3.
- Dead-chord and truthful hint alignment: covered by Task 4.
- Unit and journey coverage: covered across Tasks 1 through 5.

## Placeholder Scan

- No `TBD`, `TODO`, or “similar to above” references remain.
- Each task names exact files and concrete commands.
- Each code-changing step includes code blocks showing the intended shape.

## Type Consistency Check

- `KeyOwner` naming is consistent across the routing tasks.
- `hint_for_session_detail(stream_in_flight, esc_consumed_by_composer)` is used
  consistently in the status-bar task.
- `input_bar_mut_for_test()` / `input_bar_text_for_test()` names match the
  existing `SessionDetailView` helper pattern.
