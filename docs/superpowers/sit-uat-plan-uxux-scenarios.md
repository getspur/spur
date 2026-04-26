# SIT/UAT UX Scenarios

## 1. Existing harness primitives (reuse)

The `crates/spur-tui/tests/` directory already contains a rich set of primitives that new journey-style tests can build on without inventing new infrastructure.

### 1.1 `TestBackend`-driven `Terminal`

Every rendering assertion uses the standard ratatui pattern:

```rust
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
term.draw(|f| view.render(f, area, &ctx)).unwrap();
let buf = term.backend().buffer().clone();
```

This is exercised in:
- `detail_pane_scroll.rs` — scroll badge assertions via `buffer_contains(buf, needle)`.
- `session_picker_render_snapshots.rs` — full golden-row comparison via `buffer_to_lines(buf)`.
- `status_bar_flag_summary.rs` — substring assertions on the flattened buffer text.
- `render_golden.rs` — golden-file re-recording with `UPDATE_GOLDEN=1`.

**Reusable helpers**

| Pattern | Source file | What it does |
|---------|-------------|--------------|
| `buffer_contains(buf, "▼ following")` | `detail_pane_scroll.rs:57` | Scans every cell for a substring. Good for footer badges. |
| `buffer_to_lines(buf)` → `Vec<String>` | `session_picker_render_snapshots.rs:13` | Converts buffer to per-row strings for exact golden matching. |
| `row_text(buf, y, width)` | `render.rs` (inline) | Extracts a single row as a string. |
| `assert_no_vertical_border_glyphs(buf, w, h)` | `render.rs:760` | Scans entire rectangle for `│` glyphs. |

### 1.2 Synthetic event injection

**View-level (no `App`)**
- `dashboard.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &test_ctx())` — used heavily in `dashboard_composer_contract.rs`.
- `dashboard.handle_paste("hello")` — direct paste injection.
- `input_bar.handle_key(...)` → returns `HandleOutcome::Key(IntentEvent)` or `HandleOutcome::Submit(text, interrupt)`.

**App-level (full component graph)**
- `spur_tui::test_support::new_app()` → constructs an `App` with no user-input channel.
- `app.handle_crossterm_event(Event::Paste(text))` — routes paste through the real view-switching logic (`App::handle_paste` dispatches to `DashboardView`, `SessionDetailView`, or `SessionPickerView`).
- `app.handle_crossterm_event_for_test(key_event)` — shortcut for key events.
- `app.last_action_for_test()` → inspects the action that would have been sent to the ACP backend.

**Context fixtures**
- `spur_tui::test_support::test_view_ctx(&LINEAGE)` → `ViewContext` with idle brain status and empty plan projection.
- `spur_tui::test_support::push_event(app, ev)` → injects a `SpurEvent` exactly as the runtime loop would.

### 1.3 State poking / inspection hooks

| Hook | Location | Purpose |
|------|----------|---------|
| `dashboard.input_bar_text_for_test()` | `dashboard.rs` | Read composer text after key routing. |
| `dashboard.input_bar_mut_for_test()` | `dashboard.rs` | Read cursor, protected ranges, etc. |
| `input_bar.take_submit_capture()` | `input_bar.rs` | Returns `(text, ranges, interrupt)` after `submit()`. |
| `input_bar.protected_ranges()` | `input_bar.rs` | Inspect atom / paste-ref byte ranges. |
| `detail_pane.scroll_up()` / `scroll_down()` | `detail_pane.rs` | Manipulate scroll offset directly. |
| `detail_pane.is_following()` | `detail_pane.rs` | Assert anchor state. |

---

## 2. UAT scenarios (per feature)

Each scenario is phrased as a user goal, decomposable into a sequence of synthetic events, and verifiable via `TestBackend` buffer state and/or emitted `Action`s.

### Feature 1: Copy-friendly borders

**F1-U1 — "I want to copy multi-line trace output without grabbing border glyphs"**

*Events:*
1. Seed `DashboardView` with a focused node and a `ReactTrace` containing 5 lines of text.
2. Render the dashboard into an `80×24` `TestBackend`.

*Assertions:*
- Scan the full buffer rectangle for `│`, `║`, `█` — none must be present.
- This is essentially the existing `copy_friendly_border_tests` lifted to the `DashboardView` render path (which adds the `ActivityLog` and `InputBar` around the trace).

*Reuse:* `assert_no_vertical_border_glyphs` from `render.rs`.

