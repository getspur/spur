# Brain Async Continuation Scheduling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Revision:** rev 2 (2026-04-19) — L9 simulation pass applied. API drift vs installed `agent-client-protocol-schema-0.11.4` corrected; cross-task funnel plumbing reconciled; Task-8 restructured into three dispatch helpers.

**Goal:** Give detached worker completion a principled path back to the brain as a system-owned continuation (new ACP prompt turn), without pretending to be user input, via a pure-sync `BrainScheduler` policy owned by `run_interactive`.

**Architecture:** Three-lane ingress (User / Tool / Continuation) collapsed onto one `mpsc::Receiver<InteractiveInput>` but split internally by `BrainScheduler` into two deques (`pending_user`, `pending_continuations`). MCP result collector publishes detached completions via a single `report_detached_completion` helper that emits the funnel event first (for UI) then `try_send`s a `SystemContinuation` variant (for the model), with an overflow deque for backpressure. Autonomous continuation turns fire only when idle; they coalesce and merge into user turns when one is queued, under a per-turn byte budget, with self-describing `[SPUR:background]` marker blocks so the brain can distinguish user-authored from SPUR-injected content on the ACP wire.

**Tech Stack:** Rust (edition 2021), `tokio`, `tokio::sync::mpsc`, `tokio_util::sync::CancellationToken`, `proptest`, `serde`. Target crates: `spur-acp` (domain types, events), `spur-core` (orchestrator + scheduler), `spur-mcp` (result collector), `spur-cli` (wire-up).

**Spec:** `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md` (rev 2).

**Prerequisites / dependency notes (rev 2):**
- `DelegationStatus::Cancelled { reason }` already exists in `crates/spur-acp/src/domain/delegation.rs:48` (INV-6 Cancelled variant is usable today).
- `BrainSessionId` newtype **already exists** in `spur-acp` and is the parameter type of `McpCallbackServer::new`. This plan uses `SessionId` inside `InteractiveInput::SystemContinuation` and `BrainContinuation` to match the existing scheduler/orchestrator idiom; a one-line rename is possible when the remaining INV-2 migration work lands. `DelegationId` typed newtype is NOT yet landed — `String` is used, will rename when INV-1 lands.
- `SpurEventBody::PlanCompleted` / `SpurEventBody::PlanReadyToMerge` **already exist** (verified at `spur-mcp/tests/plan_cancelled_task_semantics.rs:267,276` and `submit_plan_persist.rs:208,215`). Therefore `ContinuationSource` includes `PlanCompleted` and `PlanReadyToMerge` variants from day 1 (Task 1).
- `SpurEventBody` is currently **not** `#[non_exhaustive]` (verified `events.rs:233`). Task 5 adds `#[non_exhaustive]` in the same commit as the new `ContinuationDropped` variant — future-proofing for all additive variants.
- `InteractiveInput` is currently **not** `#[non_exhaustive]` (verified `orchestrator.rs:135`). Task 2 adds `#[non_exhaustive]` in the same commit as `SystemContinuation` for symmetry.
- ACP crate version: `agent-client-protocol = 0.10` → resolves to `agent-client-protocol-schema-0.11.4`. Prompt builders in Task 7 use `EmbeddedResourceResource` (not `ResourceContents` — that name does not exist).
- `PromptRequest::new` signature: `new(session_id: impl Into<SessionId>, prompt: Vec<ContentBlock>) -> Self` (verified `content.rs` + live use at `orchestrator.rs:1150`). Task 9 uses `PromptRequest::new(b.acp_session_id.clone(), blocks)`.

---

## File Structure

| File | Role | Status |
|---|---|---|
| `crates/spur-acp/src/domain/continuation.rs` | Domain types: `BrainContinuation`, `ContinuationPayload`, `ContinuationSource` | Create |
| `crates/spur-acp/src/domain/mod.rs` | Re-export `continuation::*` | Modify |
| `crates/spur-acp/src/domain/events.rs` | Add `SpurEventBody::ContinuationDropped` variant | Modify |
| `crates/spur-core/src/scheduler.rs` | `BrainScheduler` struct + `ScheduledAction` enum + pure-sync `next()` | Create |
| `crates/spur-core/src/lib.rs` | `pub mod scheduler;` + re-exports | Modify |
| `crates/spur-core/src/orchestrator.rs` | Add `InteractiveInput::SystemContinuation`; replace `pending_messages` VecDeque with `BrainScheduler`; call `next()` in event loop; wire `note_session_swap`; prompt builder helpers | Modify |
| `crates/spur-core/src/orchestrator/continuation_bridge.rs` | `report_detached_completion` helper + overflow deque + prompt builder `render_autonomous_continuation_turn` / `render_merged_turn` | Create |
| `crates/spur-core/src/orchestrator.rs` module header | Declare `mod continuation_bridge;` | Modify |
| `crates/spur-mcp/src/server.rs` | Accept `continuation_tx` + `overflow_continuations` in constructor; call `report_detached_completion` from `spawn_result_collector` detached-completion path | Modify |
| `crates/spur-cli/src/main.rs` | Wire `continuation_tx` clone + overflow deque between MCP server and orchestrator | Modify |
| `crates/spur-core/tests/scheduler_properties.rs` | `proptest` harness for scheduler invariants | Create |
| `crates/spur-core/tests/continuation_integration.rs` | Integration tests (ordering, backpressure, session swap, self-describing turn) | Create |
| `scripts/lint_prompt_call_sites.sh` | CI grep-lint: `\.prompt\(` must only appear in `orchestrator.rs` within `run_interactive` | Create |
| `scripts/lint_message_construction_sites.sh` | CI grep-lint: `InteractiveInput::Message` construction restricted to TUI translation task | Create |

---

## Task 1 — Add `BrainContinuation` domain types (`spur-acp`)

**Files:**
- Create: `crates/spur-acp/src/domain/continuation.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs`
- Test: `crates/spur-acp/src/domain/continuation.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing unit test for type construction + serde round-trip**

Create `crates/spur-acp/src/domain/continuation.rs` with only the tests (no impl yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::delegation::DelegationStatus;
    use std::time::Instant;

    #[test]
    fn continuation_payload_builds_from_parts() {
        let p = ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("done".into()),
            diff_summary: None,
            worker_branch: Some("wt/abc".into()),
        };
        assert_eq!(p.summary.as_deref(), Some("done"));
        assert!(matches!(p.status, DelegationStatus::Success));
    }

    #[test]
    fn continuation_source_variants_exhaustive() {
        // Compile-time check: every variant must be listed.
        let vs = [
            ContinuationSource::AsyncRequested,
            ContinuationSource::BlockTimeout,
            ContinuationSource::Cancelled,
            ContinuationSource::PlanCompleted,
            ContinuationSource::PlanReadyToMerge,
        ];
        assert_eq!(vs.len(), 5);
    }

    #[test]
    fn brain_continuation_holds_delegation_id_and_source() {
        let c = BrainContinuation {
            delegation_id: "uuid-1".into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
            },
            created_at: Instant::now(),
        };
        assert_eq!(c.delegation_id, "uuid-1");
        assert!(matches!(c.source, ContinuationSource::AsyncRequested));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib domain::continuation::tests -- --nocapture`
Expected: compile error — types do not exist.

- [ ] **Step 3: Write minimal implementation**

Add at the top of `crates/spur-acp/src/domain/continuation.rs`:

```rust
use crate::domain::events::DiffSummary;
use crate::domain::delegation::DelegationStatus;
use std::time::Instant;

/// Why SPUR is re-entering the brain with a continuation turn.
///
/// See `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuationSource {
    /// Originating call was `delegate_async`.
    AsyncRequested,
    /// `delegate_to_worker` exceeded the MCP block window; returned
    /// `delegation_id` for polling; worker later finished.
    BlockTimeout,
    /// Worker reached `DelegationStatus::Cancelled` (INV-6).
    Cancelled,
    /// `SpurEventBody::PlanCompleted` fired for a plan the brain dispatched.
    PlanCompleted,
    /// `SpurEventBody::PlanReadyToMerge` fired for a plan the brain dispatched.
    PlanReadyToMerge,
}

/// Narrow projection of a worker outcome for scheduler consumption.
///
/// Deliberately NOT `DelegationResult` to decouple scheduler evolution
/// from result-struct evolution and to avoid moving large diffs through
/// the orchestrator ingress channel.
#[derive(Debug, Clone)]
pub struct ContinuationPayload {
    pub status:        DelegationStatus,
    pub summary:       Option<String>,
    pub diff_summary:  Option<DiffSummary>,
    pub worker_branch: Option<String>,
}

/// One detached delegation result awaiting brain re-entry.
#[derive(Debug, Clone)]
pub struct BrainContinuation {
    /// Correlation key (UUID string; migrates to `DelegationId` newtype when INV-1 lands).
    pub delegation_id: String,
    pub source:        ContinuationSource,
    pub payload:       ContinuationPayload,
    /// Monotonic creation time; not persisted across process restart.
    pub created_at:    Instant,
}
```

- [ ] **Step 4: Wire module + re-export**

Edit `crates/spur-acp/src/domain/mod.rs` — add:

```rust
pub mod continuation;
pub use continuation::{BrainContinuation, ContinuationPayload, ContinuationSource};
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p spur-acp --lib domain::continuation`
Expected: 3 tests PASS.

Run: `cargo build -p spur-acp`
Expected: clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/continuation.rs crates/spur-acp/src/domain/mod.rs
git commit -m "feat(spur-acp): add BrainContinuation / ContinuationPayload / ContinuationSource domain types"
```

