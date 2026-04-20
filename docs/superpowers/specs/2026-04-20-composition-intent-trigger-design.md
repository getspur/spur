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

The `query` slice is computed as `text[prefix_start + 1 .. cursor]` at emit time; it's derived, not stored.

The "boundary" check for `@` uses the same rule as today: `prefix_start == 0 || prev_char.is_whitespace()`. The additional guard — that the `@`'s byte position is not the `start` of any `ProtectedRange` — closes the committed-atom case (I4). In practice a freshly typed `@` cannot coincide with an existing atom boundary unless the user's typing cursor is adjacent to an atom; we still guard to be explicit.

### Module boundaries

- `completion_trigger.rs` owns the state machine. Its public surface becomes:
  - `enum IntentEvent { TypedChar(char), DeletedChar, MovedCursor, Pasted, SetText, Accepted, Dismissed, Submitted }`
  - `enum TriggerTransition { None, Open { trigger: Trigger }, Update { query: String }, Close }` (unchanged shape; retains call-site compatibility)
  - `struct TriggerDetector { state: TriggerState }`
  - `impl TriggerDetector { fn new() -> Self; fn step(&mut self, event: IntentEvent, text: &str, cursor: usize, protected_ranges: &[ProtectedRange]) -> TriggerTransition; fn reset(&mut self); }`
  - The current `detect(text, cursor) -> Option<Trigger>` free function is **removed** (no migration layer).

- `views/session_detail.rs` classifies each key/pointer/edit path into exactly one `IntentEvent` and calls `detector.step(event, text, cursor, ranges)`. A single helper `fn dispatch_intent(&mut self, event: IntentEvent)` fans out: read text, cursor, ranges from `input_bar`, call `step`, apply the transition to `self.picker_shell`. Every call site that today calls `refresh_popup()` is replaced by a `dispatch_intent(...)` with a concrete event.

- `input_bar.rs` exposes one new affordance: a way for the view to know, from a given `KeyEvent`, which `IntentEvent` to classify it as. Two reasonable factorings:
  - **(a)** View classifies directly based on the key it's about to dispatch (simpler; view already knows if it's calling `insert_char` vs `move_cursor_back`).
  - **(b)** `InputBar::handle_key` returns an `EditOutcome { mutated: bool, deleted: bool, inserted_char: Option<char>, moved: bool }` alongside its existing return. View maps outcome → event.

  **Decision: (a).** The view already branches per-key; adding a second path is less churn than threading an outcome struct through every handler. `input_bar.rs` stays focused on text/cursor/range bookkeeping; `session_detail.rs` stays the event classifier.

### Classifier reference table (session_detail)

| Dispatched action | IntentEvent |
|---|---|
| `input_bar.insert_char(c)` for printable char | `TypedChar(c)` |
| `input_bar.backspace()` / `delete_forward()` | `DeletedChar` |
| `input_bar.insert_paste(...)` | `Pasted` |
| `input_bar.set_text(...)`, history swap (`Up`/`Down`), snapshot restore | `SetText` |
| any cursor-only key: ←, →, ↑, ↓, Home, End, Ctrl-A, Ctrl-E, Ctrl-F/B, word-motion, `g`/`G` vim jumps, mouse click-to-position | `MovedCursor` |
| user selected a picker row | `Accepted` (emitted manually by the accept code path, no key dispatch) |
| Esc while picker open | `Dismissed` |
| Enter submit | `Submitted` |

Selection drag (`Shift+←` etc.) classifies as `MovedCursor` — consistent with I3 (motion refines/closes, never opens).

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
key/pointer/edit event
        │
        ▼
session_detail::dispatch_intent(event: IntentEvent)
        │
        ├─► input_bar state (already updated by the edit call, if any)
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

A small integration test in `session_detail` (or a new `tests/composition_intent_integration.rs`) verifies the classifier — that `handle_key` paths for ←, →, Home, End, Shift+←, paste, history recall all map to `MovedCursor` / `Pasted` / `SetText` and thus never Open the picker when atoms or stray `@`s are present.

### What is removed (no migration)

- `completion_trigger::detect(text, cursor) -> Option<Trigger>` public free function.
- The `detect_tests` block that tests the free function (replaced by state-machine tests).
- `session_detail::refresh_popup()`.
- Existing `detector.step(text, cursor)` signature and tests that exercised `Open`/`Update`/`Close` purely from text diffs (replaced by event-driven tests).

`Trigger`, `TriggerKind`, `TriggerTransition` keep their public shape — the shell construction sites in `session_detail::dispatch_intent` consume them unchanged.

## Rationale vs. rejected alternatives

- **Atom-aware `detect()` only** (rejected): solves committed-atom case but leaves paste, history, selection-drag, mouse-click, auto-repeat, and surprise Ctrl-E pop-ins on the table. Scored 49/90 on journey MCTS.
- **Atom-aware `detect()` + motion gate** (rejected): ties on journey score with the session-memory variant but still parses corpus text as triggers (paste case). Composition-Intent subsumes it by tracking intent directly.
- **Session-scoped shell memory** (rejected): extra state machine layered on a still-incorrect recognizer; more invalidation rules, more tests, marginal UX gain over the intent model.

The Composition-Intent Model is the L9 answer because it names the actual abstraction the code was missing: *a trigger has a lifecycle*, and that lifecycle is driven by the user, not by the text buffer's content.

## Non-goals

- Multi-char triggers (`##`, `::`, etc.). Out of scope.
- IME / composition-event handling beyond what `tui_textarea` already surfaces.
- Changes to `PickerShell`, `MentionQuerySource`, `SlashQuerySource`, or the mention registry.
- Changes to how accepted mentions become `ProtectedRange`s (that plumbing stays as today).
- Mouse-selection of picker rows. (Separate concern.)