**F1-U2 — "I want to see where I am in a long trace without a scrollbar widget"**

*Events:*
1. Build an `ExecutorNode` + `ReactTrace` with 40 lines.
2. Render `DetailPane` at `80×8` (forces overflow).
3. Scroll down 5 lines (`pane.scroll_down_by(5)`).

*Assertions:*
- Bottom row contains `· 8/40 · 20% ` (or similar, depending on wrapped height).
- Buffer must NOT contain any `█` (scrollbar thumb glyph).
- The trace block borders are `Borders::TOP | Borders::BOTTOM` only.

**F1-U3 — "I want to know that short content looks clean (no position indicator)"**

*Events:*
1. Render a `DetailPane` with a `ReactTrace` containing 2 lines in an `80×12` area.

*Assertions:*
- Bottom row does NOT contain `%`.
- Borders are still `TOP | BOTTOM` only.

**F1-U4 — "I want my Vim Visual mode to be visible in the mode badge"**

*Events:*
1. Create `DashboardView`, set edit mode to `EditMode::Vim(VimMode::Visual)`.
2. Render dashboard.

*Assertions:*
- Buffer contains a mode badge with a glyph prefix (e.g., `[VISUAL]` or similar indicator derived from `panel_context_hint`).
- The badge sits on the hint line above the input bar, not inside the trace pane.

---

### Feature 2: Scroll Option B

**F2-U1 — "I want to see exact position when I scroll to the top"**

*Events:*
1. Seed `ReactTrace` with 27 lines.
2. Render at width 70, visible height 5.
3. Scroll offset = 0.

*Assertions:*
- `position_indicator` returns `Some(" · 5/27 · 18% ")`.
- Bottom border of the trace block contains this string right-aligned.

**F2-U2 — "I want to see updated position when I scroll to the middle"**

*Events:*
1. Same trace as F2-U1.
2. Scroll down to offset 7 (so bottom-of-viewport is line 12).

*Assertions:*
- Indicator reads `· 12/27 · 44%`.

**F2-U3 — "I want to see 100% when I reach the bottom"**

*Events:*
1. Same trace.
2. Scroll down until `is_following()` is true (or offset = 22).

*Assertions:*
- Indicator reads `· 27/27 · 100%`.

**F2-U4 — "I want the indicator to degrade gracefully on narrow terminals"**

*Events:*
1. Same trace, render width = 25 (between 20 and 30).
2. Scroll to any offset.

*Assertions:*
- Indicator shows `· 55%` (percentage only, no line count).
- At width < 20, indicator is `None` entirely.

---

### Feature 3: Paste-as-atom

**F3-U1 — "I want to paste a code block and see it atomized in the input bar"**

*Events:*
1. `dashboard.handle_paste("fn main() {\n    println!(\"hello\");\n}")`.

*Assertions:*
- `dashboard.input_bar_text_for_test()` == `"[Paste #1 · 3 lines]"`.
- `input_bar.protected_ranges()` has exactly one `RangeKind::PasteRef(1)`.

**F3-U2 — "I want to submit the atomized paste and have the full text sent"**

*Events:*
1. Paste 16 lines → atom placeholder appears.
2. Press `Enter` (or call `input_bar.submit()`).

*Assertions:*
- `take_submit_capture()` returns the original 16-line string, zero protected ranges, and `interrupt = false`.
- The action emitted (at `App` level) is `Action::SendMessage { text: "<full 16 lines>", .. }`.

**F3-U3 — "I want an interrupting paste to propagate the `!` signal after expansion"**

*Events:*
1. Paste `"!stop\nplease halt"`.
2. Submit.

*Assertions:*
- `take_submit_capture()` returns `interrupt = true`.
- Expanded text starts with `!`.

**F3-U4 — "I want my 51st multi-line paste to evict the oldest one"**

*Events:*
1. Loop 55 times: paste `"line\nline"`, submit, repeat.
2. Inspect `input_bar.pastes` (or add a test hook if private).

*Assertions:*
- Store length == 50 (`PASTE_STORE_CAP`).
- Keys 1–5 are absent; keys 51–55 are present.

*Note:* This is already a unit test (`paste_store_caps_oldest_entries_evicted`). The UAT/SIT value is exercising it through `DashboardView::handle_paste` → `InputBar::insert_paste` rather than direct `InputBar` mutation.

**F3-U5 — "I want pasting while browsing history to restore my draft first"**

