# SPUR TUI Composer Contract Design

Date: 2026-04-22
Area: `crates/spur-tui`
Status: Approved for planning

## Summary

This design fixes a class of composer bugs in the SPUR TUI by tightening the
contract between:

- `InputBar`, which owns text editing and semantic token state
- view-level key routing in `SessionDetailView` and `DashboardView`
- popup ownership for trigger-driven and history pickers
- user-facing hints in the status bar

The best next step is a bounded contract refactor, not a one-off patch set and
not a full shared composer-controller rewrite.

## Problem

The current implementation has six linked behavior defects:

1. Empty-composer navigation is decided from post-edit state, so typing
   `j/k/g/G` from an empty composer is reinterpreted as navigation.
2. `InputBar` supports visual-line `Up/Down`, but real views usually never
   route those keys to the composer once text exists.
3. Several Vim edit paths clear all `protected_ranges`, which can silently
   degrade accepted mentions from semantic `ResourceLink` blocks into plain
   text on submit.
4. Some local composer/popup chords are dead because `App` reserves them
   globally first, notably `Ctrl+C` and `Ctrl+K`.
5. `Ctrl+P` and `Ctrl+N` bypass active picker ownership in `SessionDetailView`.
6. `[Esc]stop` is advertised even when Vim mode consumes the first `Esc`.

These defects share one root cause: the composer ownership contract is implicit
and inconsistent across the TUI.

## Goals

- Make key ownership explicit and deterministic before mutation.
- Preserve semantic token state across all supported edit paths.
- Align hints and supported chords with effective runtime behavior.
- Fix the defects in both `SessionDetailView` and `DashboardView` without a
  large architectural rewrite.
- Add tests that encode the contract directly, not just the symptoms.

## Non-Goals

- Do not introduce a new global composer subsystem in this change.
- Do not redesign Vim mode behavior beyond correctness fixes.
- Do not change global `App` key ownership for `Ctrl+C` or `Ctrl+K`.
- Do not unify all view routing into a shared abstraction unless the bounded
  refactor still leaves unacceptable duplication afterward.

## First-Principles Contract

### Ownership

For any incoming `KeyEvent`, ownership must be decided from pre-key state.

Possible owners:

- `Composer`: `InputBar` should receive and interpret the key.
- `Picker`: an active picker shell should consume the key.
- `View`: the surrounding view should interpret the key as navigation, review,
  or other non-composer behavior.

The current post-hoc pattern:

- send key into `InputBar`
- inspect the mutated buffer
- reinterpret the result as navigation

is explicitly retired.

### Semantic Text

`ProtectedRange` is semantic state, not presentation-only state. A committed
mention/file token must survive unrelated edits and must be removed only when
the edited span actually intersects it.

Any implementation that calls `protected_ranges.clear()` after a localized edit
violates the contract unless the entire buffer semantics are intentionally
discarded.

### Truthful UX

The status bar and local handlers must describe effective behavior, not
theoretical subcomponent behavior. If a key is globally owned by `App`, the
composer contract must not rely on it.

## Proposed Design

### 1. View Routing Contract

`SessionDetailView` and `DashboardView` will each adopt an explicit pre-key
ownership decision before dispatch.

The contract can be represented locally as:

```rust
enum KeyOwner {
    Composer,
    Picker,
    View,
}
```

This enum does not need to become a new shared framework in this phase. A
small local helper per view is sufficient if it makes the contract readable and
testable.

Rules:

- If a picker is active, it owns the key unless the key is explicitly allowed
  to fall through for trigger-driven editing.
- If the composer was empty before the key, view navigation shortcuts may win.
- If the composer was non-empty before the key, editing/navigation keys that
  belong to the composer must go to `InputBar`.
- No view may reinterpret a key by examining a one-character post-edit buffer.

### 2. Empty-Bar Navigation

Empty-bar navigation is based on `was_empty` before the key, not on the buffer
after mutation.

Consequences:

- Typing `j/k/g/G` into a non-empty composer must always type or edit.
- Typing `j/k/g/G` into an empty composer may navigate if the view contract
  says the view owns that key.
- Mode-entry keys in Vim Normal mode continue to fall through to the composer.

### 3. Multiline Cursor Movement

Once the composer is non-empty, `Up/Down` belongs to the composer unless an
active picker owns the key.

This restores the already-implemented `InputBar` visual-line movement contract
to real user journeys.

Empty-composer `Up/Down` behavior remains view-specific navigation.

### 4. Picker Ownership

Picker ownership becomes part of the first routing phase, not a later cleanup.

Rules:

- `OwnedByShell` picker state owns its full query surface and selection keys.
- `ReadFromInputBar` picker state owns accept/cancel/navigation keys and allows
  only explicit editing fallthrough into the composer.
- `Ctrl+P` and `Ctrl+N` must not mutate hidden composer state while any picker
  is active.
- If the design wants dismiss-first semantics for history shortcuts under an
  active picker, that must be implemented deliberately rather than as accidental
  bypass.

### 5. ProtectedRange Preservation

All edit paths in `InputBar` must preserve unaffected `ProtectedRange` values.

Required behaviors:

- deletion of a span removes only intersecting ranges and rebases later ranges
- insertion shifts later ranges forward by inserted byte count
- edits fully outside a protected range preserve it unchanged except for rebasing
- paste operations preserve unaffected ranges