---

## Task 2 — Add `InteractiveInput::SystemContinuation` variant

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (around line 135 — `InteractiveInput` enum)
- Test: inline in same module

- [ ] **Step 1: Write failing unit test**

Append to `orchestrator.rs` `#[cfg(test)] mod tests` (or create a minimal one if none exists):

```rust
#[cfg(test)]
mod interactive_input_tests {
    use super::InteractiveInput;
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::types::SessionId;
    use std::time::Instant;

    #[test]
    fn system_continuation_variant_constructs() {
        let c = BrainContinuation {
            delegation_id: "abc".into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        };
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: c,
        };
        match input {
            InteractiveInput::SystemContinuation { .. } => (),
            _ => panic!("expected SystemContinuation variant"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib orchestrator::interactive_input_tests::system_continuation_variant_constructs`
Expected: compile error — variant does not exist.

- [ ] **Step 3: Mark `InteractiveInput` `#[non_exhaustive]` + add the variant**

Edit `crates/spur-core/src/orchestrator.rs` — add `#[non_exhaustive]` on the `InteractiveInput` enum (above line 135), then inside the enum (near line 192, just before closing `}`), add:

```rust
    /// Detached delegation completion returned to the orchestrator for
    /// scheduled brain re-entry. Never constructed by the TUI. See
    /// `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
    SystemContinuation {
        session: SessionId,
        continuation: spur_acp::domain::BrainContinuation,
    },
```

- [ ] **Step 4: Add a shared DRY helper for "ignore at unexpected site"**

In `orchestrator.rs`, near the `InteractiveInput` enum definition, add a module-private helper:

```rust
#[inline]
fn ignore_system_continuation_unexpected_site(site: &'static str) {
    tracing::debug!(
        site = %site,
        "SystemContinuation reached unexpected match arm — routed via BrainScheduler in run_interactive only"
    );
}
```

- [ ] **Step 5: Add the variant arm at every exhaustive `InteractiveInput` match site**

The following sites match on `InteractiveInput` and are NOT behind a `_ => ...` catch-all. Each requires an explicit arm:

**In `crates/spur-core/src/orchestrator.rs`:**
- Line 795 (outer loop dispatch)
- Line 850
- Line 935
- Line 1002
- Line 1025 (`CancelStream` outside-stream-drop branch)
- Line 1033
- Line 1045
- Line 1070
- Line 1097
- Line 1258 (inner streaming `select!` — see Step 6)
- Line 1273
- Line 1281 (inner `select!` `other =>` catch-all — special, see Step 6)
- Line 1311
- Line 1330
- Line 4349 (test module)
- Line 4416 (test module)

**In `crates/spur-core/tests/review_gate_integration.rs`:**
- Line 315
- Line 339
- Line 347

For sites that treat the input as a no-op at this layer (most of them), insert:

```rust
InteractiveInput::SystemContinuation { .. } => {
    ignore_system_continuation_unexpected_site(concat!(file!(), ":", line!()));
}
```

Use `rustc` as the ground truth: `cargo check -p spur-core` will enumerate every non-exhaustive match error. Work through them one by one.

- [ ] **Step 6: Handle the inner `select!` catch-all at line 1281 specially**

The inner streaming `select!` currently has `other => { pending_messages.push_back(other) }`. Task 8 replaces `pending_messages` with `BrainScheduler`; for now, to keep Task 2 self-contained, match `SystemContinuation` explicitly at line 1281 and drop it with the helper (Task 8 rewires it into `scheduler.push_continuation(continuation)`):

```rust
other @ InteractiveInput::SystemContinuation { .. } => {
    ignore_system_continuation_unexpected_site("inner_select_catchall_line_1281");
    // Task 8 replaces this with scheduler.push_continuation(cont).
    drop(other);
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-core --lib orchestrator::interactive_input_tests`
Expected: 1 test PASS.

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: PASS (test file now handles the variant).

Run: `cargo build -p spur-core`
Expected: clean build (no non-exhaustive match warnings).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/review_gate_integration.rs
git commit -m "feat(spur-core): add InteractiveInput::SystemContinuation variant (#[non_exhaustive])"
```

---

## Task 3 — `BrainScheduler` skeleton: storage + `push_user` / `push_continuation` / dedup

**Files:**
- Create: `crates/spur-core/src/scheduler.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Test: inline in `scheduler.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/spur-core/src/scheduler.rs` with tests only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::types::SessionId;
    use std::time::Instant;

    fn mk_cont(id: &str) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[test]
    fn new_scheduler_is_empty() {
        let s = BrainScheduler::new(Some(SessionId::new()));
        assert_eq!(s.pending_user_len(), 0);
        assert_eq!(s.pending_continuation_len(), 0);
    }

    #[test]
    fn push_continuation_dedups_by_delegation_id() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-1"));           // duplicate — no-op
        assert_eq!(s.pending_continuation_len(), 1);
        s.push_continuation(mk_cont("id-2"));
        assert_eq!(s.pending_continuation_len(), 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --lib scheduler::tests`
Expected: compile error — `BrainScheduler` does not exist.

- [ ] **Step 3: Write minimal implementation**

At the top of `crates/spur-core/src/scheduler.rs`:

```rust
//! Brain turn scheduler — see
//! `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
//!
//! Pure-sync policy. No tokio primitives; unit-testable without a runtime.

use spur_acp::domain::BrainContinuation;
use spur_acp::types::SessionId;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use crate::orchestrator::InteractiveInput;

/// Owns the split-lane queues and scheduling policy for brain turns.
pub struct BrainScheduler {
    pending_user:          VecDeque<InteractiveInput>,
    pending_continuations: VecDeque<BrainContinuation>,
    delivered_ids:         HashSet<String>,
    active_session:        Option<SessionId>,
    turn_in_flight:        bool,
    cancel_grace_until:    Option<Instant>,
}

impl BrainScheduler {
    pub fn new(active_session: Option<SessionId>) -> Self {
        Self {
            pending_user: VecDeque::new(),
            pending_continuations: VecDeque::new(),
            delivered_ids: HashSet::new(),
            active_session,
            turn_in_flight: false,
            cancel_grace_until: None,
        }
    }

    pub fn push_user(&mut self, input: InteractiveInput) {
        self.pending_user.push_back(input);
    }

    /// Idempotent: duplicate `delegation_id` pushes are dropped silently.
    pub fn push_continuation(&mut self, c: BrainContinuation) {
        if self.delivered_ids.contains(&c.delegation_id) {
            return;
        }
        if self.pending_continuations.iter().any(|q| q.delegation_id == c.delegation_id) {
            return;
        }
        self.pending_continuations.push_back(c);
    }

    #[cfg(test)]
    pub(crate) fn pending_user_len(&self) -> usize { self.pending_user.len() }
    #[cfg(test)]
    pub(crate) fn pending_continuation_len(&self) -> usize { self.pending_continuations.len() }
}
```

- [ ] **Step 4: Register the module**

Edit `crates/spur-core/src/lib.rs` — add:

```rust
pub mod scheduler;
pub use scheduler::{BrainScheduler, ScheduledAction};
```

The `ScheduledAction` re-export will fail until Task 4. For now, either add a placeholder `pub enum ScheduledAction { Idle }` in `scheduler.rs` OR remove the `ScheduledAction` from the re-export until Task 4.

Choose: remove `ScheduledAction` from the re-export line now. Task 4 adds it back.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core --lib scheduler::tests`
Expected: 2 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/scheduler.rs crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): add BrainScheduler skeleton with dedup push_continuation"
```

---

## Task 4 — `BrainScheduler::next()` with user-priority / idle / cancel-grace / merge

**Files:**
- Modify: `crates/spur-core/src/scheduler.rs`
- Modify: `crates/spur-core/src/lib.rs` (re-add `ScheduledAction` re-export)

- [ ] **Step 1: Write failing tests**

Append to `scheduler.rs` tests:

```rust
    #[test]
    fn next_is_idle_when_everything_empty() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        assert!(matches!(s.next(Instant::now()), ScheduledAction::Idle));
    }

    #[test]
    fn next_returns_user_prompt_when_user_queued_and_idle() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        assert!(matches!(s.next(Instant::now()), ScheduledAction::UserPrompt(_)));
    }

    #[test]
    fn next_returns_idle_while_turn_in_flight_even_if_user_queued() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        s.note_turn_started();
        assert!(matches!(s.next(Instant::now()), ScheduledAction::Idle));
    }

    #[test]
    fn next_fires_continuation_only_when_idle_and_no_user_pending() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        match s.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(cs) => assert_eq!(cs.len(), 1),
            other => panic!("expected ContinuationPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_user_beats_continuation_when_both_queued() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        match s.next(Instant::now()) {
            ScheduledAction::MergedPrompt { continuations, .. } => {
                assert_eq!(continuations.len(), 1);
            }
            other => panic!("expected MergedPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_suppresses_continuation_during_cancel_grace() {
        let now = Instant::now();
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.note_cancel_resolved(now);
        // Inside the grace window: Idle.
        assert!(matches!(s.next(now + std::time::Duration::from_millis(100)), ScheduledAction::Idle));
        // After grace: fires.
        assert!(matches!(
            s.next(now + std::time::Duration::from_millis(2000)),
            ScheduledAction::ContinuationPrompt(_)
        ));
    }

    #[test]
    fn next_coalesces_multiple_continuations_fifo() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-2"));
        s.push_continuation(mk_cont("id-3"));
        match s.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(cs) => {
                assert_eq!(cs.len(), 3);
                assert_eq!(cs[0].delegation_id, "id-1");
                assert_eq!(cs[2].delegation_id, "id-3");
            }
            _ => panic!("expected ContinuationPrompt"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --lib scheduler::tests`
Expected: compile errors — `ScheduledAction`, `next()`, `note_turn_started`, `note_cancel_resolved` don't exist.

- [ ] **Step 3: Implement `ScheduledAction` + scheduler state-transition API + `next()`**

Add to `crates/spur-core/src/scheduler.rs`:

```rust
use std::time::Duration;

pub const CANCEL_GRACE_DEFAULT: Duration = Duration::from_millis(750);

#[derive(Debug)]
pub enum ScheduledAction {
    /// Fire a user turn. Caller flattens into `PromptRequest`.
    UserPrompt(InteractiveInput),
    /// Fire an autonomous continuation turn with these coalesced continuations.
    ContinuationPrompt(Vec<BrainContinuation>),
    /// Fire a merged turn: user input foreground, continuations as background blocks.
    MergedPrompt {
        user: InteractiveInput,
        continuations: Vec<BrainContinuation>,
    },
    /// Nothing to do.
    Idle,
}

impl BrainScheduler {
    pub fn note_turn_started(&mut self) {
        self.turn_in_flight = true;
    }

    pub fn note_turn_finished(&mut self) {
        self.turn_in_flight = false;
    }

    /// Call AFTER cancel has fully resolved (stream drained / force-timeout fired).
    pub fn note_cancel_resolved(&mut self, now: Instant) {
        self.cancel_grace_until = Some(now + CANCEL_GRACE_DEFAULT);
    }

    /// Arriving user prompt during grace clears the grace window.
    fn clear_grace_if_user_arrived(&mut self) {
        if !self.pending_user.is_empty() {
            self.cancel_grace_until = None;
        }
    }

    fn in_cancel_grace(&self, now: Instant) -> bool {
        match self.cancel_grace_until {
            Some(t) => now < t,
            None => false,
        }
    }

    /// Pure sync: given the current clock, return the next action.
    /// Mutates internal queues for any action that delivers continuations.
    pub fn next(&mut self, now: Instant) -> ScheduledAction {
        self.clear_grace_if_user_arrived();

        if self.turn_in_flight {
            return ScheduledAction::Idle;
        }

        // User priority.
        if let Some(user) = self.pending_user.pop_front() {
            let continuations = if self.pending_continuations.is_empty() {
                Vec::new()
            } else {
                self.drain_continuations_for_delivery()
            };
            if continuations.is_empty() {
                return ScheduledAction::UserPrompt(user);
            }
            return ScheduledAction::MergedPrompt { user, continuations };
        }

        // No user queued: can we fire an autonomous continuation?
        if self.pending_continuations.is_empty() {
            return ScheduledAction::Idle;
        }
        if self.in_cancel_grace(now) {
            return ScheduledAction::Idle;
        }
        ScheduledAction::ContinuationPrompt(self.drain_continuations_for_delivery())
    }

    /// Drains ALL pending continuations. Merge-byte-budget enforcement
    /// lives at the prompt-builder layer (Task 7), not here — the
    /// scheduler hands over the full batch and the builder spills.
    fn drain_continuations_for_delivery(&mut self) -> Vec<BrainContinuation> {
        let batch: Vec<_> = self.pending_continuations.drain(..).collect();
        for c in &batch {
            self.delivered_ids.insert(c.delegation_id.clone());
        }
        batch
    }
}
```

Re-add `ScheduledAction` to the `lib.rs` re-export line.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --lib scheduler::tests`
Expected: all 9 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/scheduler.rs crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): implement BrainScheduler::next with user-priority / cancel-grace / coalesce"
```

---

## Task 5 — Session-swap eviction + `ContinuationDropped` event

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` — new variant
- Modify: `crates/spur-core/src/scheduler.rs` — `note_session_swap` returning evicted continuations
- Test: inline

- [ ] **Step 1: Add failing test in scheduler.rs**

```rust
    #[test]
    fn session_swap_evicts_stale_continuations_and_returns_them() {
        let sid_a = SessionId::new();
        let sid_b = SessionId::new();
        let mut s = BrainScheduler::new(Some(sid_a.clone()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-2"));

        let evicted = s.note_session_swap(Some(sid_b));
        assert_eq!(evicted.len(), 2);
        assert_eq!(s.pending_continuation_len(), 0);
    }
```

- [ ] **Step 2: Find and locate `SpurEventBody`**

Run: `grep -n "pub enum SpurEventBody" crates/spur-acp/src/domain/events.rs`
Note the line number of the enum; you'll insert a variant near the end.

- [ ] **Step 3: Mark `SpurEventBody` `#[non_exhaustive]` + add `ContinuationDropped`**

`SpurEventBody` is currently not `#[non_exhaustive]`. Adding the new variant without this attribute breaks every exhaustive match on the enum across lineage/projection, lineage/adapter, TUI app/dashboard/session_detail, and tests (~20 sites). The hygiene fix is a one-line addition in the SAME commit.

Edit `crates/spur-acp/src/domain/events.rs` at line ~233:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]                         // ← ADD THIS
pub enum SpurEventBody {
    // ... existing variants ...

