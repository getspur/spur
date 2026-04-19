# Chat Input Retrieval Unification — Structured History Foundation

**Date:** 2026-04-19
**Scope:** `crates/spur-tui/src/input_history.rs`, `crates/spur-tui/src/components/input_bar.rs`, `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/views/dashboard.rs`, `crates/spur-tui/src/session_metadata.rs`, `crates/spur-tui/src/app.rs`
**Status:** Draft - stage 1 implemented; review pass 2026-04-19 added defects, missing contracts, and pruned alternatives
**Related docs:** `docs/superpowers/specs/2026-04-13-chat-input-commands-mentions-design.md`, `docs/superpowers/specs/2026-04-18-input-bar-soft-wrap-design.md`

## Problem

SPUR currently has two different retrieval models for previously-authored chat
input:

- `InputBar` owns linear history browsing (`Ctrl+P` / `Ctrl+N`) and draft
  restoration.
- `SessionDetailView` owns fuzzy history retrieval (`Ctrl+R`) and popup UI.

That split is survivable for plain text, but incorrect once chat input can
contain structured atoms such as `@` mentions.

The pre-change history pipeline persisted global input history as
`Vec<String>`, derived from `blocks_preview()` text. That caused three concrete
problems:

1. **History recall was lossy.** A prompt that originally contained
   `ResourceLink` blocks round-tripped back into plain `@path` text.
2. **Re-submit could change semantics.** Recalling a prompt from history and
   pressing Enter no longer reproduced the original outbound blocks.
3. **The user mental model diverged from the implementation model.** Mentions
   were treated as first-class input atoms during editing, but as plain text as
   soon as they entered history.

The core issue is that retrieval persisted a presentation string, not the
actual restorable `InputBar` state.

## Goals

1. History recall must be an exact replay of chat composer state, including
   protected mention atoms.
2. Recalled prompts must preserve `ResourceLink` semantics on re-submit.
3. `InputBar` must remain the single source of truth for editable text,
   protected ranges, and draft restoration.
4. Existing `session_metadata.json` files must continue to load without a
   manual migration.
5. The design should establish a clean foundation for a future shared retrieval
   model across mentions and history.

## Non-goals

- Rebuilding the mention picker in this patch.
- Introducing a shared picker abstraction for commands, mentions, and history
  in the same change.
- Recovering mention atoms from old replayed session history where upstream ACP
  only provides flat text.
- Adding preview panes, match highlighting, multi-column layouts, or `fzf`-like
  advanced keymaps in this stage.
- Changing dashboard history UX beyond seeding it with the new structured
  entries.
- **Multi-process safety on `.spur/session_metadata.json`**. Two concurrent
  `spur watch` processes against the same metadata file will last-writer-wins.
  Acknowledged as a known limitation; see Risks and Next Steps. Industry
  comparison: zsh `INC_APPEND_HISTORY`, fish locked universal-history file.

## Grounding From Current TUI Behavior

### 1. `InputBar` already has the right ownership boundary

`InputBar` is where SPUR already models:

- text content
- cursor position
- protected mention atoms (`ProtectedRange`)
- draft preservation while browsing history
- linear history traversal

That makes `InputBar` the correct home for exact history restore behavior.
Moving durable history semantics out of `InputBar` would duplicate state and
increase the chance of cursor / range drift.

### 2. The lossy step was app-level history persistence

The old path took outbound blocks, flattened them with `blocks_preview()`, and
persisted only the resulting string. That string was suitable for trace echo,
but not for exact retrieval.

The design mistake was using one representation for two different jobs:

- user-visible preview
- durable restorable input state

Those representations are not equivalent once the input model includes
protected atoms.

### 3. ACP replay history is text-only

`spur_acp::HistoryEntry` contains only:

```rust
pub struct HistoryEntry {
    pub role: String,
    pub text: String,
}
```

So history backfilled from replayed session history cannot recover the original
mention atoms. That is an upstream truth constraint, not a TUI bug.

## Decision

Introduce a first-class input-history model that stores exact composer state:

```rust
pub struct InputStateSnapshot {
    pub text: String,
    pub protected_ranges: Vec<ProtectedRange>,
}

pub struct InputHistoryEntry {
    pub snapshot: InputStateSnapshot,
    pub submitted_at: Option<String>,
    pub session_id: Option<String>,
    pub agent: Option<String>,
}
```