Vim destructive paths such as `D`, `C`, visual `d/c`, operator `d/c`, and `p`
must use targeted range bookkeeping rather than blanket clearing.

Where `tui_textarea` only exposes coarse operations, SPUR must wrap those
operations with explicit byte-span accounting or replace them with internal
editing paths that can preserve semantics correctly.

### 6. Dead-Chord and Hint Alignment

The design accepts current global `App` ownership:

- `Ctrl+C` remains global quit
- `Ctrl+K` remains global palette

Therefore:

- composer/picker behavior must not depend on those chords
- unreachable local handlers should be removed or documented
- the status bar must only advertise `[Esc]stop` when first-press `Esc`
  actually cancels the stream in the current mode

## Components Affected

### `InputBar`

Responsibilities after this change:

- edit text
- preserve `ProtectedRange` semantics
- expose existing mode/capability queries such as `is_empty()`,
  `is_vim_normal()`, and `wants_esc()`

It does not decide cross-component ownership.

### `SessionDetailView`

Responsibilities after this change:

- decide pre-key ownership among composer, picker, and view
- dispatch according to that ownership
- stop using post-edit single-character rescue logic
- keep picker and history behavior internally consistent

### `DashboardView`

Responsibilities after this change:

- apply the same ownership principles as `SessionDetailView`
- keep its broader local navigation/review shortcuts, but only when the view
  truly owns the key from pre-key state

### `StatusBar`

Responsibilities after this change:

- render hints that match effective first-press behavior

## Error Handling and Edge Cases

- If a picker is active and a blocked shortcut is pressed, prefer deterministic
  no-op or picker-consistent behavior over mutating hidden composer state.
- If a Vim edit intersects an accepted mention, removing that mention is valid.
  Removing all mentions is not valid unless the edit span covers them.
- If the composer is empty and the current mode or focus context grants the key
  to the view, navigation wins without first mutating the composer.
- If status hints cannot cheaply express the exact mode-specific behavior, they
  must prefer conservative truth over convenience.

## Implementation Sequence

### Phase 1: Routing and Ownership

Combine the originally separate routing and picker fixes into one first phase.

Scope:

- `SessionDetailView`
- `DashboardView`

Deliverables:

- explicit pre-key ownership decision
- no post-edit rescue logic
- multiline `Up/Down` routed to composer when non-empty
- picker ownership respected before history/navigation fallthrough

### Phase 2: ProtectedRange Preservation

Scope:

- `InputBar`

Deliverables:

- remove blanket `protected_ranges.clear()` from localized edit paths
- preserve unaffected ranges under Vim edits and paste flows
- maintain submit-time semantic block assembly

### Phase 3: Hint and Chord Cleanup

Scope:

- `StatusBar`
- any unreachable local chord handlers

Deliverables:

- truthful `[Esc]stop` behavior
- dead local chord expectations removed or documented

## Testing Strategy

### Unit Tests

Add focused unit tests for the contract itself.

Routing contract:

- pre-key empty composer: `j/k/g/G` route to view navigation
- pre-key empty composer in Vim Normal: `i/a/A/I/o/O` route to composer
- non-empty composer: `j/k/g/G` do not trigger view rescue logic
- non-empty composer: `Up/Down` route to `InputBar`
- empty composer: `Up/Down` route to view navigation

Picker ownership:

- active picker + `Ctrl+P/Ctrl+N` does not mutate hidden composer state
- trigger-driven picker allows only intended editing fallthrough

ProtectedRange preservation:

- `dd`, `cc`, `D`, `C`, visual `d/c`, and `p` preserve unaffected ranges
- intersecting edits remove only touched ranges
- paste outside a protected range rebases later ranges without destroying them

Status hints:

- stream live + mode consumes `Esc` => hint does not promise first-press stop

### Journey Tests

Add integration tests for end-to-end user paths:

- typing `j/k/g/G` from empty vs non-empty composer
- multiline drafting with `Up/Down`
- mention/slash popup + history shortcuts
- accepted mention survives unrelated Vim edits and still submits as `ResourceLink`
- stream cancel behavior under Vim Insert/Visual/Operator modes

## Trade-Offs

### Why not local patches only

They would be faster short-term, but they preserve the same routing anti-pattern
and almost guarantee further drift between `SessionDetailView` and
`DashboardView`.

### Why not a shared composer controller now

That is a plausible end-state, but the current defect set does not justify the
extra abstraction and migration risk. The bounded refactor is enough to restore
correctness.

### Why not change global `App` shortcuts

That would broaden scope into app-wide interaction design. The current problem
is primarily local truthfulness and correctness, so the safer move is to align
local semantics with current global ownership.

## Acceptance Criteria

- Users can type `j/k/g/G` at the start of a prompt when the composer should
  own the key, and can navigate only when the view owns the key from pre-key
  state.
- Multiline drafts support `Up/Down` cursor movement in both Emacs and Vim
  insert flows when the composer is non-empty.
- Accepted mentions/files continue to submit as semantic `ResourceLink` blocks
  after unrelated Vim edits.
- Active picker state prevents hidden composer/history mutation.
- Status hints are truthful for first-press `Esc`.
- New tests encode the routing and semantic-token invariants directly.

## Open Follow-Up

After implementation, re-evaluate whether the ownership helpers in
`SessionDetailView` and `DashboardView` are still too duplicated. If yes,
extract a shared helper as a follow-up, not as part of this fix set.