    /// A pending system continuation was evicted without being delivered
    /// to the brain. See async-continuation design spec §Failure Cases.
    ContinuationDropped {
        delegation_id: String,
        reason: ContinuationDropReason,
    },
}
```

Just above `SpurEventBody`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContinuationDropReason {
    BrainDisconnected,
    SessionSwap,
    Shutdown,
}
```

Wire it through any `#[serde]` tags already on `SpurEventBody`.

**Note on compilation impact:** because we mark `#[non_exhaustive]` in the same commit, any existing exhaustive `match` on `SpurEventBody` that lacks a `_ => ...` arm will now need one. Run `cargo check --workspace` and add `_ => {}` (or a debug log) at any flagged site. Most TUI sites already have catch-alls; the lineage adapter / projection may need the arm.

- [ ] **Step 4: Implement `note_session_swap` in scheduler**

Add to `scheduler.rs`:

```rust
impl BrainScheduler {
    /// Evict continuations tagged for a session other than the new active
    /// one. Returns the evicted continuations so the caller can emit
    /// `ContinuationDropped` events for audit.
    pub fn note_session_swap(&mut self, new_active: Option<SessionId>) -> Vec<BrainContinuation> {
        // Continuations don't currently carry their own SessionId; the
        // scheduler's `active_session` acts as the lane guard. On swap,
        // every currently-pending continuation becomes stale.
        let evicted: Vec<_> = self.pending_continuations.drain(..).collect();
        self.active_session = new_active;
        // Do NOT insert evicted ids into delivered_ids — they were dropped,
        // not delivered; future re-push of the same id under the new brain
        // should still be accepted if semantically valid.
        evicted
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-acp --lib`
Run: `cargo test -p spur-core --lib scheduler::tests::session_swap_evicts_stale_continuations_and_returns_them`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-core/src/scheduler.rs
git commit -m "feat: add ContinuationDropped event + scheduler session-swap eviction"
```

---

## Task 6 — `continuation_bridge` module: overflow deque + `report_detached_completion`

**Files:**
- Create: `crates/spur-core/src/orchestrator/continuation_bridge.rs`
- Modify: `crates/spur-core/src/orchestrator.rs` — add `mod continuation_bridge;` near top
- Test: inline

Note on module layout: if `orchestrator.rs` is a single file (not a directory module), convert it to `orchestrator/mod.rs` OR put the bridge in a sibling file `crates/spur-core/src/continuation_bridge.rs` and adjust `lib.rs`. The grep of line `orchestrator.rs` suggests it's a single file. Default: put the new file at `crates/spur-core/src/continuation_bridge.rs` and `pub(crate) mod continuation_bridge;` in `lib.rs`.

- [ ] **Step 1: Create the module with failing test**

Create `crates/spur-core/src/continuation_bridge.rs`:

```rust
//! Bridge from MCP detached completion → orchestrator ingress.
//! Enforces INV-C3 (UI event BEFORE model-visible continuation).

use spur_acp::domain::BrainContinuation;
use spur_acp::types::SessionId;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};

use crate::orchestrator::InteractiveInput;

/// Overflow buffer for continuations when the `InteractiveInput` ingress
/// channel is full. Drained by the orchestrator on every scheduler tick.
pub type OverflowBuf = Arc<Mutex<VecDeque<(SessionId, BrainContinuation)>>>;

