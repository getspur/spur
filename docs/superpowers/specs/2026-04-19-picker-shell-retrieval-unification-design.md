# PickerShell — Retrieval UI Unification (Stage 2)

**Date:** 2026-04-19
**Scope:** `crates/spur-tui/src/components/` (new `picker_shell.rs`, `mini_input.rs`, `query_source.rs`), `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/components/completion_trigger.rs`, `crates/spur-tui/src/components/completion_popup.rs`
**Status:** Draft — approved design 2026-04-19
**Related docs:** `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md` (Stage 1, data model), `docs/superpowers/specs/2026-04-13-chat-input-commands-mentions-design.md`

## Problem

Stage 1 made history entries exact-replay snapshots, but left the retrieval UI inconsistent:

- `Ctrl+R` routes keystrokes into an invisible `history_search: Option<String>` sink. The query is never rendered. The `InputBar` continues to show the prior draft with a blinking terminal cursor, while keys go elsewhere. Users see the cursor lie about where their input lands.
- The per-keystroke `Matcher::new + Pattern::parse` in `session_detail.rs:646-647` also resets `CompletionPopup::state` selection to row 0 on every character — arrowing into a row and typing one more character loses the navigation.
- Text-only backfill twins of structured entries render as visually identical popup rows — the distinction that drives Goal #2 (preserve `ResourceLink` on re-submit) is invisible at accept time.
- Mention popup and slash popup use the same `CompletionPopup` widget but live in a different code path (`completion_trigger::detect()` + `active_trigger`). The three retrieval UIs share no abstraction.

Seven concrete Ctrl+R UX defects were enumerated in the prior Ctrl+R grounding review: invisible query, blinking cursor on frozen composer, no mode indicator, EOL-only accept cursor, twin ambiguity, silent empty-Backspace, selection reset on every keystroke.

## Goals

1. The retrieval query is always visible, with cursor placement that matches where keys land.
2. `InputBar` remains strictly the composer — it holds outbound draft text only. Navigation scratch text lives elsewhere.
3. Mention, slash, and history pickers share one popup widget, one row type, one accept dispatch model.
4. Adding a future picker kind (session picker, agent picker, command palette) is a new `QuerySource` implementation — no state-machine growth in `InputBar` or `SessionDetailView`.
5. Stage 2 ships in four monotone phases; each phase is independently shippable and reversible.

## Non-goals

- Replacing `nucleo` ranking.
- Changing the on-disk `InputHistoryEntry` shape (owned by Stage 1 spec).
- Adding mouse/click handling.
- Multi-line queries.
- Multi-process safety on metadata (still deferred per Stage 1 Non-goals).
- Vim modal bindings inside the picker query surface.

## Decision

Introduce `PickerShell` — a popup shell that owns its own one-line query surface (`MiniInput`) and wraps the existing `CompletionPopup`. All three trigger kinds (history, mention, slash) route through `PickerShell` against a `QuerySource` trait. `RetrievalAccept` dispatches back to `InputBar` via existing public methods.

This design was chosen over two alternatives considered in detail:

- **Sentinel-char trigger reusing `completion_trigger::detect()`**: rejected — forces zero-width `ProtectedRange` extension (breaks `apply_deleted_span` and non-overlap invariants) and makes `detect()` kind-polymorphic on the whitespace-termination rule (history queries contain spaces, mention queries cannot).
- **Clear-and-search into `InputBar`**: rejected — overloads `InputBar::draft` (already used by Ctrl+P/N linear browse) with a second role, bifurcates Enter semantics (submit vs accept), and stores throw-away navigation text in a buffer whose identity is "outbound draft."

First-principles rationale: a history query is *scratch* text (discarded on accept, vanishes on Esc, never reaches ACP). A mention prefix is *outbound* text (becomes a `ResourceLink` in the outgoing message). These are ontologically different; they belong on different surfaces. Convergent-evolution evidence: GNU readline, fzf, Slack `Cmd+K`, VS Code `Cmd+P`, IntelliJ Search Everywhere, Neovim Telescope, Emacs helm/ivy/consult, Zed command palette — every mature picker uses a separate query surface.

## Architecture

### New components

```rust
// crates/spur-tui/src/components/mini_input.rs
pub struct MiniInput {
    text: String,
    cursor: usize, // byte offset
}
// Contract — held deliberately:
// * Single-line only. No newline insertion.
// * No protected ranges. No history. No vim mode. No undo.
// * Public API: insert_char, backspace, delete, left, right, home, end,
//              paste(&str), clear, text() -> &str, cursor() -> usize.
// * ~80 LOC target. When a feature request would grow it past that,
//   redesign — do not extend.
```