*Events:*
1. Type `"new draft"`, submit `"old message"`.
2. `history_prev()` to recall `"old message"`.
3. Paste `"interrupting\npaste"`.

*Assertions:*
- Final text contains `"new draft"` AND `"[Paste #2 · 2 lines]"` (draft restored, then paste appended).
- History browse index is reset.

**F3-U6 — "I want backspacing over a placeholder to remove the whole atom, not character-by-character"**

*Events:*
1. Paste multi-line text.
2. Press `Backspace` once.

*Assertions:*
- `input_bar_text_for_test()` is empty (or predecessor text unchanged).
- `protected_ranges()` is empty.

---

## 3. SIT/UAT layering

| Scenario | Layer | Why |
|----------|-------|-----|
| F1-U1 (no border glyphs) | **SIT** | Asserts on `ReactTrace::render` + `DashboardView::render_with_lineage` integration; no real terminal or ACP backend needed. |
| F1-U2 (position indicator on overflow) | **SIT** | Integrates `ReactTrace` scroll math with `DetailPane` tab focus + `DashboardView` layout computation. |
| F1-U4 (Vim Visual badge) | **UAT** | Black-box: user sets mode, renders full dashboard, reads buffer. Mode is a visual contract, not a component wire. |
| F2-U1..U3 (scroll position values) | **SIT** | Unit-level math is already tested (`position_indicator_table`). SIT value is verifying the string appears in the rendered block title after `DetailPane` → `ReactTrace` integration. |
| F2-U4 (narrow degradation) | **SIT** | Tests the width-branching logic inside `position_indicator` as exercised through the real render path. |
| F3-U1 (atomized placeholder) | **UAT** | `DashboardView::handle_paste` is the public user-facing entry point; asserting on `InputBar` internals is SIT, but the user goal is verified at the dashboard boundary. |
| F3-U2 (submit expands) | **UAT** | End-to-end through `handle_key(Enter)` → `HandleOutcome::Submit` → `Action::SendMessage`. The ACP backend is stubbed by `App::new()` (no `user_input_tx`). |
| F3-U3 (interrupt propagation) | **SIT** | Verifies the `!` prefix is recomputed on *expanded* text, not placeholder text. This is a component integration concern. |
| F3-U4 (cap + eviction) | **SIT** | Side-store (`BTreeMap`) behavior is an internal invariant; SIT confirms it holds across multiple `handle_paste` calls. |
| F3-U5 (history browse + paste) | **UAT** | User journey: browse history → paste → expect draft restoration. This crosses `InputBar` + `InputHistory` integration. |
| F3-U6 (atomic backspace) | **UAT** | User presses Backspace once; expects whole placeholder gone. Black-box from the key event. |

**General rule of thumb**
- **SIT** = we care that component A (e.g., `DetailPane`) wired correctly to component B (e.g., `ReactTrace`), often with direct state poking (`scroll_up()`, `set_edit_mode()`).
- **UAT** = we care that a synthetic user action (paste, key, submit) produces the right visible outcome or the right `Action` emission, without peering into intermediate state.

---

## 4. Coverage gaps not covered by existing unit tests

The following behaviors are either **not tested at all** or tested only in isolation and need integration-level reinforcement:

### 4.1 Paste-during-Vim-Normal-mode

Kimi review noted that pasting while in Vim Normal mode is inconsistent with vim conventions (pasting should probably enter Insert mode first). Today `dashboard.handle_paste` unconditionally switches to `DashboardMode::Compose` and calls `input_bar.insert_paste`. There is no test that asserts what happens when the user is in `VimMode::Normal` and receives a paste event.

*Suggested test:* `App::handle_crossterm_event(Event::Paste)` while `EditMode::Vim(VimMode::Normal)` is active. Assert that mode transitions to Compose and the paste appears atomized.

### 4.2 Slash-command interaction with paste atom

Kimi review noted: a leading `/` followed by a paste expansion can break command resolution because the expanded text may contain newlines or spaces that the command parser does not expect. The `render_input_hint` already branches on `text.starts_with('/')`, but there is no integration test that pastes `"/explain\nthis code"` and then submits.

*Suggested test:* Paste `/explain\nthis code`, submit, assert that the captured text is the full expanded string and that the `Action` emitted is `SendMessage` (not a malformed command error).

### 4.3 Protected-range styling rendering in the input bar

The `[Paste #N · M lines]` placeholder is a `RangeKind::PasteRef`. The input bar applies special styling to protected ranges (cyan background for atoms, possibly different for paste refs). There is **no rendering test** that asserts the placeholder text actually appears with distinct styling in a `TestBackend` buffer.

