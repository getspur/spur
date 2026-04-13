# Lineage Event-Contract Hardening — Design

**Date:** 2026-04-13
**Status:** Proposed for review
**Owner:** spur-acp / spur-core (consumer updates in spur-tui, spur-cli)
**Follows:** `2026-04-13-executor-lineage-visualization-design.md`

## Goal

Harden the `SpurEvent` contract so typos can't silently drift state, replays produce identical projections, and duplicate type definitions between wire and projection layers collapse into one authoritative source.

## Non-goals

- Orchestrator-side logic that translates `ExecutorReviewResolved` into
  a brain tool-call result. Still a separate future spec.
- New event variants. Scope is hardening, not expanding.
- Behavioral changes to `ExecutorLineage` beyond what the contract
  fixes require (no new methods, no new invariants).

## Motivation

A three-axis final review of the executor-lineage implementation
(commits `25d8bdb`..`68b23a6`) surfaced two structural flaws the
initial spec did not catch:

1. **Stringly-typed wire fields.** `ExecutorPhaseChanged { phase: String }`
   and `ExecutorSpawned { role: String }` silently no-op or
   misclassify on orchestrator typos. Invariant-axis reviewer
   confirmed: a `"running"` lowercase typo freezes the executor in
   its previous phase with no diagnostic signal.

2. **`SystemTime::now()` inside `apply()`.** The projection calls
   wall-clock at apply time, not event time. Same event stream
   applied at T₁ and T₂ produces different `started_at`/`ended_at`
   values. The spec's "pure projection" invariant is violated.

Secondary findings: duplicate wire/state type pairs across
`spur-acp` and `spur-core::lineage::types` (5 pairs, each with a
mapper function); silent orphan-buffer re-entry risk; undocumented
parent-before-child ordering requirement; `attempt_n` field stored
but never validated.

All are correctness or contract issues. Shipping the event contract
to main without fixing them would mean breaking-change-migrations
later — cheaper to fix now.

## Architecture

### Two load-bearing changes

**Change 1 — `SpurEvent` becomes an envelope.** The current
`pub enum SpurEvent { … }` becomes a struct wrapping a renamed
body enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    pub body: SpurEventBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEventBody {
    BrainSpawned { … },
    WorkerSpawned { … },
    // …all current variants…
    ExecutorSpawned { … },
    ExecutorPhaseChanged { id: String, phase: LifecycleState },
    ExecutorArtifact { … },
    ExecutorReviewRequested { id: String, kind: ReviewKind, payload: ReviewPayload },
    // no more requested_at — envelope carries it
    ExecutorReviewResolved { … },
    ExecutorRetryStarted { … },
}
```

Every event emission site captures `occurred_at: SystemTime::now()`.
The projection reads `event.occurred_at` and assigns it into
`Attempt.started_at` / `ended_at` — never calling `SystemTime::now()`
inside `apply`. Replay-purity restored.

**Change 2 — Wire types become canonical.** The 5 duplicate type
pairs collapse into single definitions owned by `spur-acp`:

| Duplicated pair | Canonical home | Action |
|---|---|---|
| `ReviewKind` (core) ↔ `ExecutorReviewKind` (acp) | `spur-acp::ReviewKind` | Delete core copy. Rename acp's version to drop the `Executor` prefix. |
| `ReviewPayload` ↔ `ExecutorReviewPayload` | `spur-acp::ReviewPayload` | Same. |
| `ReviewDecision` ↔ `ExecutorReviewDecision` | `spur-acp::ReviewDecision` | Same. |
| `Artifact` ↔ `ExecutorArtifactPayload` | `spur-acp::Artifact` | Same. |
| `DiffSummary` ↔ `ExecutorDiffSummary` | `spur-acp::DiffSummary` | Same. |
| `LifecycleState` (core) | `spur-acp::LifecycleState` | Move from core to acp. |
| `Role` (core) | `spur-acp::Role` | Move from core to acp. |

The 5 mapper functions in `spur-core::lineage::projection`
(`parse_phase`, `parse_role`, `map_artifact`, `map_review_kind`,
`map_review_payload`) become unused and are deleted.

`spur-core` re-exports the wire types from `spur-acp` for consumers
who already import from `spur_core::`:

```rust
// spur-core/src/lib.rs
pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind,
    ReviewPayload, Role,
};
```

Net code change: ~120 lines deleted (5 duplicate types + 5 mapper
functions + `parse_*` helpers). The envelope refactor adds
~20 lines. Total: smaller codebase.

### Projection-only types that stay in spur-core

These are projection state, not wire:
- `ExecutorId` — identity (wire uses `String`, projection wraps it)
- `ExecutorNode` — tree node container
- `Attempt`, `AttemptStatus` — retry history
- `ReviewRequest` — projection-side container for a pending review
  (wraps wire `ReviewKind` + `ReviewPayload` plus `requested_at`
  carried from envelope)
- `ExecutorLineage` — the projection itself

### Data flow change

```
Emitter                                    Consumer
────────                                   ────────
body = SpurEventBody::ExecutorSpawned{…}   match &event.body {
event = SpurEvent {                          SpurEventBody::ExecutorSpawned {…}
    occurred_at: SystemTime::now(),            => attempt.started_at
    body,                                         = event.occurred_at,
}                                            // never SystemTime::now() inside apply
broadcast.send(event)                      }
```

## Additional hardening items

### Parent-before-child ordering

Current state: `ExecutorSpawned` with an unknown `parent_id` silently
promotes the node to a new root. This is the inverse of the existing
child-orphan buffer (which handles phase-changes arriving before
spawns).

Fix: add a symmetric parent-orphan buffer. When
`ExecutorSpawned { parent_id: Some(p) }` arrives and `p` is not yet
in `nodes`, buffer the spawn event under `p` in
`parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>`
with the same 128-cap used for phase-change orphans. When the
parent's `ExecutorSpawned` eventually arrives, drain its parent-orphan
queue and replay the children.

This removes the undocumented ordering contract. Events can now
arrive in any order within the buffer window.

### Orphan-replay re-entry guard

Current: the orphan drain loop calls `self.apply(&ev)`, which calls
`apply_legacy` first, then dispatches on the variant. For v1 this is
safe because the legacy adapter only handles legacy `BrainSpawned` /
`WorkerSpawned` / etc., not `Executor*` events. Future code
adding `Executor*` handling to the legacy adapter would trigger
double-application.

Fix: split `apply` into two functions:

```rust
pub fn apply(&mut self, event: &SpurEvent) {
    super::adapter::apply_legacy(self, event);
    self.apply_inner(&event.body, event.occurred_at);
}