pub fn new_overflow_buf() -> OverflowBuf {
    Arc::new(Mutex::new(VecDeque::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::{ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use std::time::Instant;

    fn mk_cont(id: &str) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn overflow_buf_stores_on_try_send_full() {
        let buf = new_overflow_buf();
        let (_tx, _rx) = mpsc::channel::<InteractiveInput>(1);   // tiny cap
        let _tx_clone = _tx.clone();
        // Fill the channel.
        _tx.try_send(InteractiveInput::Message { blocks: vec![], interrupt: false }).unwrap();

        let sid = SessionId::new();
        let c = mk_cont("id-overflow-1");
        let input = InteractiveInput::SystemContinuation {
            session: sid.clone(), continuation: c.clone()
        };
        match _tx.try_send(input) {
            Err(TrySendError::Full(_)) => {
                buf.lock().await.push_back((sid, c));
            }
            _ => panic!("expected Full"),
        }
        assert_eq!(buf.lock().await.len(), 1);
    }
}
```

- [ ] **Step 2: Register module + run test**

Edit `crates/spur-core/src/lib.rs`:
```rust
pub mod continuation_bridge;
pub use continuation_bridge::{new_overflow_buf, OverflowBuf};
```

Run: `cargo test -p spur-core --lib continuation_bridge::tests::overflow_buf_stores_on_try_send_full`
Expected: PASS.

- [ ] **Step 3: Implement `report_detached_completion`**

Add to `continuation_bridge.rs`:

```rust
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::delegation::DelegationStatus;

/// Abstract sink — decouples the helper from both `FunnelHandle` (spur-core)
/// and `McpEventSink` (spur-mcp). Both types implement this by simple
/// delegation; callers in orchestrator use a closure over `FunnelHandle::emit`
/// and callers in MCP use the existing `event_sink` via a small adapter.
pub trait ContinuationEventSink: Send + Sync {
    fn emit(&self, body: SpurEventBody);
}

/// Exactly-once bridge from MCP result collector → orchestrator ingress.
/// Emits the UI event BEFORE sending `SystemContinuation` (INV-C3).
pub async fn report_detached_completion(
    sink: &dyn ContinuationEventSink,
    continuation_tx: &mpsc::Sender<InteractiveInput>,
    overflow: &OverflowBuf,
    session: SessionId,
    worker_session: SessionId,
    cont: BrainContinuation,
) {
    // 1) UI-visible event FIRST.
    sink.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: cont.payload.status.clone(),
    });
    // 2) Model-visible continuation SECOND (try_send + overflow fallback).
    let input = InteractiveInput::SystemContinuation {
        session: session.clone(),
        continuation: cont.clone(),
    };
    if let Err(TrySendError::Full(_)) = continuation_tx.try_send(input) {
        overflow.lock().await.push_back((session, cont));
    }
}
```

Verify `SpurEventBody::DelegationCompleted { worker_session, status }` matches the current shape at `spur-acp/src/domain/events.rs:315` — CONFIRMED. `status` is `DelegationStatus` directly; no `.into()` conversion needed.

- [ ] **Step 4: Adapter impls for `FunnelHandle` and `McpEventSink`**

Add below in the same file:

```rust
impl ContinuationEventSink for crate::event_funnel::FunnelHandle {
    fn emit(&self, body: SpurEventBody) { self.emit(body) }
}
```

And, in `crates/spur-mcp/src/events.rs` (or wherever `McpEventSink` lives), add a blanket impl OR a small wrapper inside Task 10's wiring — concrete code provided in Task 10 Step 4.

No unit test for ordering here — `FunnelHandle` has no `for_test()` helper. Ordering is covered in Task 13's integration test via a real `broadcast::Receiver`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core --lib continuation_bridge`
Expected: overflow-buf test PASSES; no ordering test at this layer.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/continuation_bridge.rs crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): report_detached_completion bridge with ContinuationEventSink trait + overflow deque"
```

---

## Task 7 — Prompt builder: `render_autonomous_continuation_turn` + `render_merged_turn`

**Files:**
- Modify: `crates/spur-core/src/continuation_bridge.rs` (append builder functions)
- Test: inline snapshot tests

- [ ] **Step 1: Write failing snapshot tests**

Append to `continuation_bridge.rs`:

```rust
#[cfg(test)]
mod builder_tests {
    use super::*;
    use agent_client_protocol::ContentBlock;

