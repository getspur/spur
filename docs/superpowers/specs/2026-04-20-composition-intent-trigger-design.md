# Composition-Intent Trigger Model for InputBar Pickers

Date: 2026-04-20
Status: Approved (brainstorm complete, ready for plan)
Area: `spur-tui` — `components/input_bar.rs`, `components/completion_trigger.rs`, `views/session_detail.rs`

## Problem

Cursor movement through text that contains `@` characters — including committed atomic mention tokens (`ProtectedRange`), pasted text, or history-recalled messages — causes `PickerShell` to pop open on every cursor tick. In the worst cases (arrow auto-repeat, `Shift+←` selection, mouse click, `Home`/`End`/`Ctrl-E`), the picker re-opens on every keystroke or pointer event, blocking selection, stealing focus, and allocating fresh picker state per tick.

Root cause is structural: `completion_trigger::detect(text, cursor)` is a pure text parser. It cannot distinguish:

1. **Live composition** — the user just typed `@` and is mid-query.
2. **Committed atom** — the `@foo-bar` text is opaque; cursor can't even land inside it.
3. **Corpus text** — `@` arrived via paste, history recall, or `set_text`.

`refresh_popup()` runs after every key event (including pure cursor motion), calls the text-only `detect()`, and opens/updates a `PickerShell` from whatever `rfind('@')` finds. The detector has no model of intent.

## Goal

Replace the text-only recognizer with a **Composition-Intent Model**: a state machine where trigger-liveness is a function of user intent events, not string content. Text + cursor are used only as context for the current query slice while `Composing`.

## UX Invariants (the contract)

- **I1 — Open only on intent.** The picker opens iff the user presses `@` (at a boundary) or `/` (at offset 0) as a single keystroke. No other event opens the picker.
- **I2 — Close on explicit termination.** Whitespace keystroke inside the query, `Esc`, accept, submit, or a delete that removes the trigger char closes the picker.
- **I3 — Motion refines, never summons.** While composing, cursor motion may update the query slice or close the picker (if the cursor leaves the trigger window). Cursor motion never opens a new picker.
- **I4 — Atoms are opaque.** Committed protected ranges are invisible to the detector regardless of their internal text. Cursor navigation atomically skips them (existing `move_cursor_back`/`move_cursor_forward` behavior).
- **I5 — Corpus is not composition.** Paste, history recall, and `set_text` never create a live trigger. They force the detector to `Idle`.

## Design

### State machine

```
enum TriggerState {
    Idle,
    Composing { kind: TriggerKind, prefix_start: usize },
}
```

Events the detector accepts:

| Event | Carries |
|---|---|
| `TypedChar(c)` | character the user just typed, cursor is already at its post-type position |
| `DeletedChar` | a single-char deletion (Backspace/Delete) just completed |
| `MovedCursor` | pure cursor motion (arrow, Home, End, Ctrl-A/E, mouse-click-to-position, vertical nav) |
| `Pasted` | `insert_paste` ran |
| `SetText` | bulk `set_text`, history swap, snapshot restore |
| `Accepted` | picker accepted a selection (view-driven) |
| `Dismissed` | user pressed Esc on the picker (view-driven) |
| `Submitted` | Enter submit (view-driven) |

### Transitions

| From | Event | Guard | To | Emit |
|---|---|---|---|---|
| Idle | `TypedChar('@')` | cursor is at boundary (offset 0 or prev char is whitespace) AND `@`'s position is NOT inside a protected range | `Composing{Mention, pos}` | `Open` |
| Idle | `TypedChar('/')` | cursor offset 1 AND `/` is at offset 0 | `Composing{Slash, 0}` | `Open` |
| Idle | any other event | — | Idle | `None` |
| Composing | `TypedChar(c)`, `c` not whitespace | — | Composing (same) | `Update { query }` |
| Composing | `TypedChar(whitespace)` | — | Idle | `Close` |
| Composing | `DeletedChar` | the deletion removed the trigger char at `prefix_start` | Idle | `Close` |
| Composing | `DeletedChar` | otherwise | Composing | `Update { query }` |
| Composing | `MovedCursor` | cursor is in the trigger window (see below) | Composing | `Update { query }` |
| Composing | `MovedCursor` | cursor left the trigger window | Idle | `Close` |