fn apply_inner(&mut self, body: &SpurEventBody, occurred_at: SystemTime) {
    match body {
        SpurEventBody::ExecutorSpawned { … } => { … }
        // … etc.
    }
}
```

Orphan drain calls `apply_inner`, bypassing `apply_legacy`. No
re-entry possible.

### `attempt_n` validation

Current: field is ignored in the match (`attempt_n: _`). If the
orchestrator skips attempts (emits `attempt_n = 5` when only 2
attempts exist), no warning.

Fix: validate in `ExecutorRetryStarted` arm:

```rust
let expected = node.attempts.len() as u32 + 1;
if *attempt_n != expected {
    tracing::warn!(
        executor_id = %id,
        got = *attempt_n,
        expected,
        "attempt_n mismatch — orchestrator may have dropped retry events"
    );
}
```

Still append the new attempt; don't block. Observability fix only.

### Double-apply idempotency contract

Current: projection is idempotent in practice (applying the same
`ExecutorReviewResolved` twice has no effect — `pending_review = None`
on a None-valued field is a no-op). Optimistic-apply in the TUI
relies on this but the invariant is undocumented.

Fix: add a module-level doc comment on `projection.rs` declaring the
invariant explicitly, plus a test that applies every event variant
twice and asserts state unchanged after the second application
(except `CostUpdate`, which is deliberately additive — document that
exception).

## Migration mechanics

All callers of `SpurEvent` need a two-layer refactor:

- **Constructors**: `SpurEvent::X { … }` → `SpurEvent { occurred_at: now, body: SpurEventBody::X { … } }`.
- **Matches**: `match event { SpurEvent::X { … } }` → `match &event.body { SpurEventBody::X { … } }`. `occurred_at` available as `event.occurred_at` when needed.

Touched files (grep-level estimate):
- `spur-acp/src/domain/events.rs` — schema
- `spur-acp/src/lib.rs` — re-exports
- `spur-acp/src/connection/**` — every emitter
- `spur-core/src/lineage/projection.rs` — apply + apply_inner split
- `spur-core/src/lineage/adapter.rs` — `apply_legacy` signature changes to `(lineage, &SpurEvent)` and matches on `&event.body`. Uses `event.occurred_at` for the `Attempt.started_at` it constructs for `BrainSpawned`/`WorkerSpawned` legacy paths.
- `spur-core/src/lineage/types.rs` — delete duplicate types
- `spur-core/src/lib.rs` — re-export wire types
- `spur-core/src/orchestrator.rs` — event emissions
- `spur-tui/src/app.rs` — every match
- `spur-tui/src/views/dashboard.rs` — every match
- `spur-tui/src/views/session_detail.rs` — every match
- `spur-tui/tests/*` — fixtures
- `spur-core/tests/lineage_*.rs` — fixtures
- `spur-cli/src/main.rs` — if it matches on events

No orchestrator semantic logic changes. No TUI UX changes. Pure
contract + type refactor.

## Error handling

- **Envelope deserialization failure** (malformed timestamp): treat
  as a fatal event-stream error (same as current behavior on
  malformed enum discriminant). Log + drop. No recovery attempt.
- **Unknown enum variant on deserialize**: serde errors out; caller
  sees `Err`. Current behavior for strings was to silently accept
  unknown values — the new typed fields fail loud at the
  deserialize boundary, which is the goal.
- **Parent-orphan buffer overflow** (>128): warn + drop, same pattern
  as existing child-orphan buffer.
- **`attempt_n` mismatch**: warn + apply anyway (don't block).

## Testing strategy

**Unit (spur-acp)**
- Envelope round-trips through serde JSON for every body variant.
- Typed-field deserialization failure: verify that a malformed
  `phase: "running"` (lowercase) JSON produces a serde deserialize
  error, not a silent acceptance. This is the regression guard for
  the stringly-typed contract fragility.

**Unit (spur-core)**
- Projection replay-purity: apply the same event list to two
  `ExecutorLineage` instances, assert **full equality** including
  all timestamps (currently excluded from the existing
  `replay_equals_live` test). This is the regression guard for the
  invariant violation.
- Parent-orphan buffer: emit child spawn before parent spawn,
  assert child attaches to parent on parent arrival.
- Double-apply: for each event variant, apply twice, assert state
  matches single-apply (except `CostUpdate` — explicitly additive).
- `attempt_n` mismatch warning: capture tracing output, assert warn
  fires on gap but state still progresses.
- Orphan replay re-entry: construct a scenario that would trigger
  double-application under the old `apply`, assert no duplication
  under the new `apply_inner` path.

**Integration**
- Full-flow test from existing `lineage_integration.rs` updated to
  use envelope construction. Assert replay-purity: build from the
  same event log twice, compare states byte-for-byte.

**Manual / smoke**
- Run TUI end-to-end with synthetic envelope events, verify nothing
  regressed visually.

## Build stages

The envelope refactor (stage 1) cannot be staged via a type alias —
struct and enum share the same name slot — so it lands atomically.
Subsequent stages are independently shippable.

1. **Envelope refactor (atomic).** Rename current `SpurEvent` enum to
   `SpurEventBody`. Introduce `pub struct SpurEvent { occurred_at:
   SystemTime, body: SpurEventBody }`. Update every emitter site to
   construct the envelope with `occurred_at: SystemTime::now()`.
   Update every matcher to destructure `&event.body`. Compiles + tests
   pass at end of stage. Single commit; mechanical.
2. **Move `LifecycleState`, `Role` to spur-acp.** Change the two body
   variants' fields from `String` to the typed enums. Delete
   `parse_phase`, `parse_role` in projection. `spur-core` re-exports
   the types for downstream consumers.
3. **Collapse 5 duplicate type pairs.** Delete core-side duplicates
   (`ReviewKind`, `ReviewPayload`, `ReviewDecision`, `Artifact`,
   `DiffSummary`). Delete 3 remaining mapper functions (`map_artifact`,
   `map_review_kind`, `map_review_payload`). Projection consumes wire
   types directly. `ReviewRequest` (projection container) still wraps
   the wire kind + payload plus `requested_at` carried from envelope.
4. **Wire `Attempt.started_at` / `ended_at` from `event.occurred_at`**.
   Remove every `SystemTime::now()` call from `apply` / `apply_legacy`.
5. **Add parent-orphan buffer + `apply_inner` split.** New
   `parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>`
   field; `apply_inner` bypasses `apply_legacy` for the orphan-drain
   path.
6. **Add `attempt_n` validation warn.**
7. **Replay-purity test with full-equality assertion.** Acceptance
   test for the whole hardening: apply the same event log twice to
   fresh projections, assert byte-for-byte state equality including
   all `started_at`/`ended_at` values.

## Open questions

None blocking. The orchestrator-side spec still tracks its own
separate design; this spec only defines the contract that
orchestrator will honor.