This changes the retrieval contract:

- history is no longer `Vec<String>`
- history is now a sequence of exact restorable composer snapshots
- trace echo remains string-based
- history persistence becomes block-derived, not preview-derived

The design is staged:

1. **Stage 1, implemented now:** history persists and restores exact input
   snapshots.
2. **Stage 2, deferred:** mentions and history move onto a shared retrieval
   protocol and eventually a shared picker model.

## Data Model

### `InputStateSnapshot`

`InputStateSnapshot` is the minimal exact replay format for the composer:

- `text`: the literal input buffer
- `protected_ranges`: atom spans over `text`

It is intentionally narrower than the full `InputBar` state. It does **not**
store editing mode, visual scroll, or transient popup state.

### `InputHistoryEntry`

`InputHistoryEntry` adds retrieval metadata:

- `submitted_at`: optional timestamp for future ranking / display
- `session_id`: optional origin session
- `agent`: optional origin brain handle

This metadata is not required for restore correctness, but it is useful for
future retrieval UI and lets history rows carry lightweight provenance now.

### `ProtectedRange`

`ProtectedRange` now participates in serialization and equality so it can be
embedded directly in persisted history snapshots and compared during history
deduplication.

`start` and `end` are **byte indices** into `text`. After load, ranges MUST be
validated:

- `start <= end <= text.len()`
- both endpoints land on UTF-8 `char_boundary` positions
- ranges are sorted, non-overlapping

Invalid ranges from a corrupted or hand-edited metadata file MUST be dropped
(not panic). Defense-in-depth requirement; the codebase already enforces
char-boundary truncation in popup display (`session_detail.rs:670`) — the
same discipline applies to deserialized ranges.

### Supported `ContentBlock` kinds

`InputStateSnapshot::from_blocks` is in-scope for:

- `ContentBlock::Text` → contributes literal text
- `ContentBlock::ResourceLink` → `@name` text + matching `ProtectedRange`

All other ACP block kinds (`Image`, `Audio`, `EmbeddedResource`, future
additions) are **dropped** during snapshot construction. Recall of a prompt
that originally contained those kinds will reproduce only its text/mention
spine. This is a deliberate stage-1 narrowing; widening it requires either
(a) introducing additional snapshot atom kinds with their own protected-range
discipline, or (b) preserving the original blocks in an opaque side-channel.

### Versioning and forward compatibility

`SessionMetadata::version` is bumped when the on-disk shape of
`InputHistoryEntry` changes. Stage 1 introduces the structured shape; the
loader accepts both the structured form and the legacy `Vec<String>` form via
an `#[serde(untagged)]` fallback (`session_metadata.rs:70-89`).

Limitation: because every field of `InputStateSnapshot` and `InputHistoryEntry`
is `#[serde(default)]` and `snapshot` is `#[serde(flatten)]`, a future rename
inside the snapshot will silently produce empty entries rather than a load
error. Subsequent revs SHOULD adopt an explicit `v: u32` discriminator on the
entry rather than relying on flatten + untagged.

## Behavior

### 1. Submit path

When the user submits from `SessionDetailView` or Dashboard:

1. `SubmitRouter` produces outbound `ContentBlock`s as before.
2. `App` derives a trace preview string from `blocks_preview()` for local echo.
3. `App` derives an `InputHistoryEntry` from the actual outbound blocks:
   - `Text` contributes literal text
   - `ResourceLink` contributes `@name` text plus a matching `ProtectedRange`
4. The structured history entry is persisted to metadata and reseeded into live
   `InputBar`s.

The preview string remains for human-readable echo only. It is no longer the
storage format for retrieval.

### 2. Linear history (`Ctrl+P` / `Ctrl+N`)

`InputBar` now stores:

- history as `Vec<InputHistoryEntry>`
- draft as `InputStateSnapshot`

The linear browsing contract becomes:

1. entering history mode snapshots the current live draft, including mention
   atoms
2. browsing backward restores full snapshots, not just text
3. browsing back forward to the live draft restores the exact prior draft state

This preserves both plain text and atomized input without introducing special
case logic for mentions.

### 3. Fuzzy history retrieval (`Ctrl+R`)

`SessionDetailView` continues to own the fuzzy-history popup in stage 1, but it
now operates on `InputHistoryEntry` values instead of strings:

1. scoring is performed over `entry.snapshot.text`
2. popup rows may show lightweight metadata such as mention count or agent tag
3. accept restores the full `InputStateSnapshot` through `InputBar::set_state`

This means fuzzy history recall and linear history recall now agree on what a
history entry actually is.

### 4. Session-history backfill

Replayed ACP session history is still imported into global input history, but
those entries are backfilled as text-only `InputHistoryEntry::from_text(...)`
items because ACP does not provide structured atom metadata.

This is a deliberate truth-preserving limitation:

- new locally-submitted prompts round-trip exactly
- old replay-only prompts remain text-only

## Why `InputBar` Keeps Ownership

The retrieval boundary is:

- `SessionDetailView`: query, rank, render popup, accept selection
- `InputBar`: restore exact editable state

This separation is intentional. The popup layer decides **which** candidate to
load; `InputBar` decides **how** to restore the editable buffer and protected
atoms safely.

That keeps all cursor/range invariants in one place and avoids a second
"restore state into composer" implementation in higher-level views.

## Migration And Compatibility

`SessionMetadata.input_history` changes from:

```rust
Vec<String>
```

to:

```rust
Vec<InputHistoryEntry>
```

To preserve backward compatibility, the metadata loader accepts both:

- structured `InputHistoryEntry`
- legacy plain strings

Legacy strings are upgraded on read into text-only `InputHistoryEntry` values
with empty `protected_ranges`.

This makes the migration:

- transparent
- one-way
- crash-safe under the existing atomic metadata save path

## Relationship To Mention Retrieval

This spec does **not** unify mentions and history under one picker yet, but it
defines the prerequisite state model for doing so safely.

Both mention retrieval and history retrieval are now recognized as instances of
the same higher-level problem:

- query
- rank candidates
- show rows with metadata
- accept candidate into `InputBar`

The next design step should introduce a shared retrieval protocol, likely with
shapes along the lines of:

```rust
pub struct RetrievalRow {
    pub primary: String,
    pub secondary: String,
    pub tag: String,
}

pub enum RetrievalAccept {
    ReplaceState(InputStateSnapshot),
    InsertAtom { text: String, uri: String, name: String },
}
```

History and mentions differ mainly in accept semantics:

- history replaces the whole composer state
- mentions insert a protected atom into the current state

That unification is intentionally deferred until after the history model is no
longer lossy.

## Testing

The implemented stage is covered by focused tests:

- `input_bar_editing.rs`
  - history restore preserves protected ranges
  - draft round-trips through history browsing with protected ranges
- `session_detail_commands_integration.rs`
  - `Ctrl+R` recall of a prior prompt with a mention re-submits a
    `ResourceLink`, not plain text
- `session_metadata.rs`
  - legacy string history upgrades on load
  - structured history round-trips through save/load

These tests validate both the local `InputBar` invariants and the end-to-end
submit path.

## Known Defects (P0 — fix before stage 2)

These were found in the stage-1 implementation during the 2026-04-19 review
and are listed here so subsequent work cannot lose track of them.

1. **Undo/redo silently re-enabled on history restore.** `InputBar::new()`
   calls `set_max_histories(0)` at `input_bar.rs:82` "to prevent protected
   range desync". `restore_snapshot` at `input_bar.rs:1001-1013` rebuilds the
   `TextArea` from scratch and does **not** re-disable history. Outcome:
   after the first `Ctrl+P` or `Ctrl+R` accept, undo/redo is on, violating
   the exact invariant the original author called out. Fix: re-apply
   `set_max_histories(0)` inside `restore_snapshot`.

2. **`HISTORY_CAP` magic number was duplicated.** The cap lived as
   `HISTORY_CAP = 100` at `input_bar.rs:44` and as a hardcoded `100` at
   `app.rs:1417`. The two could drift independently, causing the in-memory
   and persisted caps to disagree. The 2026-04-19 review's earlier framing
   ("persisted history is unbounded") was inaccurate — every writer to
   `metadata.input_history` already routed through `merge_input_history_entry`
   which truncates on every push. Fix: lift `HISTORY_CAP` to
   `crate::input_history` and reference it from both sites.

## Risks