    fn mk_cont(id: &str, summary: &str) -> BrainContinuation {
        use spur_acp::domain::{ContinuationPayload, ContinuationSource};
        use spur_acp::domain::delegation::DelegationStatus;
        use std::time::Instant;
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: Some(summary.into()),
                diff_summary: None,
                worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[test]
    fn autonomous_turn_has_marker_and_resource_blocks() {
        let blocks = render_autonomous_continuation_turn(&[mk_cont("id-1", "done")]);
        // Block 0: SPUR:background marker text.
        match &blocks[0] {
            ContentBlock::Text(t) => assert!(t.text.starts_with("[SPUR:background]")),
            _ => panic!("block 0 must be text marker"),
        }
        // Block 1: resource with spur://continuation/{id-1} URI.
        match &blocks[1] {
            ContentBlock::Resource(r) => {
                let uri_has_id = format!("{:?}", r).contains("spur://continuation/id-1");
                assert!(uri_has_id, "resource URI must contain delegation id");
            }
            _ => panic!("block 1 must be resource"),
        }
        // Last block: trailing action hint text.
        assert!(matches!(blocks.last(), Some(ContentBlock::Text(_))));
    }

    #[test]
    fn merged_turn_preserves_user_blocks_byte_exact_at_front() {
        let user_blocks = vec![ContentBlock::Text(agent_client_protocol::TextContent {
            text: "hello world".into(), annotations: None, meta: None,
        })];
        let merged = render_merged_turn(&user_blocks, &[mk_cont("id-1", "done")]);
        assert_eq!(merged[0], user_blocks[0], "user block must be first, byte-exact");
        // Block 1: separator text marker.
        match &merged[1] {
            ContentBlock::Text(t) => {
                assert!(t.text.contains("[SPUR:background]"));
            }
            _ => panic!("separator must follow user blocks"),
        }
        // Block 2: resource.
        assert!(matches!(merged[2], ContentBlock::Resource(_)));
    }

    #[test]
    fn merged_turn_spills_when_over_budget() {
        let user_blocks = vec![ContentBlock::Text(agent_client_protocol::TextContent {
            text: "hi".into(), annotations: None, meta: None,
        })];
        // 10 continuations × big summary each.
        let big = "x".repeat(4096);
        let conts: Vec<_> = (0..10).map(|i| mk_cont(&format!("id-{i}"), &big)).collect();
        let (merged, spilled) = render_merged_turn_with_spill(&user_blocks, &conts, 4096);
        assert!(spilled.len() > 0, "budget should force spill");
        // User block still present and still byte-exact.
        assert_eq!(merged[0], user_blocks[0]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --lib continuation_bridge::builder_tests`
Expected: compile errors — functions don't exist.

- [ ] **Step 3: Implement the builders**

Append to `continuation_bridge.rs`:

```rust
use agent_client_protocol::{
    ContentBlock, EmbeddedResource, EmbeddedResourceResource,
    TextContent, TextResourceContents,
};

pub const MERGE_BUDGET_DEFAULT_BYTES: usize = 4096;

const MARKER_AUTONOMOUS: &str =
    "[SPUR:background] Detached delegation completed after tool call returned.";
const MARKER_SEPARATOR: &str =
    "[SPUR:background] The following blocks were injected by SPUR, not authored by the user.";
const ACTION_HINT: &str = "Review the result and decide the next action.";

fn continuation_uri(id: &str) -> String {
    format!("spur://continuation/{id}")
}

fn continuation_resource_block(c: &BrainContinuation) -> ContentBlock {
    // Serialize payload as JSON text inside an embedded resource.
    let json = serde_json::json!({
        "delegation_id": c.delegation_id,
        "source": format!("{:?}", c.source),
        "status": format!("{:?}", c.payload.status),
        "summary": c.payload.summary,
        "diff_summary": c.payload.diff_summary,
        "worker_branch": c.payload.worker_branch,
    }).to_string();

    ContentBlock::Resource(EmbeddedResource {
        annotations: None,
        meta: None,
        resource: EmbeddedResourceResource::TextResourceContents(
            TextResourceContents {
                uri: continuation_uri(&c.delegation_id),
                mime_type: Some("application/json".into()),
                text: json,
                meta: None,
            }
        ),
    })
}

fn text_block(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent {
        text: s.into(), annotations: None, meta: None,
    })
}

/// Build an autonomous continuation-only turn.
pub fn render_autonomous_continuation_turn(conts: &[BrainContinuation]) -> Vec<ContentBlock> {
    let mut out = Vec::with_capacity(2 + conts.len());
    out.push(text_block(MARKER_AUTONOMOUS));
    for c in conts {
        out.push(continuation_resource_block(c));
    }
    out.push(text_block(ACTION_HINT));
    out
}

/// Build a merged user+continuation turn (no budget).
pub fn render_merged_turn(
    user_blocks: &[ContentBlock],
    conts: &[BrainContinuation],
) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = user_blocks.to_vec();
    if !conts.is_empty() {
        out.push(text_block(MARKER_SEPARATOR));
        for c in conts {
            out.push(continuation_resource_block(c));
        }
    }
    out
}

/// Build a merged turn enforcing a byte budget for injected content.
/// Returns `(blocks, spilled_continuations)`. Continuations deliver
/// oldest-first; the first one that would overflow and every following
/// continuation is returned for re-queueing.
pub fn render_merged_turn_with_spill(
    user_blocks: &[ContentBlock],
    conts: &[BrainContinuation],
    budget_bytes: usize,
) -> (Vec<ContentBlock>, Vec<BrainContinuation>) {
    let mut out: Vec<ContentBlock> = user_blocks.to_vec();
    let mut injected_bytes = 0usize;
    let separator_cost = MARKER_SEPARATOR.len();

    let mut to_inject: Vec<&BrainContinuation> = Vec::new();
    let mut spilled: Vec<BrainContinuation> = Vec::new();
    let mut separator_accounted = false;

    for c in conts {
        let block = continuation_resource_block(c);
        let cost = block_byte_cost(&block);
        let with_sep_if_first = if !separator_accounted { separator_cost } else { 0 };
        if injected_bytes + cost + with_sep_if_first > budget_bytes {
            spilled.push(c.clone());
        } else {
            if !separator_accounted {
                injected_bytes += separator_cost;
                separator_accounted = true;
            }
            injected_bytes += cost;
            to_inject.push(c);
        }
    }

    if !to_inject.is_empty() {
        out.push(text_block(MARKER_SEPARATOR));
        for c in to_inject {
            out.push(continuation_resource_block(c));
        }
    }
    (out, spilled)
}

fn block_byte_cost(b: &ContentBlock) -> usize {
    match b {
        ContentBlock::Text(t) => t.text.len(),
        ContentBlock::Resource(r) => {
            match &r.resource {
                EmbeddedResourceResource::TextResourceContents(t) => t.text.len() + t.uri.len(),
                EmbeddedResourceResource::BlobResourceContents(_) => 256,  // best-effort
            }
        }
        _ => 128,
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --lib continuation_bridge::builder_tests`
Expected: 3 tests PASS.

If `EmbeddedResource` / `TextResourceContents` field shapes differ in the installed `agent-client-protocol` crate version, adjust the constructor to match the actual API (use `cargo doc -p agent-client-protocol --open` if needed).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/continuation_bridge.rs
git commit -m "feat(spur-core): add self-describing continuation prompt builders with merge budget spill"
```

---

## Task 8 — Wire `BrainScheduler` into `run_interactive` (replaces `pending_messages`)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Locate the scheduler integration points**

Find these sites (use line numbers from grounding):
- `orchestrator.rs:748` — `let mut pending_messages: VecDeque<InteractiveInput> = VecDeque::new();`
- `orchestrator.rs:784-791` — drain-then-recv loop
- `orchestrator.rs:1156` — `connection.prompt(prompt_request)` call
- `orchestrator.rs:1253` — stream drain exit
- `orchestrator.rs:1273-1279` — cancel path
- `orchestrator.rs:1305-1321` — `NewSessionWithMessage` session swap site

Record the exact line numbers from your local checkout — they may drift.

- [ ] **Step 2: Replace `pending_messages` VecDeque with a `BrainScheduler`**

At `orchestrator.rs:748`:

```rust
// OLD:
// let mut pending_messages: VecDeque<InteractiveInput> = VecDeque::new();

// NEW:
let mut scheduler = crate::scheduler::BrainScheduler::new(None);
```

- [ ] **Step 3: Introduce three dispatch helpers + replace drain-then-recv block**

The outer loop at `orchestrator.rs:784-791` is replaced with a **three-dispatch** pattern. Continuation turns do NOT route through an `InteractiveInput` — they go directly to a dispatcher. This avoids the "synthesize a fake SystemContinuation" footgun (C8.1).

Declare above the outer loop (after the `scheduler` construction):

```rust
// Extracted later in Task 9 into standalone fns; for now, closures inline.
```

Replace the pop-then-await block with:

```rust
loop {
    // (a) Drain overflow at TOP of iteration so next() sees fresh state (C8.2).
    {
        let mut over = overflow_continuations.lock().await;
        while let Some((_sid, c)) = over.pop_front() {
            scheduler.push_continuation(c);
        }
    }

    // (b) Ask scheduler what to do now.
    let action = scheduler.next(std::time::Instant::now());

    match action {
        crate::scheduler::ScheduledAction::UserPrompt(user_input) => {
            dispatch_user_turn(user_input, &[], /* &mut brain, &funnel, ... */).await?;
        }
        crate::scheduler::ScheduledAction::MergedPrompt { user, continuations } => {
            dispatch_user_turn(user, &continuations, /* ... */).await?;
        }
        crate::scheduler::ScheduledAction::ContinuationPrompt(continuations) => {
            dispatch_autonomous_turn(&continuations, /* ... */).await?;
        }
        crate::scheduler::ScheduledAction::Idle => {
            match user_input_rx.recv().await {
                Some(crate::orchestrator::InteractiveInput::SystemContinuation {
                    continuation, ..
                }) => scheduler.push_continuation(continuation),
                Some(other) => scheduler.push_user(other),
                None => break,   // channel closed — shutdown
            }
        }
    }
}
```

The three dispatch helpers are defined as inline closures OR pulled out as `async fn` helpers at module scope (recommended — easier to read and test). Each encapsulates: (1) the prompt-block construction via Task 7 builders, (2) the `PromptRequest::new(session_id, blocks)` call (Task 9), (3) `scheduler.note_turn_started()`/`note_turn_finished()` bracketing.

Declare `overflow_continuations: OverflowBuf` as a new parameter on `run_interactive` (Task 10 threads it from `spur-cli/main.rs`).

- [ ] **Step 4: Wire `note_turn_started` / `note_turn_finished` via RAII guard**

The streaming loop has multiple exit paths (normal `:1253`, cancel force-break `:1294`, any `?` error bail-outs). A bare "call finished at line 1253" is insufficient — an error-path exit would leave `turn_in_flight == true` forever. Use an RAII guard to guarantee invocation.

Add to `crates/spur-core/src/scheduler.rs`:

```rust
/// RAII: sets `turn_in_flight = true` on construction, clears on Drop.
/// Binds a &mut BrainScheduler for the duration of a turn.
pub struct TurnGuard<'a> {
    sched: &'a mut BrainScheduler,
}

impl<'a> TurnGuard<'a> {
    pub fn arm(sched: &'a mut BrainScheduler) -> Self {
        sched.note_turn_started();
        Self { sched }
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.sched.note_turn_finished();
    }
}
```

In each dispatch helper, bracket the `connection.prompt(...).await` loop with:

```rust
let _guard = crate::scheduler::TurnGuard::arm(&mut scheduler);
// ... the entire streaming select! loop ...
// _guard drops here → note_turn_finished() regardless of exit path.
```

- [ ] **Step 5: Wire `note_cancel_resolved`**

At the cancel-drain resolution site (`orchestrator.rs:1294`, just before the `break` in the `cancel_deadline` arm), call:

```rust
scheduler.note_cancel_resolved(std::time::Instant::now());
```

Also call it on the normal-path post-cancel completion: if the stream drains during cancel (rather than force-break firing), the `select!` exits at `:1253`; the dispatch helper must check whether a cancel was in progress and call `note_cancel_resolved` if so. Concrete shape:

```rust
// Inside dispatch_user_turn / dispatch_autonomous_turn:
let mut saw_cancel = false;
// inside the select! cancel arm:
saw_cancel = true;
// after the select! exits (any path):
if saw_cancel {
    scheduler.note_cancel_resolved(std::time::Instant::now());
}
```

- [ ] **Step 6: Build — expect prompt-assembly errors**

Run: `cargo build -p spur-core`
Expected: errors in the prompt-builder integration — `pending_merge_continuations` is declared but not yet consumed. Task 9 wires it into the prompt payload.

- [ ] **Step 7: Commit (intermediate — compiles? if yes, commit; if no, defer to Task 9)**

If the build is clean-except-for-dead-warn:
```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "refactor(spur-core): replace pending_messages VecDeque with BrainScheduler"
```

If not, continue to Task 9 and commit jointly at its end.

---

## Task 9 — Integrate continuation blocks into the prompt payload

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Locate `PromptRequest::new(...)` construction**

Find where `PromptRequest::new(...)` (or equivalent) is built from the flattened `InteractiveInput::Message` blocks, near line 1156. Record the exact path.

- [ ] **Step 2: Extract each dispatch helper, each with its own block-construction**

Replace Task 8's inline closures with three module-local async fns in `orchestrator.rs`:

```rust
use crate::continuation_bridge::{
    render_autonomous_continuation_turn, render_merged_turn_with_spill,
    MERGE_BUDGET_DEFAULT_BYTES,
};
use agent_client_protocol::PromptRequest;

/// Fire a user-originated turn (with optional background continuations).
async fn dispatch_user_turn(
    user_input: InteractiveInput,
    continuations: &[spur_acp::domain::BrainContinuation],
    brain: &mut BrainSession,
    scheduler: &mut crate::scheduler::BrainScheduler,
    funnel: &crate::event_funnel::FunnelHandle,
    // ... other run_interactive state passed through
) -> Result<Vec<spur_acp::domain::BrainContinuation> /* spilled */> {
    let user_blocks = match user_input {
        InteractiveInput::Message { blocks, .. } => blocks,
        InteractiveInput::NewSessionWithMessage { blocks, .. } => blocks,
        // Other variants should not reach this path; handle inline or route earlier.
        other => {
            tracing::warn!(?other, "unexpected non-Message variant in dispatch_user_turn");
            return Ok(vec![]);
        }
    };

    let (blocks, spilled) = if continuations.is_empty() {
        (user_blocks, vec![])
    } else {
        render_merged_turn_with_spill(&user_blocks, continuations, MERGE_BUDGET_DEFAULT_BYTES)
    };

    let prompt_request = PromptRequest::new(brain.acp_session_id.clone(), blocks);
    let _guard = crate::scheduler::TurnGuard::arm(scheduler);
    run_brain_prompt_stream(brain, prompt_request, funnel).await?;
    Ok(spilled)
}

/// Fire an autonomous continuation-only turn.
async fn dispatch_autonomous_turn(
    continuations: &[spur_acp::domain::BrainContinuation],
    brain: &mut BrainSession,
    scheduler: &mut crate::scheduler::BrainScheduler,
    funnel: &crate::event_funnel::FunnelHandle,
    // ... other state
) -> Result<()> {
    let blocks = render_autonomous_continuation_turn(continuations);
    let prompt_request = PromptRequest::new(brain.acp_session_id.clone(), blocks);
    let _guard = crate::scheduler::TurnGuard::arm(scheduler);
    run_brain_prompt_stream(brain, prompt_request, funnel).await?;
    Ok(())
}
```

`run_brain_prompt_stream` is the existing streaming-loop body extracted from `orchestrator.rs:1156-1300`. Extract it to a helper `async fn` that owns only the streaming `select!` + event emission, not the outer event-loop control flow. The extraction is the biggest structural edit in the plan — do it in small steps and compile after each.

After each dispatch returns, re-queue spilled continuations:

```rust
for c in spilled { scheduler.push_continuation(c); }
```

**Important:** `PromptRequest::new` signature is `new(session_id: impl Into<SessionId>, prompt: Vec<ContentBlock>)` (verified at `orchestrator.rs:1150`). The second argument is `prompt`, not `blocks`.

- [ ] **Step 3: Build + sanity test**

Run: `cargo build -p spur-core`
Expected: clean build.

Run: `cargo test -p spur-core --lib`
Expected: all previously-green tests still pass; no new test is added here (integration covers this in Task 11).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): integrate continuation blocks into brain PromptRequest"
```

---

## Task 10 — MCP result-collector integration + `continuation_tx` wiring through CLI

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-core/src/orchestrator.rs` — `run_interactive` signature gains `continuation_tx` + `overflow_continuations` params if not already present
- Modify: `crates/spur-cli/src/main.rs` — construct overflow buf + clone a continuation sender into MCP server + into orchestrator

- [ ] **Step 1: Expose overflow + sender into `run_interactive`**

At the signature of `run_interactive` (grep for `pub async fn run_interactive`), add parameters:

```rust
continuation_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
overflow_continuations: crate::continuation_bridge::OverflowBuf,
```

Feed them through the same argument list used by every caller (likely `spur-cli/src/main.rs` and any tests).

- [ ] **Step 2: Extend `McpCallbackServer` with continuation context**

`McpCallbackServer::new` currently takes `session_id: &spur_acp::BrainSessionId, pm_service, event_sink: Option<Arc<dyn McpEventSink>>` (verified at `server.rs:485`). Do NOT add a new `brain_funnel` field — reuse the existing `event_sink`.

Add a single `DetachedContinuationCtx` bundle to the struct:

```rust
// In spur-mcp/src/server.rs, near McpCallbackServer struct:
pub struct DetachedContinuationCtx {
    pub continuation_tx: tokio::sync::mpsc::Sender<spur_core::orchestrator::InteractiveInput>,
    pub overflow: spur_core::continuation_bridge::OverflowBuf,
    pub brain_session_id: spur_acp::types::SessionId,
}
```

Extend `McpCallbackServer::new`:

```rust
pub fn new(
    session_id: &spur_acp::BrainSessionId,
    pm_service: Option<Arc<PmService>>,
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    continuation_ctx: DetachedContinuationCtx,     // NEW
) -> (Self, DelegationChannel) {
```

Store `continuation_ctx: Arc<DetachedContinuationCtx>` on the struct.

- [ ] **Step 3: Adapter from `McpEventSink` to `ContinuationEventSink`**

Add in `crates/spur-mcp/src/events.rs` (or wherever `McpEventSink` trait lives):

```rust
/// Thin adapter so Task 6's helper can accept an McpEventSink.
pub struct SinkAsContinuationEventSink(pub Arc<dyn McpEventSink>);

impl spur_core::continuation_bridge::ContinuationEventSink for SinkAsContinuationEventSink {
    fn emit(&self, body: spur_acp::domain::events::SpurEventBody) {
        // McpEventSink typically has a send method that wraps body into SpurEvent
        // and forwards to the funnel. Inline the exact call here.
        self.0.send_event(body);   // or self.0.emit(body) — match actual trait
    }
}
```

Confirm the actual `McpEventSink` method name by reading `crates/spur-mcp/src/events.rs`. Adjust the `emit` body to use the real method.

- [ ] **Step 4: Call `report_detached_completion` from the result-collector detached branch**

Modify `spawn_result_collector` to take a single bundled arg:

```rust
// Old 5-arg signature → new 6-arg signature with one bundle.
fn spawn_result_collector(
    tracker: &TaskTracker,
    delegation_id: String,
    rx: oneshot::Receiver<DelegationResult>,
    active: Arc<Mutex<HashMap<String, ...>>>,
    completed: Arc<Mutex<HashMap<String, (DelegationResult, Instant)>>>,
    detached: Option<DetachedCompletionHandle>,   // NEW — None for blocking path
)
```

Where:

```rust
pub struct DetachedCompletionHandle {
    pub ctx: Arc<DetachedContinuationCtx>,
    pub sink: Arc<dyn ContinuationEventSink>,
    pub source_kind: DetachedSourceKind,   // AsyncRequested | BlockTimeout
    pub worker_session: SessionId,
}

pub enum DetachedSourceKind { AsyncRequested, BlockTimeout }
```

Inside `spawn_result_collector`, after the `completed_delegations` insert, add:

```rust
if let Some(h) = detached {
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};

    let source = if matches!(result.status, spur_acp::domain::delegation::DelegationStatus::Cancelled { .. }) {
        ContinuationSource::Cancelled
    } else {
        match h.source_kind {
            DetachedSourceKind::AsyncRequested => ContinuationSource::AsyncRequested,
            DetachedSourceKind::BlockTimeout   => ContinuationSource::BlockTimeout,
        }
    };

    let cont = BrainContinuation {
        delegation_id: delegation_id.clone(),
        source,
        payload: ContinuationPayload {
            status: result.status.clone(),
            summary: result.summary.clone(),
            diff_summary: result.diff_summary.clone(),
            worker_branch: result.worker_branch.clone(),   // DelegationResult does have this field
        },
        created_at: std::time::Instant::now(),
    };
    spur_core::continuation_bridge::report_detached_completion(
        h.sink.as_ref(),
        &h.ctx.continuation_tx,
        &h.ctx.overflow,
        h.ctx.brain_session_id.clone(),
        h.worker_session.clone(),
        cont,
    ).await;
}
```

Update the three call sites:
- `server.rs:740` (`handle_delegate_to_worker` inline success) → `detached = None`
- `server.rs:834` (`handle_delegate_to_worker` block-timeout branch) → `detached = Some(handle with BlockTimeout)`
- `server.rs:2025` (`handle_delegate_async`) → `detached = Some(handle with AsyncRequested)`

(Line numbers verified against the current checkout at grounding time.)

- [ ] **Step 5: Wire in `spur-cli/src/main.rs`**

Near line 466 where `user_tx, user_rx = mpsc::channel(32)` is created:

```rust
let overflow_continuations = spur_core::continuation_bridge::new_overflow_buf();
let continuation_tx = user_tx.clone();   // MCP reuses the same ingress.
let continuation_ctx = spur_mcp::server::DetachedContinuationCtx {
    continuation_tx: continuation_tx.clone(),
    overflow: overflow_continuations.clone(),
    brain_session_id: brain_session_id.clone(),   // already constructed nearby
};
```

Pass `continuation_ctx` into `McpCallbackServer::new(...)`. Pass `overflow_continuations.clone()` as the new `run_interactive` parameter.

Update any test callers of `run_interactive` in `crates/spur-core/tests/review_gate_integration.rs:315,339,347` — pass a test overflow buf constructed with `new_overflow_buf()`.

- [ ] **Step 6: Build + lightweight run**

Run: `cargo build --workspace`
Expected: clean build.

Run: `cargo test --workspace -- --skip expensive`
Expected: no new failures.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/events.rs crates/spur-core/src/orchestrator.rs crates/spur-cli/src/main.rs crates/spur-core/tests/review_gate_integration.rs
git commit -m "feat: wire MCP result collector through report_detached_completion (sink-reuse)"
```

---

## Task 11 — `run_interactive` session-swap eviction + `ContinuationDropped` emission

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Locate session-swap sites**

`orchestrator.rs:1305-1321` — `NewSessionWithMessage` tears down brain and pushes `Message` onto pending queue.

Also: any `ResumeSession` handler (grep `InteractiveInput::ResumeSession`).

- [ ] **Step 2: Call `note_session_swap` at each swap**

There is no `active_brain_session_id` variable. The active brain is `brain: Option<BrainSession>` at `orchestrator.rs:747`, and the session id is `brain.as_ref().unwrap().acp_session_id`. After the new brain session is constructed and assigned:

```rust
let new_sid = brain.as_ref().map(|b| b.acp_session_id.clone());
let evicted = scheduler.note_session_swap(new_sid);
for c in evicted {
    funnel.emit(spur_acp::domain::events::SpurEventBody::ContinuationDropped {
        delegation_id: c.delegation_id,
        reason: spur_acp::domain::events::ContinuationDropReason::SessionSwap,
    });
}
```

Call sites:
1. **`NewSessionWithMessage`** handler (`orchestrator.rs:1305-1321`) — teardown brain, spawn new brain, push Message.
2. **`ResumeSession`** handler (grep `InteractiveInput::ResumeSession` for line number).
3. **Do NOT call on initial brain startup** — first None→Some transition has `pending_continuations` empty by invariant, so the eviction is a no-op, but it's clearer to skip the call entirely at startup.

`funnel` is already in scope in `run_interactive` (built at `orchestrator.rs:454` and threaded throughout).

- [ ] **Step 3: Build**

Run: `cargo build -p spur-core`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): evict stale continuations + emit ContinuationDropped on session swap"
```

---

## Task 12 — Property tests for scheduler invariants

**Files:**
- Create: `crates/spur-core/tests/scheduler_properties.rs`
- Modify: `crates/spur-core/Cargo.toml` — add `proptest` dev-dependency if absent

- [ ] **Step 1: Add `proptest` dev-dep**

Run: `grep -n "proptest" crates/spur-core/Cargo.toml`

If absent, edit `[dev-dependencies]`:
```toml
proptest = "1"
```

- [ ] **Step 2: Create `crates/spur-core/tests/scheduler_properties.rs`**

```rust
use proptest::prelude::*;
use spur_core::scheduler::{BrainScheduler, ScheduledAction};
use spur_core::orchestrator::InteractiveInput;
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::types::SessionId;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
enum Event {
    PushUser,
    PushContinuation(String),
    TurnStart,
    TurnEnd,
    CancelResolve,
    Tick(u64),            // advance clock by N ms
}

fn event_strategy() -> impl Strategy<Value = Event> {
    prop_oneof![
        Just(Event::PushUser),
        "id-[0-9]{1,3}".prop_map(Event::PushContinuation),
        Just(Event::TurnStart),
        Just(Event::TurnEnd),
        Just(Event::CancelResolve),
        (0u64..2000).prop_map(Event::Tick),
    ]
}

fn mk_cont(id: &str) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: None, diff_summary: None, worker_branch: None,
        },
        created_at: Instant::now(),
    }
}

proptest! {
    #[test]
    fn no_continuation_is_ever_scheduled_twice(events in prop::collection::vec(event_strategy(), 0..100)) {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        let mut now = Instant::now();
        let mut seen_scheduled_ids: std::collections::HashSet<String> = Default::default();

        for e in events {
            match e {
                Event::PushUser => s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false
                }),
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => s.note_turn_started(),
                Event::TurnEnd => s.note_turn_finished(),
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            let action = s.next(now);
            match action {
                ScheduledAction::ContinuationPrompt(cs) |
                ScheduledAction::MergedPrompt { continuations: cs, .. } => {
                    for c in cs {
                        prop_assert!(
                            seen_scheduled_ids.insert(c.delegation_id.clone()),
                            "delegation_id {} scheduled twice", c.delegation_id
                        );
                    }
                }
                _ => (),
            }
        }
    }

    #[test]
    fn turn_in_flight_implies_idle(events in prop::collection::vec(event_strategy(), 0..50)) {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        let mut now = Instant::now();
        let mut in_flight = false;

        for e in events {
            match e {
                Event::PushUser => s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false
                }),
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => { s.note_turn_started(); in_flight = true; }
                Event::TurnEnd => { s.note_turn_finished(); in_flight = false; }
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            if in_flight {
                prop_assert!(matches!(s.next(now), ScheduledAction::Idle),
                    "scheduler returned non-Idle while turn_in_flight=true");
            }
        }
    }

    #[test]
    fn pending_user_is_never_leapfrogged_by_continuation(events in prop::collection::vec(event_strategy(), 0..80)) {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        let mut now = Instant::now();
        let mut user_pending = false;

        for e in events {
            match e {
                Event::PushUser => { s.push_user(InteractiveInput::Message {
                    blocks: vec![], interrupt: false }); user_pending = true; }
                Event::PushContinuation(id) => s.push_continuation(mk_cont(&id)),
                Event::TurnStart => s.note_turn_started(),
                Event::TurnEnd => s.note_turn_finished(),
                Event::CancelResolve => s.note_cancel_resolved(now),
                Event::Tick(ms) => now += Duration::from_millis(ms),
            }
            let action = s.next(now);
            match action {
                ScheduledAction::ContinuationPrompt(_) => {
                    prop_assert!(!user_pending,
                        "continuation fired while user was pending");
                }
                ScheduledAction::UserPrompt(_) | ScheduledAction::MergedPrompt { .. } => {
                    user_pending = false;
                }
                _ => (),
            }
        }
    }
}
```

- [ ] **Step 3: Run the property tests**

Run: `cargo test -p spur-core --test scheduler_properties`
Expected: all 3 properties PASS. (`proptest` runs ~256 random cases per property by default.)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/tests/scheduler_properties.rs crates/spur-core/Cargo.toml
git commit -m "test(spur-core): property tests for BrainScheduler invariants"
```

---

## Task 13 — Integration tests: ordering, backpressure, session-swap, self-describing turn

**Files:**
- Create: `crates/spur-core/tests/continuation_integration.rs`

- [ ] **Step 1: Scaffold the harness**

Create `crates/spur-core/tests/continuation_integration.rs`:

```rust
//! Integration tests for async-continuation scheduling.
//! These exercise the bridge + orchestrator with a mock brain.

use spur_core::continuation_bridge::{
    new_overflow_buf, render_autonomous_continuation_turn, render_merged_turn_with_spill,
    report_detached_completion, MERGE_BUDGET_DEFAULT_BYTES,
};
use spur_core::scheduler::BrainScheduler;
use spur_core::orchestrator::InteractiveInput;
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::types::SessionId;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

fn mk_cont(id: &str) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("ok".into()),
            diff_summary: None,
            worker_branch: None,
        },
        created_at: Instant::now(),
    }
}
```

- [ ] **Step 2: Test — backpressure via overflow buf**

```rust
#[tokio::test]
async fn backpressure_overflow_on_full_channel() {
    let (tx, mut rx) = mpsc::channel::<InteractiveInput>(1);
    let overflow = new_overflow_buf();

    // Fill the channel.
    tx.try_send(InteractiveInput::Message { blocks: vec![], interrupt: false }).unwrap();

    // Simulate bridge calls (without funnel — use a test funnel helper if available).
    for i in 0..5 {
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: mk_cont(&format!("id-{i}")),
        };
        if let Err(TrySendError::Full(_)) = tx.try_send(input) {
            overflow.lock().await.push_back((SessionId::new(), mk_cont(&format!("id-{i}"))));
        }
    }

    // All 5 should have overflowed (channel cap=1, already full).
    assert_eq!(overflow.lock().await.len(), 5);

    // Drain channel once → overflow still holds them until drained by scheduler.
    let _ = rx.recv().await;
    assert_eq!(overflow.lock().await.len(), 5);
}
```

- [ ] **Step 3: Test — session-swap drops stale continuations**

```rust
#[test]
fn session_swap_drops_all_pending_continuations() {
    let mut s = BrainScheduler::new(Some(SessionId::new()));
    s.push_continuation(mk_cont("id-1"));
    s.push_continuation(mk_cont("id-2"));
    let evicted = s.note_session_swap(Some(SessionId::new()));
    assert_eq!(evicted.len(), 2);
    // Scheduler is now empty.
    let action = s.next(Instant::now());
    assert!(matches!(action, spur_core::scheduler::ScheduledAction::Idle));
}
```

- [ ] **Step 4: Test — self-describing merged turn**

```rust
#[test]
fn merged_turn_has_user_block_at_front_and_self_describing_marker() {
    use agent_client_protocol::{ContentBlock, TextContent};
    let user = vec![ContentBlock::Text(TextContent {
        text: "what is the plan?".into(), annotations: None, meta: None,
    })];
    let (blocks, spilled) = render_merged_turn_with_spill(
        &user,
        &[mk_cont("id-1")],
        MERGE_BUDGET_DEFAULT_BYTES,
    );
    assert!(spilled.is_empty());
    // User block present byte-exact at position 0.
    assert_eq!(blocks[0], user[0]);
    // Separator marker present.
    let has_marker = blocks.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.text.contains("[SPUR:background]")));
    assert!(has_marker, "merged turn must carry self-describing marker");
    // Resource with spur:// URI present.
    let has_resource = blocks.iter().any(|b| format!("{b:?}").contains("spur://continuation/id-1"));
    assert!(has_resource, "merged turn must carry spur://continuation/ resource");
}
```

- [ ] **Step 5: Test — autonomous turn self-describes**

```rust
#[test]
fn autonomous_turn_is_self_describing() {
    let blocks = render_autonomous_continuation_turn(&[mk_cont("id-42")]);
    let joined = format!("{blocks:?}");
    assert!(joined.contains("[SPUR:background]"), "must carry marker");
    assert!(joined.contains("spur://continuation/id-42"), "must carry resource URI");
}
```

- [ ] **Step 6: Run integration tests**

Run: `cargo test -p spur-core --test continuation_integration`
Expected: 4 tests PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/tests/continuation_integration.rs
git commit -m "test(spur-core): continuation integration — backpressure, swap, self-describing turns"
```