```rust
// crates/spur-tui/src/components/query_source.rs
pub struct RetrievalRow {
    pub primary: String,       // main label
    pub secondary: String,     // description / metadata
    pub tag: String,           // right-aligned provenance tag
    pub atoms: Vec<(usize, usize)>, // byte ranges in primary to style as atoms;
                                    // ranges are validated against the final
                                    // (possibly truncated) primary string by
                                    // the QuerySource before being returned.
}

pub enum RetrievalAccept {
    ReplaceState(InputStateSnapshot),
    InsertAtom { text: String, uri: String, name: String },
    ReplaceTriggerToken { prefix_start: usize, replacement: String },
}

pub trait QuerySource {
    /// Filter and rank. MUST reuse an internal Matcher across calls.
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow>;
    /// Build accept payload for the row at `row_idx`.
    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept>;
    /// Title shown in the shell header (e.g. "History · bck-i-search", "Mentions · @").
    fn title(&self) -> &str;
    /// Whether the shell owns the query surface (true for history) or reads
    /// query from the InputBar trigger prefix (true for mention/slash).
    fn query_mode(&self) -> QueryMode;
}

pub enum QueryMode {
    OwnedByShell,           // MiniInput is visible and active
    ReadFromInputBar,       // shell reads query from `Trigger::query`
}
```

```rust
// crates/spur-tui/src/components/picker_shell.rs
pub struct PickerShell {
    source: Box<dyn QuerySource>,
    query: MiniInput,
    popup: CompletionPopup,
    rows: Vec<RetrievalRow>,
}

impl PickerShell {
    pub fn open(source: Box<dyn QuerySource>) -> Self;
    pub fn set_query_from_input_bar(&mut self, trigger_query: &str);
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction;
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect);
}

pub enum PickerAction {
    None,
    Accept(RetrievalAccept),
    Cancel,
}
```

### Unchanged primitives

`InputBar`, `ProtectedRange`, `InputStateSnapshot`, `InputHistoryEntry`, `CompletionPopup` (wrapping widget), `SessionMetadata` persistence — all unchanged.

### Data flow

```
key event
    │
    ▼
SessionDetailView::on_key
    │
    ├── if PickerShell active ───▶ PickerShell::handle_key
    │                                  │
    │                                  ├── Accept(r) ──▶ dispatch on RetrievalAccept:
    │                                  │                    • ReplaceState → input_bar.set_state
    │                                  │                    • InsertAtom   → input_bar.insert_atom
    │                                  │                    • ReplaceTrigger → input_bar.set_text
    │                                  ├── Cancel ─────▶ close shell; composer untouched
    │                                  └── None
    │
    └── else ─────────────────────▶ InputBar::handle_key (existing path)
                                       │
                                       └── if Trigger detected (mention/slash):
                                             open PickerShell(ReadFromInputBar mode)
```

## Phased rollout

Each phase is independently shippable and leaves the system in a working state.

### Phase 1 — Ctrl+R migrates to PickerShell

- Introduce `MiniInput`, `QuerySource` trait, `PickerShell`, `RetrievalAccept`.
- Implement `HistoryQuerySource` (`QueryMode::OwnedByShell`) that wraps `Vec<InputHistoryEntry>` and reuses a single `nucleo::Matcher`.
- Rewire `Ctrl+R` / `Alt+R` in `session_detail.rs:977-983` to open `PickerShell::open(Box::new(HistoryQuerySource::new(self.input_bar.history())))`.
- Delete `SessionDetailView::history_search: Option<String>`, `history_search_hits`, `refresh_history_popup`.
- Mention/slash continue to use `completion_trigger::detect()` + direct `CompletionPopup` — unchanged.

Phase 1 exit criteria:
- Pressing `Ctrl+R` shows a popup with a visible `search:` line, cursor in the MiniInput.
- Arrow keys navigate rows; selection survives across keystrokes (selection reset bug fixed).
- Tab and Enter accept the selected snapshot via `RetrievalAccept::ReplaceState`.
- Esc closes the shell; `InputBar` state is bit-identical to pre-Ctrl+R.
- `InputBar::render` skips the terminal cursor placement call when a `PickerShell` is active (prevents the "blinking cursor on frozen composer" artifact).

### Phase 2 — matcher reuse + row atom styling

- Add an internal `nucleo::Matcher` to each `QuerySource` impl; assert in tests that `refresh()` reuses it across calls.
- Extend `RetrievalRow.atoms` population in `HistoryQuerySource` so `entry.snapshot.protected_ranges` translate to byte spans on `RetrievalRow.primary`.
- `PickerShell`'s row renderer applies `Color::LightBlue + Modifier::UNDERLINED` to atom spans — matching `input_bar.rs:1362-1365` atom styling.