**Trigger window definition.** Given `prefix_start` and the current `text`, the trigger window is `[prefix_start + 1, window_end]` inclusive on the left, exclusive on the right, where `window_end` is the byte index of the first whitespace character at or after `prefix_start + 1`, or `text.len()` if none exists. A cursor position `c` is "in the window" iff `prefix_start < c <= window_end`. `c == prefix_start` (cursor sitting just before the `@`) is outside the window → Close.
| any | `Pasted` / `SetText` | — | Idle | `Close` (if was Composing) |
| any | `Accepted` / `Dismissed` / `Submitted` | — | Idle | `Close` (if was Composing) |
| any | `NoOp` | — | unchanged | `None` |

The `query` slice is computed as `text[prefix_start + 1 .. cursor]` at emit time; it's derived, not stored.

The "boundary" check for `@` uses the same rule as today: `prefix_start == 0 || prev_char.is_whitespace()`. The additional guard — that the `@`'s byte position is not the `start` of any `ProtectedRange` — closes the committed-atom case (I4). In practice a freshly typed `@` cannot coincide with an existing atom boundary unless the user's typing cursor is adjacent to an atom; we still guard to be explicit.

### Module boundaries

- `completion_trigger.rs` owns the state machine. Its public surface becomes:
  - `enum IntentEvent { TypedChar(char), DeletedChar, MovedCursor, Pasted, SetText, Accepted, Dismissed, Submitted, NoOp }`
  - `enum TriggerTransition { None, Open { trigger: Trigger }, Update { query: String }, Close }` (unchanged shape; retains call-site compatibility)
  - `struct TriggerDetector { state: TriggerState }`
  - `impl TriggerDetector { fn new() -> Self; fn step(&mut self, event: IntentEvent, text: &str, cursor: usize, protected_ranges: &[ProtectedRange]) -> TriggerTransition; fn reset(&mut self); }`
  - The current `detect(text, cursor) -> Option<Trigger>` free function is **removed** (no migration layer).

- `input_bar.rs` classifies every `KeyEvent` into an `IntentEvent` as part of its existing dispatch. `handle_key` changes its return type to an enum that carries the intent:

  ```rust
  pub enum HandleOutcome {
      /// Buffer submitted. Preserves today's submit tuple.
      /// View also emits IntentEvent::Submitted to the detector.
      Submit(String, bool),
      /// Ordinary key processed; carries the classified intent.
      Key(IntentEvent),
  }

  pub enum IntentEvent {
      TypedChar(char),
      DeletedChar,   // single-char or bulk delete (Ctrl+K, Ctrl+U, word-delete)
      MovedCursor,
      Pasted,        // emitted at view's insert_paste() call site
      SetText,       // emitted at view's set_text() / history-swap call site
      Accepted,      // emitted at view's picker-accept site
      Dismissed,     // emitted at view's Esc/cancel site
      Submitted,     // emitted alongside HandleOutcome::Submit
      NoOp,          // unhandled key
  }
  ```

  Every branch of `handle_key`'s match sets its intent. Rust exhaustiveness then compile-time-enforces the classifier: adding a new edit branch without deciding its `IntentEvent` is a build error.

- `views/session_detail.rs` consumes `HandleOutcome` and emits non-key intents at their call sites (paste, set_text, accept, dismiss). A single helper `fn dispatch_intent(&mut self, event: IntentEvent)` centralises the detector call, including a **fast-path short-circuit** for the common case of `Idle` state + non-opening event:

  ```rust
  fn dispatch_intent(&mut self, event: IntentEvent) {
      // History-shell owns the picker; detector is inert.
      if matches!(
          self.picker_shell.as_ref().map(|s| s.query_mode()),
          Some(QueryMode::OwnedByShell)
      ) {
          self.trigger_detector.reset();
          return;
      }
      // Fast path: Idle + non-opening event → no text fetch, no alloc, return immediately.
      if self.trigger_detector.is_idle()
          && !matches!(
              event,
              IntentEvent::TypedChar('@') | IntentEvent::TypedChar('/')
          )
      {
          return;
      }
      let text = self.input_bar.text();
      let cursor = self.input_bar.cursor();
      let ranges = self.input_bar.protected_ranges();
      let transition = self.trigger_detector.step(event, &text, cursor, ranges);
      // apply transition to self.picker_shell (same match as today)
  }
  ```

  The fast path is the perf contract for J6 (auto-repeat), J7 (selection drag), J8 (mouse click): zero `text()` allocations when idle.