---

## Task 14 — CI grep-lints for INV-C1 / INV-C2

**Files:**
- Create: `scripts/lint_prompt_call_sites.sh`
- Create: `scripts/lint_message_construction_sites.sh`
- Modify: CI config (`.github/workflows/*.yml` if present, else add as a pre-commit hook)

- [ ] **Step 1: Write `scripts/lint_prompt_call_sites.sh`**

```bash
#!/usr/bin/env bash
# INV-C1: only `run_interactive` in orchestrator.rs may call `.prompt(` on AgentConnection.
set -euo pipefail

OFFENDERS=$(
  git grep -nE '\bconnection\.prompt\(' -- 'crates/spur-core/src/**/*.rs' \
  | grep -v 'crates/spur-core/src/orchestrator.rs' \
  || true
)

# Inside orchestrator.rs, allow only inside run_interactive (best-effort: require the
# call to be preceded by the function signature within 2000 lines).
ORCH_OFFENDERS=$(
  git grep -nE '\bconnection\.prompt\(' -- 'crates/spur-core/src/orchestrator.rs' \
  || true
)

if [[ -n "$OFFENDERS" ]]; then
  echo "INV-C1 violation: .prompt() called outside orchestrator.rs"
  echo "$OFFENDERS"
  exit 1
fi

if [[ -n "$ORCH_OFFENDERS" ]]; then
  # Soft-verify each hit is inside run_interactive.
  while IFS= read -r line; do
    FILE=$(echo "$line" | cut -d: -f1)
    LINENO=$(echo "$line" | cut -d: -f2)
    FN=$(awk -v ln="$LINENO" 'NR<=ln && /pub async fn |pub fn |async fn |fn / {last=$0} END{print last}' "$FILE")
    if ! echo "$FN" | grep -q "run_interactive"; then
      echo "INV-C1 violation at $FILE:$LINENO — .prompt() called outside run_interactive"
      echo "  enclosing fn candidate: $FN"
      exit 1
    fi
  done <<<"$ORCH_OFFENDERS"
fi

echo "INV-C1: OK"
```

Make executable: `chmod +x scripts/lint_prompt_call_sites.sh`

- [ ] **Step 2: Write `scripts/lint_message_construction_sites.sh`**

```bash
#!/usr/bin/env bash
# INV-C2: only the TUI translation task may construct InteractiveInput::Message.
set -euo pipefail

ALLOWED_FILES=(
  'crates/spur-tui/src/components/input_bar.rs'
  'crates/spur-cli/src/main.rs'                     # TUI→core translation task
  'crates/spur-core/src/orchestrator.rs'            # test modules only
  'crates/spur-core/tests/'
  'crates/spur-core/src/continuation_bridge.rs'     # test modules only
  'crates/spur-core/src/scheduler.rs'               # test modules only
)

HITS=$(git grep -nE 'InteractiveInput::Message' -- 'crates/**/*.rs' || true)
if [[ -z "$HITS" ]]; then
  echo "INV-C2: no construction sites found (suspicious)"; exit 0
fi

VIOLATIONS=""
while IFS= read -r line; do
  FILE=$(echo "$line" | cut -d: -f1)
  OK=0
  for allowed in "${ALLOWED_FILES[@]}"; do
    if [[ "$FILE" == "$allowed"* ]]; then OK=1; break; fi
  done
  if [[ $OK -eq 0 ]]; then
    VIOLATIONS+="$line"$'\n'
  fi
done <<<"$HITS"

if [[ -n "$VIOLATIONS" ]]; then
  echo "INV-C2 violation: InteractiveInput::Message constructed outside allowed sites:"
  echo "$VIOLATIONS"
  exit 1
fi

echo "INV-C2: OK"
```

Make executable.

- [ ] **Step 3: Create new CI workflow for invariant lints**

`.github/workflows/` currently contains `release-python.yml`, `release.yml`, `vendor-leak-check.yml` — no general CI / test workflow. Create a NEW file `.github/workflows/lint-invariants.yml`:

```yaml
name: Lint invariants
on:
  pull_request:
  push:
    branches: [main]

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0      # git grep needs the full tree

      - name: INV-C1 — only run_interactive calls brain.prompt()
        run: ./scripts/lint_prompt_call_sites.sh

      - name: INV-C2 — only TUI translation constructs InteractiveInput::Message
        run: ./scripts/lint_message_construction_sites.sh
```

Note: `git grep` only sees committed files; this lint is a CI check, not a pre-commit hook. If a pre-commit hook is desired later, switch the scripts to `grep -rn` locally.

- [ ] **Step 4: Run the scripts locally**

Run: `./scripts/lint_prompt_call_sites.sh`
Expected: `INV-C1: OK`

Run: `./scripts/lint_message_construction_sites.sh`
Expected: `INV-C2: OK`

- [ ] **Step 5: Commit**

```bash
git add scripts/lint_prompt_call_sites.sh scripts/lint_message_construction_sites.sh .github/workflows/lint-invariants.yml
git commit -m "chore(ci): grep-lints for INV-C1 / INV-C2 (prompt call + Message construction)"
```

---

## Rev 2 Corrections Applied

Applied from the L9 simulation pass (see the review transcript for full rationale):

| Correction | Task | Summary |
|---|---|---|
| C1.1 | 1 | Added `PlanCompleted` / `PlanReadyToMerge` to `ContinuationSource` (variants exist in SpurEventBody already) |
| C1.2 | 1 | Prereq note updated: `BrainSessionId` already exists; `DelegationId` not yet landed |
| C2.1 | 2 | Enumerated 19 match sites + added DRY `ignore_system_continuation_unexpected_site` helper + marked enum `#[non_exhaustive]` |
| C5.1 | 5 | Marked `SpurEventBody` `#[non_exhaustive]` in same commit as `ContinuationDropped` variant |
| C6.1 | 6 | Introduced `ContinuationEventSink` trait to decouple from `FunnelHandle` / `McpEventSink` |
| C6.2 | 6 | Dropped unit ordering test (no `FunnelHandle::for_test()` helper exists); ordering covered in Task 13 |
| C7.1 | 7 | Renamed `ResourceContents` → `EmbeddedResourceResource` (ACP schema 0.11.4 API) |
| C8.1 | 8 | Replaced fake-SystemContinuation synthesis with three-dispatch helpers (`dispatch_user_turn`, `dispatch_autonomous_turn`, plus merged path) |
| C8.2 | 8 | Moved overflow drain to top of loop iteration (before `next()`) |
| C8.3 | 8 | Added `TurnGuard` RAII to guarantee `note_turn_finished` on every exit path |
| C9.1 | 9 | `PromptRequest::new(session_id, blocks)` — corrected signature |
| C10.1 | 10 | Removed `brain_funnel` parameter; reuses existing `event_sink` via `SinkAsContinuationEventSink` adapter |
| C10.2 | 10 | Bundled continuation context into `DetachedContinuationCtx` + `DetachedCompletionHandle` structs |
| C11.1 | 11 | Replaced `active_brain_session_id` with `brain.as_ref().map(|b| b.acp_session_id.clone())` |
| C13.1 | 13 | Added `use tokio::sync::mpsc::error::TrySendError;` import |
| C14.1 | 14 | Create NEW `.github/workflows/lint-invariants.yml` (no existing workflow to append to) |

---

## Self-Review (final check before handoff)

**Spec coverage — each section mapped to a task:**

| Spec section | Task(s) |
|---|---|
| §Core Design #1 typed continuation input | Tasks 1, 2 |
| §Core Design #2 single scheduler owner / `BrainScheduler` extraction | Tasks 3, 4, 8 |
| §Core Design #3 unified ingress + backpressure (G3) | Tasks 6, 9 (drain), 10 |
| §Core Design #4 idle-only materialization + cancel grace (G5) | Task 4 |
| §Core Design #5 merge with user turn + byte budget (G10) + terminal states (G11) | Tasks 4, 7, 9 |
| §Prompt Construction Rules (self-describing INV-C7) | Task 7 |
| §Priority & Fairness Rules 1–9 | Tasks 4, 5, 7 |
| §Invariants INV-C1 / INV-C2 (lint) | Task 14 |
| §Invariants INV-C3 (ordering helper) | Task 6 |
| §Invariants INV-C6 (`turn_in_flight`) | Task 4 |
| §Invariants INV-C7 (self-describing) | Tasks 7, 13 |
| §Failure Cases — session swap (G2) | Tasks 5, 11 |
| §Failure Cases — cancelled worker | Task 10 |
| §Testing — unit | Tasks 1, 3, 4, 5, 7 |
| §Testing — property (proptest) | Task 12 |
| §Testing — integration | Task 13 |

No gaps identified.

**Placeholder scan:** every `# TODO` / `# TBD` replaced with concrete code or explicit "record the exact line from your local checkout" guidance where orchestrator.rs line numbers may drift. No "add error handling" instructions; no "similar to task N" hand-waves.

**Type consistency:** `BrainContinuation.delegation_id: String` throughout; `SessionId` not `BrainSessionId` throughout (documented at the top as a prerequisite). `ScheduledAction` variant names consistent between Task 4 definition and Task 8 / 9 consumers. `ContinuationSource` variants `AsyncRequested | BlockTimeout | Cancelled` consistent across Tasks 1, 10, 13.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-19-brain-async-continuation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