*Suggested test:* Render `InputBar` (or full `DashboardView`) containing a paste placeholder, then assert that the buffer cells spanning the placeholder text have a background style different from default.

### 4.4 Full App-level paste → submit → action journey

All existing paste tests (`paste_atom_tests` in `input_bar.rs`) mutate `InputBar` directly. There is no test that exercises:

```rust
app.handle_crossterm_event(Event::Paste("multi\nline"));
app.handle_crossterm_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
```

and asserts on `app.last_action_for_test()`.

*Suggested test:* End-to-end through `App` event dispatch, view routing, and action emission.

### 4.5 Position indicator interaction with tab cycling

When the user cycles away from Stream and back, `DetailPane` snaps to `Following`. There is no test that asserts the position indicator re-appears (or disappears) correctly after tab cycling in the dashboard context.

*Suggested test:* Focus a node, scroll up so indicator is mid-range, cycle tabs `Stream → Task → Stream`, assert indicator shows bottom/100% again.

### 4.6 Draft restoration with paste atom during history browse

`F3-U5` is unit-tested inside `input_bar.rs` but not at the `DashboardView` or `App` level. The `App` has additional logic around `force_flush_active_draft` and `SaveDraft` actions that could interfere.

*Suggested test:* `App`-level: type draft, submit, type new draft, browse history, paste, assert draft restored and no `SaveDraft` action is emitted with stale state.

### 4.7 Mixed atoms + pastes expansion ordering with mentions

The unit test `submit_preserves_non_paste_ranges_with_adjusted_offsets` covers one atom + one paste. There is no test for:
- Multiple mentions (`@foo.rs`) interleaved with multiple paste placeholders.
- Submit expansion when protected ranges are out of order (should never happen, but defensive).

*Suggested test:* `InputBar` with text `"check @foo.rs then [Paste#1] and @bar.rs then [Paste#2]"`; submit and assert captured text, ranges, and offsets are correct.

---

## 5. Recommended ordering (which scenarios first)

Build confidence from the inside out: prove the smallest integration works, then layer up to the full `App` graph.

| Order | Scenario | Rationale |
|-------|----------|-----------|
| 1 | **F3-U1** + **F3-U6** (paste atomization + atomic backspace) | These exercise `DashboardView::handle_paste` and `InputBar::handle_key` — the most recently shipped code. Fast feedback. |
| 2 | **F3-U2** + **F3-U3** (submit expansion + interrupt) | Validates the submit path, which is the riskiest part of paste-as-atom (data loss if expansion fails). |
| 3 | **F3-U5** (history browse + paste) | Higher complexity; depends on #1 and #2 passing. |
| 4 | **F1-U1** + **F1-U2** (copy-friendly borders + position indicator) | Rendering assertions are stable once the above logic is correct. |
| 5 | **F2-U1**..**F2-U4** (scroll Option B table) | Already covered by unit tests; SIT layer is quick to add once #4 is in place. |
| 6 | **F3-U4** (cap + eviction) | Stress test; run after basic paste flow is solid. |
| 7 | **Gap 4.3** (protected-range styling render) | Requires reading cell styles from `TestBackend`, which is slightly more verbose. |
| 8 | **Gap 4.4** (full App-level journey) | The ultimate integration gate. Run last because it depends on all lower layers. |
| 9 | **Gap 4.2** (slash-command + paste) | Behavioral question: should we even allow this? If the test fails, it may drive a product decision, not a bugfix. |

---

*Document grounded in:*
- `crates/spur-tui/tests/palette_integration/util.rs`
- `crates/spur-tui/tests/dashboard_composer_contract.rs`
- `crates/spur-tui/tests/composition_intent_integration.rs`
- `crates/spur-tui/tests/landing_paths.rs`
- `crates/spur-tui/tests/detail_pane_scroll.rs`
- `crates/spur-tui/tests/render_golden.rs`
- `crates/spur-tui/tests/session_picker_render_snapshots.rs`
- `crates/spur-tui/tests/status_bar_flag_summary.rs`
- `crates/spur-tui/src/components/react_trace/render.rs` (lines 756–805, 844–879)
- `crates/spur-tui/src/components/input_bar.rs` (lines 1820–2013)
- `crates/spur-tui/src/views/dashboard.rs` (lines 333–336, 463–587)
- `crates/spur-tui/src/lib.rs` (`test_support` module, lines 16–85)