**Rationale for outcome-return over view-classify.** The view today does NOT know what `input_bar.handle_key` will do — `InputBar` owns internal decisions (protected-range blocks, vim operator-pending, Ctrl+K multi-char delete, insert-with-protected-check compound ops). A view-side classifier that parallels those decisions would be a second source of truth that drifts silently when `InputBar` evolves. Returning the outcome puts the classifier at the exact site where the decision is made. This mirrors the existing `PickerShell::handle_key(key) -> PickerAction` pattern in the codebase.

### Intent classification table (inside `InputBar::handle_key`)

| `handle_key` branch | `IntentEvent` returned |
|---|---|
| `insert_char_with_protected_check(c)` for any printable char | `TypedChar(c)` |
| `delete_char_before_cursor()` / `delete_char_after_cursor()` (incl. atom-unit deletion) | `DeletedChar` |
| `delete_line_by_end()` / `delete_line_by_head()` (Ctrl+K / Ctrl+U) | `DeletedChar` |
| `delete_span(...)` word-delete | `DeletedChar` |
| any cursor-only branch: `move_cursor_back/forward`, `CursorMove::{Up,Down,Head,End,WordForward,WordBack,WordEnd,Top,Bottom,Jump}` | `MovedCursor` |
| Enter submit branch | `HandleOutcome::Submit(text, interrupt)` — view then emits `Submitted` |
| vim-pending intermediate (e.g., first `g` of `gg`) that produces no edit | `NoOp` |
| unhandled key (falls through match) | `NoOp` |

Call sites outside `handle_key` that the view emits explicitly:

| View call site | `IntentEvent` |
|---|---|
| `input_bar.insert_paste(...)` on `Event::Paste` | `Pasted` |
| `input_bar.set_text(...)` / history swap (Up/Down recall) / snapshot restore | `SetText` |
| `input_bar.insert_atom(...)` after picker-accept | `Accepted` |
| `self.picker_shell = None` on Esc / `PickerAction::Cancel` | `Dismissed` |
| `HandleOutcome::Submit` arm of `handle_key` | `Submitted` |

Selection drag (`Shift+←` etc.) is a cursor-only branch → `MovedCursor`, consistent with I3.

### Edge cases covered

- **Typing `@` immediately after a committed atom's closing char** (no whitespace between) — the boundary check says "prev char is not whitespace", so **no Open**. Correct: the pill needs a whitespace to its right before a new mention begins. This matches today's behavior, preserved.
- **Typing `@` at the very start of the input** — offset 0 is a valid boundary. Open.
- **Typing `/` at offset 1+** — not offset 0 → stays Idle. Preserved from today.
- **Cursor motion into the middle of an uncommitted `@query`** — if the detector is already Composing (e.g., picker was open), motion stays in Composing with an Update that trims the query. Picker remains open, refines. Matches P3.
- **Cursor motion into the middle of an uncommitted `@query` after Esc** — detector was reset to Idle by `Dismissed`. Motion alone can't open it (I3). User must type a char to re-summon — matches P2 (explicit dismiss stays dismissed).
- **Paste ending in bare `@foo`** — `Pasted` forces Idle. **No picker opens.** (Approved.)
- **History recall of a message containing atoms** — `SetText` forces Idle. Atoms remain as protected ranges (restored from the history entry). Cursor lands at end with no picker. I5 satisfied.
- **Shift+← selection drag across stray `@`** — `MovedCursor` only; detector is Idle; nothing happens. J7 fixed.
- **Mouse click past stray `@`** — `MovedCursor` from Idle → Idle. J8 fixed.
- **Arrow auto-repeat across text with stray `@`s** — every tick is `MovedCursor` from Idle → Idle. Zero allocations, zero flicker. J6 fixed.