Phase 2 exit criteria:
- No per-keystroke `Matcher::new` / `Pattern::parse` allocation on the Ctrl+R hot path.
- Text-only backfill twins and ranges-bearing entries are visually distinct in the popup (atoms render colored + underlined).

### Phase 3 — mention and slash migrate to PickerShell

- Implement `MentionQuerySource` and `SlashQuerySource` (`QueryMode::ReadFromInputBar`).
- When `completion_trigger::detect()` returns `Some(trig)`, `SessionDetailView` opens `PickerShell::open(...)` with the matching source and calls `shell.set_query_from_input_bar(&trig.query)` on every InputBar text change.
- Accept paths:
  - `MentionQuerySource::accept` → `RetrievalAccept::InsertAtom { text, uri, name }` — same semantics as today's `accept_completion → insert_atom`.
  - `SlashQuerySource::accept` → `RetrievalAccept::ReplaceTriggerToken { prefix_start, replacement }` — same semantics as today's `accept_completion → replace_trigger_token`.
- User-visible behavior of mention/slash is unchanged: query still in `InputBar`, Tab accepts, Esc dismisses.

Phase 3 exit criteria:
- All three trigger kinds (history, mention, slash) route through the same `PickerShell` instance (one at a time).
- `session_detail.rs::accept_completion` deleted; `SessionDetailView::active_trigger` replaced by an `Option<PickerShell>`.
- Integration test parity: mention-insert-atom and slash-replace-token behavior match pre-migration.

### Phase 4 — retire `completion_trigger` as a separate concept

- `completion_trigger::detect()` becomes a `TriggerDetector` that emits a `Box<dyn QuerySource>` factory when a trigger is found.
- No user-visible change.

Phase 4 exit criteria:
- Grep for `active_trigger` returns no hits outside `picker_shell.rs`.
- `completion_trigger.rs` is pure detection — no popup wiring.

## Behavior contracts

### `MiniInput` scope contract

`MiniInput` is a deliberately small single-line editor. Target ~80 LOC. Public API exactly: `insert_char(char)`, `insert_paste(&str)` (strips newlines), `backspace`, `delete`, `left`, `right`, `home`, `end`, `clear`, `text() -> &str`, `cursor() -> usize`. No newline insertion, no protected ranges, no history, no vim mode, no undo.

When a feature request would grow `MiniInput` past this contract, the decision is binary: (a) reject scope creep, or (b) redesign to reuse `tui_textarea::TextArea` via a trait. Never extend incrementally.

### `InputBar` is inert while `PickerShell` is active

While `PickerShell.is_some()` in `SessionDetailView`:
- `InputBar::render` is still called (so the composer remains visible as context) but `frame.set_cursor_position(...)` is NOT called for the InputBar's cursor. The terminal cursor is placed by `MiniInput` instead.
- `InputBar::handle_key` is NOT called. All keys route through `PickerShell::handle_key`.
- `InputBar`'s border renders in dimmed style (e.g. `Color::DarkGray` instead of the mode color) as a visual cue that it is inert.

### Key bindings inside `PickerShell`

| Key | Action |
|---|---|
| Printable char | MiniInput insert (history) OR ignored (mention/slash — query comes from InputBar) |
| Backspace / Delete | MiniInput edit (history) OR routed back to InputBar (mention/slash) |
| Left / Right | MiniInput cursor (history) OR InputBar cursor (mention/slash) |
| Up / Down | `popup.select_prev` / `select_next` (all kinds) |
| Tab | Accept selected row (all kinds) |
| Enter | Accept selected row for history. For mention/slash: submit the InputBar as-is (including the in-progress trigger token) — matches today's behavior where Enter in an `@foo` trigger submits `@foo` literally without accepting a picker row. |
| Esc | `PickerAction::Cancel` — close shell, do not mutate InputBar |
| Ctrl+C | Same as Esc |
| Ctrl+R again | No-op while shell is open (does not re-open) |

### Accept cursor placement

`RetrievalAccept::ReplaceState` currently places the cursor at `snapshot.text.len()` (EOL). Preserved for Stage 2. Future refinement: restore the cursor position as stored in the snapshot — deferred until `InputStateSnapshot` carries a cursor field (Stage 3, not this spec).

### Cancel semantics

Esc always restores the InputBar to its state immediately before the shell opened. For Ctrl+R (history), that means the pre-Ctrl+R draft is retained verbatim — no stashing in `InputBar::draft`. For mention/slash, the trigger prefix typed so far is retained in the InputBar (same as today).

## Data-model consequences for Stage 1 spec

The Stage 1 spec's speculative Stage 2 sketch (lines 322-333) is realized here with minor shape changes:

- `RetrievalRow` gains a `secondary: String` and `atoms: Vec<(usize, usize)>` field (Stage 1 sketch had `secondary` as `String` already; `atoms` is new).
- `RetrievalAccept` gains a third variant `ReplaceTriggerToken` (Stage 1 sketch had two variants; slash needs a third shape because it replaces a trigger-anchored token, not the whole state).

The Stage 1 spec SHOULD be amended to reference this Stage 2 spec as the source of truth for retrieval UI shapes. No data-model change on disk.

## Testing

### Unit (Phase 1)

- `mini_input.rs`: round-trip text+cursor across every public op, including multi-byte UTF-8 (`你好world`).
- `query_source.rs::HistoryQuerySource`: empty-query returns newest-20 in reverse order; non-empty query calls `Matcher` once per keystroke (mock or count allocations); `accept(idx)` returns `ReplaceState(entry.snapshot)`.
- `picker_shell.rs`: Tab and Enter emit `PickerAction::Accept`; Esc emits `Cancel`; arrow keys mutate popup selection; open + query + select + accept sequence is deterministic.

### Integration (Phase 1)

- `ctrl_r_picker_shell_integration.rs` (new): press `Ctrl+R`, assert popup visible with empty query, type `ref`, assert query reads `"ref"`, assert popup rows narrowed; press `Down`, assert selection advances; press `Tab`, assert `InputBar::text()` matches selected entry and cursor at EOL; repeat with `Enter` as accept key; repeat with `Esc` and assert InputBar unchanged from pre-Ctrl+R state.

### Integration (Phase 2)

- `matcher_reuse_test.rs`: 100 simulated keystrokes against a 100-entry history allocate a bounded number of `Matcher` instances (target: 1).
- `atom_row_styling_test.rs`: history entries with `ProtectedRange`s render with `Color::LightBlue + Modifier::UNDERLINED` on those byte spans in popup rows.

### Integration (Phase 3)

- `mention_migration_parity.rs`: the pre-migration mention insert-atom test suite passes unchanged against the `MentionQuerySource` path.
- `slash_migration_parity.rs`: same for slash.

## Known risks

| Risk | Mitigation |
|---|---|
| `MiniInput` grows into a second TextArea | Hold the 80-LOC contract in code review; reject scope creep or redesign to wrap TextArea. |
| PickerShell and InputBar both try to place the terminal cursor | `SessionDetailView::render` is the single arbiter: when shell is active, InputBar's cursor placement is suppressed. |
| Mention/slash migration regresses existing behavior | Phase 3 ships with parity integration tests derived from the current suite; freeze the mention/slash behavior spec in test fixtures before migrating. |
| Ctrl+R while a trigger is active (user pressed `@`, then Ctrl+R) | Phase 1: reject Ctrl+R if `active_trigger.is_some()`. Phase 3: reject Ctrl+R if any `PickerShell` is open; user must Esc first. |
| Esc canceling an accidental accept is impossible | Accepted — accept is terminal. Future `Ctrl+Z` undo on `InputBar::set_state` is out of scope (would require re-enabling TextArea undo, which Stage 1 disabled for protected-range safety). |
| Slash-command's v1 "fires only at byte offset 0" rule is lost in migration | `SlashQuerySource::open` preserves the rule at trigger-detection time; once open, the shell doesn't re-check. |

## Next Steps

**Phase 1 (ship first):**
1. `mini_input.rs` — new file, ~80 LOC, unit-tested.
2. `query_source.rs` — new file: trait + `HistoryQuerySource` + `RetrievalAccept`.
3. `picker_shell.rs` — new file: shell struct + render + handle_key.
4. Rewire `Ctrl+R` in `session_detail.rs` to open the shell; delete `history_search*` state.
5. Suppress InputBar cursor when shell is open.
6. Dim InputBar border when shell is open.
7. Integration test `ctrl_r_picker_shell_integration.rs`.
8. Amend Stage 1 spec: mark its stale P0 list as closed (fixes already shipped); link to this Stage 2 spec.

**Phase 2:**
9. Internal `Matcher` in `HistoryQuerySource`; allocation-bounded test.
10. Atom span rendering in popup rows.

**Phase 3:**
11. `MentionQuerySource`, `SlashQuerySource`.
12. Migration of `active_trigger` → `Option<PickerShell>`.
13. Parity integration tests.

**Phase 4:**
14. Refactor `completion_trigger::detect()` into `TriggerDetector` that emits `QuerySource` factories.
15. Delete `accept_completion` and `active_mention_hits` from `SessionDetailView`.

**Deferred (out of Stage 2):**
16. Cursor position preserved in `InputStateSnapshot` (requires Stage 1 data-model revision).
17. Multi-line queries.
18. Session picker, agent picker, saved-prompt picker as additional `QuerySource` impls.