| Risk | Mitigation |
|---|---|
| Metadata size grows because history now stores `ProtectedRange`s and provenance | Capped at `HISTORY_CAP` (single-sourced from `input_history.rs`) at every push site. Earlier review wording suggesting the persisted layer was unbounded was incorrect; corrected after grounding `app.rs:1417`. |
| Replay-imported prompts still lose mention atoms | Explicitly documented as an ACP history limitation. |
| Replay backfill produces text-only twins of structured entries | `same_recall_state` dedup compares full snapshots, so a structured `"hello @foo"` with ranges and an ACP-replayed `"hello @foo"` without ranges are different entries and both persist. Acknowledged limitation; a future merge step could collapse text-equal pairs by preferring the ranges-bearing snapshot. |
| Duplicate history entries become harder to reason about | Dedup is defined in terms of exact recall state (`snapshot` equality), including `ProtectedRange.uri` — i.e. two prompts that *display* as `@foo` but resolve to different URIs are intentionally distinct. |
| History UI and mention UI still feel inconsistent | This spec treats that as a follow-on retrieval-UI problem, not a reason to keep lossy history. |
| Provenance fields half-wired | `submitted_at` and `session_id` are populated by `InputHistoryEntry::with_context` but not yet read in UI; only `agent` renders. Tracked under Next Steps; either wire them or remove. |
| Unsupported `ContentBlock` kinds silently dropped | Stage-1 supports only `Text` and `ResourceLink` (see Data Model). Non-text/non-link blocks vanish from history. Acknowledged; widening requires explicit additional atom kinds. |
| Multi-process metadata races | Out of scope for stage 1 (see Non-goals). Last-writer-wins on concurrent saves. Future work: append-only event log or file-locked write path (fish/zsh precedent). |
| Per-keystroke `Matcher` allocation in fuzzy popup | `session_detail.rs:646-647` constructs a fresh `nucleo::Matcher` and `Pattern` on every keystroke. Fine at N=100; the shared retrieval protocol (Stage 2) MUST mandate matcher reuse before history scales. |

## MCTS — Alternatives Considered

These branches were enumerated during design review. Chosen path is **A**;
the rest are recorded so future work doesn't relitigate them.

| # | Branch | Verdict |
|---|---|---|
| **A** | Snapshot-per-entry, untagged legacy fork, two UIs share data (this spec) | **Chosen for stage 1** — adopt P0 fixes above |
| B | Build the unified picker (mentions + history + commands) in the same change | Pruned — scope explosion, would block ship |
| C | Event-sourced history log (`InputHistoryEvent::{Submit,Delete}`), fold on load | Deferred — solves persisted-cap and multi-process races, but overshoot for stage 1; see Next Steps |
| D | Don't store ranges; reparse `@token` against live mention registry on restore | Pruned — violates "exact replay" the moment a mentioned file moves; the goal is frozen prompt semantics |
| E | Replace `#[serde(flatten)]` + untagged with explicit `v: u32` discriminator | Recommended for the next rev (see Versioning subsection) |
| F | Lift `take(20)` and `HISTORY_CAP` magic numbers into named consts | Trivial; bundle with P0 cleanup |

## Next Steps

**Immediate (P0, blocks stage 2):**

1. Re-disable `set_max_histories(0)` inside `restore_snapshot` (Defect #1).
2. Truncate persisted `metadata.input_history` to `HISTORY_CAP` before
   `save()`; lift the cap to a shared const (Defect #2).
3. Add `validate_ranges_within_text` on `InputStateSnapshot` deserialize
   path; drop invalid ranges (see ProtectedRange contract).

**Short-term (P1):**

4. Replace `#[serde(flatten)]` + untagged with an explicit `v: u32`
   discriminator on `InputHistoryEntry` and bump `SessionMetadata::version`
   accordingly.
5. Either wire `submitted_at` / `session_id` into the history popup row
   model, or remove them.
6. Reuse one `nucleo::Matcher` per `SessionDetailView` instead of
   reconstructing on every keystroke.

**Medium-term (Stage 2 retrieval unification):**

7. Introduce a shared retrieval abstraction above history and mentions.
8. Move mention rows onto the richer row model used by structured history.
9. Add match highlighting and empty-state rendering to the popup.
10. Rework mention ranking to use basename/path-aware scoring rather than a
    single flat fuzzy score.
11. Decide whether large-repo mention workflows should stay inline or
    escalate into an expanded picker mode.

**Longer-term (out of stage 2):**

12. Event-sourced history log (Branch C) for multi-process safety and
    bounded growth without per-save truncation. Industry precedent: fish
    universal history, zsh `INC_APPEND_HISTORY`.