### Data flow

```
KeyEvent
   │
   ▼
InputBar::handle_key(key) ──► HandleOutcome::{Submit(...) | Key(intent)}
   │
   ▼
session_detail receives outcome
   │
   ├─ Submit arm   ──► process submit; emit IntentEvent::Submitted
   └─ Key arm      ──► emit the carried IntentEvent
                 │
other view paths:
   insert_paste   ──► emit Pasted
   set_text       ──► emit SetText
   insert_atom    ──► emit Accepted
   picker-cancel  ──► emit Dismissed
                 │
                 ▼
session_detail::dispatch_intent(event)
                 │
                 ├─ fast-path return if Idle and event is non-opening
                 │
                 ▼
TriggerDetector::step(event, text, cursor, protected_ranges)
                 │
                 ▼
TriggerTransition { None | Open | Update | Close }
                 │
                 ▼
self.picker_shell ← apply(transition)
```

`refresh_popup()` is deleted. All call sites become `dispatch_intent(IntentEvent::…)`.

### Performance contract

The motivating bug includes a performance pathology: under arrow auto-repeat (~20/s) or selection drag, today's path calls `textarea.lines().join("\n")` — an O(N) string copy — per event, plus a full `detect()` scan that opens/updates a fresh `PickerShell` with per-tick allocations. The new design pins the following:

1. **Idle fast-path.** `dispatch_intent` returns in O(1) without touching text, cursor, or ranges when the detector is `Idle` and the event is not `TypedChar('@')` or `TypedChar('/')`. Every `MovedCursor`, `DeletedChar`, `Pasted`, `SetText`, `Accepted`, `Dismissed`, `NoOp` in Idle is constant-time.
2. **Zero alloc in fast-path.** No `String` from `text()`, no `Vec` from `lines()`, no `Trigger`/`PickerShell` constructions. Verified by unit test `idle_movecursor_does_not_fetch_text` with a spy `InputBar` (or by asserting `is_idle()` was short-circuited).
3. **No per-tick PickerShell reconstruction.** A `Composing → Composing` transition emits `Update { query }`, which calls `PickerShell::set_query_from_input_bar(&query)` — no shell rebuild. Only `Idle → Composing` builds a new shell (once per `@`-typed event).
4. **State machine is branch-predictor-friendly.** The `match (state, event)` is a small table with predictable Idle-heavy branch on the hot path.

### Error handling

No new fallible paths. The detector's state is purely in-process and in-memory; no I/O. Invariant violations (e.g., `Composing` with a `prefix_start` no longer inside `text`) are defended against with an internal re-check at the top of `step`: if `prefix_start > text.len()` OR text[prefix_start..].chars().next() != Some(trigger_char), force `state = Idle` and emit `Close`. This guards against any upstream edit path that forgot to send a `Pasted`/`SetText` event.

### Testing strategy

Unit tests live in `completion_trigger.rs` as today. New test matrix covers every row of the transition table plus these journey-level tests:

1. `power_user_proofread_never_opens_over_atoms` — insert two atoms, walk cursor Home→End one step at a time, assert `Open` count is zero.
2. `typo_fix_with_picker_open_updates_query` — TypedChar('@'), TypedChar('f'), TypedChar('o'), TypedChar('o'), MovedCursor back one position → assert Composing, assert last emit is `Update { query: "fo" }` or similar.
3. `typo_fix_after_esc_stays_closed_on_motion` — same as above but with `Dismissed` after the 4 TypedChars; MovedCursor emits `Close` once (or `None` if already Idle), never `Open`.
4. `paste_with_stray_at_does_not_open` — TypedChar sequence building text, then `Pasted` event → state Idle, emit `Close` (or `None`).
5. `history_recall_does_not_open` — `SetText` → Idle regardless of content.
6. `selection_drag_across_at_stays_idle` — 20× MovedCursor from Idle → all `None`, zero `Open`.
7. `mouse_click_past_stray_at_stays_idle` — single MovedCursor to position past `@text` → Idle.
8. `auto_repeat_left_arrow_across_atom_stays_idle` — 50× MovedCursor, no Opens.
9. `typed_at_inside_word_does_not_open` — TypedChar('x'), TypedChar('@') with prev non-whitespace → no Open.
10. `typed_at_right_after_atom_no_whitespace_does_not_open` — insert atom, then TypedChar('@') directly → no Open.
11. `defensive_reset_on_stale_prefix_start` — manipulate state to have prefix_start past text.len(), call step → Idle, Close emitted.

Classifier tests live in `input_bar.rs`'s existing test block (line 1527+). They exercise `handle_key` and assert on the returned `HandleOutcome::Key(IntentEvent)`:

- `arrow_keys_return_moved_cursor`
- `backspace_returns_deleted_char`
- `printable_char_returns_typed_char`
- `ctrl_k_returns_deleted_char`
- `enter_returns_submit`
- `vim_pending_first_g_returns_noop`
- `unhandled_key_returns_noop`

A small integration test in `tests/composition_intent_integration.rs` verifies the end-to-end flow: constructs a view-equivalent harness, drives `handle_key` + `insert_paste` + `set_text`, asserts `PickerShell` never opens on `MovedCursor` / `Pasted` / `SetText` when atoms or stray `@`s are present (covers J6/J7/J8 at integration level).

### What is removed (no migration)

- `completion_trigger::detect(text, cursor) -> Option<Trigger>` public free function.
- The `detect_tests` block that tests the free function (replaced by state-machine tests).
- `session_detail::refresh_popup()`.
- Existing `detector.step(text, cursor)` signature and tests that exercised `Open`/`Update`/`Close` purely from text diffs (replaced by event-driven tests).

`Trigger`, `TriggerKind`, `TriggerTransition` keep their public shape — the shell construction sites in `session_detail::dispatch_intent` consume them unchanged.

## Rationale vs. rejected alternatives

**UX model alternatives:**

- **Atom-aware `detect()` only** (rejected): solves committed-atom case but leaves paste, history, selection-drag, mouse-click, auto-repeat, and surprise Ctrl-E pop-ins on the table. Scored 49/90 on journey MCTS.
- **Atom-aware `detect()` + motion gate** (rejected): ties on journey score with the session-memory variant but still parses corpus text as triggers (paste case). Composition-Intent subsumes it by tracking intent directly.
- **Session-scoped shell memory** (rejected): extra state machine layered on a still-incorrect recognizer; more invalidation rules, more tests, marginal UX gain over the intent model.

**Classifier placement alternatives** (within the Composition-Intent Model):

- **View classifies from KeyCode** (rejected): requires the view to re-derive which action `InputBar` will take (protected-range blocks, vim operator-pending, multi-char deletes). Two sources of truth → silent drift when `InputBar` evolves. Scored 46/70 on perf+architecture MCTS.
- **Event queue** (rejected): foreign pattern in spur-tui; unjustified plumbing. Scored 48/70.
- **Detector inside `InputBar`** (rejected): widens `InputBar`'s remit to include overlay-state concerns; couples it to `PickerShell`. Scored 57/70.
- **`InputBar` returns `HandleOutcome`** (chosen): mirrors `PickerShell::handle_key → PickerAction` — an established spur-tui pattern. Compile-time exhaustiveness enforces the classifier. Scored 68/70.

The Composition-Intent Model is the L9 answer because it names the actual abstraction the code was missing: *a trigger has a lifecycle*, and that lifecycle is driven by the user, not by the text buffer's content. The outcome-return factoring is the L9 answer because it keeps the decision and its declaration co-located — the only place that knows what was done is the place that did it.

## Non-goals

- Multi-char triggers (`##`, `::`, etc.). Out of scope.
- IME / composition-event handling beyond what `tui_textarea` already surfaces.
- Changes to `PickerShell`, `MentionQuerySource`, `SlashQuerySource`, or the mention registry.
- Changes to how accepted mentions become `ProtectedRange`s (that plumbing stays as today).
- Mouse-selection of picker rows. (Separate concern.)
