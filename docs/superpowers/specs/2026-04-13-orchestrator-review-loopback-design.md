# Orchestrator Review Loopback — Design

**Date:** 2026-04-13
**Status:** Proposed for review
**Owner:** spur-core / spur-acp (consumer update in spur-cli + spur-tui)
**Follows:** `2026-04-13-executor-lineage-visualization-design.md`, `2026-04-13-lineage-event-contract-hardening-design.md`

## Goal

Close the observe → review → next-execute loop. When the brain delegates
a task to a worker, insert an optional human-review gate between worker
completion and the tool-call result the brain receives. The human's
`ReviewDecision` is translated into a `DelegationStatus` variant that
carries the right signal back to the brain.

## Non-goals

- Mid-flight worker interrupt (killing a running worker before it
  completes). Review happens *after* worker completion in v1.
- Review on `Conflict` or `Timeout` worker-outcomes. v1 gates only
  completion; conflicts/timeouts stay auto-failure.
- Per-task (not per-agent) review flags. v1 configures review at agent
  level; task-level override is future work.
- TUI-side policy overrides (e.g., "always review for this session
  regardless of config"). Future work.
- Crash durability. If the orchestrator crashes mid-review, the
  in-flight delegation is lost. Real durability requires persistence
  (Temporal-style); out of scope.

## Motivation

The `UserInput::SubmitReview` stub in `spur-cli/src/main.rs` has sat
unwired since the executor-lineage TUI landed. Without the
orchestrator side, TUI review decisions go nowhere — the whole
observe→review→execute loop is half-built. This spec wires it.

## Industry grounding

Research round (2026-04-13) examined LangGraph, CrewAI, AutoGen,
Temporal, Argo, n8n. Three convergent patterns and one clear
reference:

**The reference: LangGraph's `interrupt()`-inside-the-tool pattern.**
The tool call stays blocked; the human's decision resumes it with a
typed payload; that payload is translated into the tool's return
value the agent sees. Spur's Rust async + `oneshot` channel gives
strictly better continuation semantics than LangGraph's Python
implementation (no replay needed — the await point is a real
continuation).

**Pattern divergences matter:**
- **Retry ownership** splits: Temporal and CrewAI internalize retries
  in the orchestrator (one logical call, many worker spawns); AutoGen
  and LangGraph-default surface rejection to the agent. Research's
  recommendation and adopted position: **split the two** — internal
  loop for `Retry`, surface for `Reject`. Rationale: `Retry` is "same
  task, refined"; `Reject` is a signal the agent needs to reason
  about.
- **Tool-result translation** has three shapes: synthetic result,
  new conversational turn, or real-result gated. Research's adopted
  position: **synthetic tool result with typed status variants** —
  the brain sees one `DelegationResult` whose `status` already
  encodes the human's decision.

## Architecture

### Insertion point

`crates/spur-core/src/orchestrator.rs`, inside the `delegate` path
around line 1594 — between the moment the worker produces a
candidate `DelegationResult` and the `event_tx.send(DelegationCompleted)`
+ `oneshot.send(result)` that would normally unblock the brain.

When `agent_config.review_required == true`, the orchestrator
pauses there, awaits a `ReviewDecision`, shapes the final
`DelegationResult`, then completes the rendezvous.

### Flow

```
┌─ orchestrator::delegate_to_worker ─────────────────────────────────┐
│                                                                    │
│   spawn worker in worktree                                         │
│   collect candidate DelegationResult { status, diff, summary }     │
│                                                                    │
│   if agent_config.review_required:                                 │
│     executor_id = <from lineage>                                   │
│     (tx, rx) = oneshot::channel::<ReviewDecision>()                │
│     pending_reviews.insert(executor_id, tx)                        │
│     emit ExecutorReviewRequested { id, kind: Completion, payload } │
│     emit ExecutorPhaseChanged { id, AwaitingReview }               │
│                                                                    │
│     decision = select! {                                           │
│       Ok(d) = rx => d,                                             │
│       _ = sleep(review_timeout) => review_timeout_default,         │
│     }                                                              │
│                                                                    │
│     emit ExecutorReviewResolved { id, decision }                   │
│                                                                    │
│     match decision {                                               │
│       Approve  => keep candidate                                   │
│       Reject{r} => status = Rejected{r}, preserve worktree         │
│       Modify{n} => status = Modified{n}, keep diff+summary         │
│       Retry{c} => despawn+respawn, attempt_n += 1,                 │
│                   loop if under max_review_retries                 │
│     }                                                              │
│                                                                    │
│   emit DelegationCompleted                                         │
│   oneshot.send(final DelegationResult)  ← brain's tool call unblocks│
└────────────────────────────────────────────────────────────────────┘
```

### Three units, one interface each

**Unit 1 — Expanded `DelegationStatus` enum** (modify `spur-acp::domain::delegation`)
- *Does:* encodes all possible outcomes of a delegation — worker-only
  outcomes (Success, Failed, Conflict, Timeout) plus review outcomes
  (Rejected, Modified, Abandoned).
- *Marked `#[non_exhaustive]`* so external crates can add catch-alls
  without recompile-breakage on future additions.

**Unit 2 — Pending-reviews routing** (new field on `Orchestrator`)
- *Does:* owns `pending_reviews: Arc<Mutex<HashMap<ExecutorId, oneshot::Sender<ReviewDecision>>>>`. The delegation task registers the sender; a separate user-input dispatcher pops+sends when `UserInput::SubmitReview` arrives.
- *Depends on:* `tokio::sync::{oneshot, Mutex}`, `spur_core::ExecutorId`.
- *Testability:* pure state container; unit tests with synthetic decisions.

**Unit 3 — User-input dispatcher task** (new top-level task in `Orchestrator::run`)
- *Does:* reads from a new `mpsc::Receiver<UserInput>` owned by the orchestrator; on `SubmitReview { executor_id, decision }`, looks up the oneshot in `pending_reviews` and forwards the decision.
- *Spawned at orchestrator startup; runs for the orchestrator's lifetime.*
- *Depends on:* Unit 2, `spur_tui::UserInput`.

### Decision → DelegationStatus mapping

| ReviewDecision | DelegationStatus | DelegationResult.diff | DelegationResult.summary | Worktree |
|---|---|---|---|---|
| `Approve` | `Success` | worker's diff | worker's summary | removed |
| `Reject { reason }` | `Rejected { reason }` | worker's diff (preserved) | worker's summary | **preserved for inspection** |
| `Modify { note }` | `Modified { reviewer_note: note }` | worker's diff | worker's summary | removed |
| `Retry { new_constraints }` | *(not a terminal decision — orchestrator re-enters the delegation loop)* | final iteration's diff | final iteration's summary | reused across attempts |

**Retry terminal behavior:** Retry is not a terminal decision; the
final `DelegationStatus` is determined by whichever non-Retry
decision ends the loop (or by the retry-limit backstop). Example:
Retry → Retry → Approve produces `Success`. Retry × 4 when
`max_review_retries = 3` produces
`Failed { error: "retry limit exceeded after 3 attempts" }`.

**Review-timeout with `ReviewTimeoutAction::Abandon`** produces
`DelegationStatus::Abandoned { waited_for: review_timeout }`.
With `ReviewTimeoutAction::Reject { reason }` it produces
`Rejected { reason }`. With `ReviewTimeoutAction::Approve` it
produces `Success`.

**Review-timeout default:** `Rejected { reason: "review timeout" }`. This
is the safer default — treats no-response as no. Users can configure
`review_timeout_default` to a different `ReviewDecision` if their
workflow wants auto-approve on timeout.

**`Abandoned { waited_for }`** fires only when config explicitly sets
`review_timeout_default = Abandon`. Reserved for headless/batch modes
where the operator wants "nobody reviewed in time" as a distinct,
observable signal.

### Retry semantics

- `ReviewDecision::Retry { new_constraints }` triggers orchestrator-internal
  loop: despawn the current worker's worktree, re-spawn with
  `task = format!("{}\n\n## Additional constraints\n{}", original_task, new_constraints)`,
  emit `ExecutorRetryStarted { id, attempt_n: n+1, reason: new_constraints, new_session_id }`,
  re-enter the review phase.
- Bounded by `agent_config.max_review_retries` (default 3). When
  exceeded, force final status = `Failed { error: "retry limit exceeded" }`.
- The brain sees exactly one `DelegationResult` regardless of attempts.
- `ExecutorLineage` correctly tracks the attempt history (projection
  already handles `ExecutorRetryStarted`).

### Config additions

Per-agent config (add to `agent_config`):

```rust
pub struct AgentReviewPolicy {
    /// When true, the orchestrator gates every delegation to this agent
    /// on a human review. Default: false (preserves existing behavior).
    pub review_required: bool,
    /// How long to wait for a human decision before applying the default.
    /// Default: 30 minutes.
    pub review_timeout: Duration,
    /// What to apply on timeout. Default: `Reject { reason: "review timeout" }`.
    /// Set to `Abandon` for headless modes that want an explicit
    /// "nobody reviewed" signal.
    pub review_timeout_default: ReviewTimeoutAction,
    /// Cap on retry loops from `ReviewDecision::Retry`. Default: 3.
    pub max_review_retries: u32,
}

pub enum ReviewTimeoutAction {
    Approve,
    Reject { reason: String },
    Abandon,
}
```

Read from agent config TOML; default values apply when the
`[review]` section is absent.

## Data flow (end-to-end)

```
Brain tool call
   │ delegate_to_worker(agent, task)
   ▼
MCP server → DelegationRequest + oneshot<DelegationResult>
   │
   ▼
Orchestrator::delegate
   ├─ spawn worker in worktree
   ├─ worker runs, produces candidate DelegationResult
   │
   ├─ IF review_required:
   │   ├─ register oneshot<ReviewDecision> in pending_reviews
   │   ├─ emit ExecutorReviewRequested
   │   ├─ select! { decision_rx, timeout }
   │   ├─ (TUI) user sees review card, picks a/d/m/R
   │   ├─ (TUI) emits UserInput::SubmitReview
   │   ├─ (spur-cli) forwards to orchestrator's user_input_rx
   │   ├─ (orchestrator) dispatcher task pops sender, sends decision
   │   ├─ decision received → shape final DelegationResult
   │   ├─ IF decision == Retry, loop (bounded)
   │   └─ emit ExecutorReviewResolved
   │
   ├─ emit DelegationCompleted
   └─ oneshot.send(final DelegationResult)
       │
       ▼
MCP server returns DelegationResult text to brain
   │
   ▼
Brain reads "Delegation [Rejected | Success | Modified | ...]: ..."
```

## Error handling

- **Oneshot sender dropped before decision arrives** (TUI crashes,
  user quits): the orchestrator's `rx.recv()` returns `Err`.
  Treat as equivalent to timeout — apply `review_timeout_default`.
- **User-input dispatcher receives `SubmitReview` for an unknown
  executor_id** (race — decision arrives after review already
  resolved or node removed): log a `tracing::warn!` and drop the
  decision. Do not panic.
- **Retry limit exceeded**: final status = `Failed { error: "retry
  limit exceeded after {n} attempts" }`. Worktree state: the last
  attempt's worktree is removed normally. Historical attempts'
  worktrees were already removed as part of the despawn-respawn
  cycle.
- **Review timeout fires while decision is in-flight** (race):
  timeout wins. The decision that arrives afterward is dropped via
  the "unknown executor_id" path above. This is acceptable — timeout
  semantics trump late decisions.
- **Brain tool call cancelled while review is pending**: the MCP
  server's oneshot receiver drops. `oneshot.send(result)` returns
  `Err`. Orchestrator logs + abandons the delegation, cleans up the
  worktree. Pending review is cancelled (oneshot sender in
  `pending_reviews` is dropped too).

## Testing strategy

**Unit (spur-core)**
- `DelegationStatus` round-trips through serde JSON for every variant.
- Pending-reviews router: register sender, simulate decision arrival,
  verify oneshot fires.
- Decision → DelegationStatus mapping: table-driven test with all
  4 decision variants and 1 timeout.
- Retry bound: feed 4 `Retry` decisions, assert 4th iteration returns
  `Failed{"retry limit exceeded..."}`.
- Timeout behavior: use `tokio::time::pause()` to advance virtual
  time past `review_timeout`, assert default-decision applied.

**Integration**
- End-to-end: mock worker, inject `ExecutorReviewResolved` events,
  verify brain receives the correctly-shaped `DelegationResult`.
- Parallel delegations: spawn 3 workers concurrently, submit 3
  reviews (in arbitrary order), verify each gets the right decision
  routed to the right oneshot.
- Drop-cancellation: start a review, drop the user_input_tx, assert
  orchestrator handles gracefully.

**Manual / smoke**
- Configure a test agent with `review_required = true`, run a real
  delegation, verify the TUI shows the review card and the brain
  sees the chosen outcome.

## Build stages

Each stage compiles + tests independently.

1. **Expand `DelegationStatus`.** Add `Rejected`, `Modified`,
   `Abandoned` variants with `#[non_exhaustive]`. Update every
   `match status` site (spur-core + spur-tui lineage adapter +
   activity_log renderer). Existing tests must still pass.
2. **Add `AgentReviewPolicy` config.** Parse from agent config TOML
   with sensible defaults; plumb into the delegation path as a
   field on `agent_config`.
3. **Add `pending_reviews` state + user-input dispatcher task.** New
   field on `Orchestrator`; spawn a dispatcher task in
   `Orchestrator::run`; wire a new `mpsc::Receiver<UserInput>` input.
4. **Wire `UserInput::SubmitReview` from spur-cli.** Replace the TODO
   stub with a `send` on the orchestrator's input channel. spur-cli
   needs an `Arc` (or channel) to reach the orchestrator; verify
   plumbing.
5. **Insert the review gate in `delegate`.** Around line 1594: when
   `review_required`, register oneshot, emit events, `select!` on
   (decision_rx, timeout), shape `DelegationResult` per the decision.
6. **Retry loop.** Wrap the delegate body so `ReviewDecision::Retry`
   re-spawns the worker with appended constraints, emits
   `ExecutorRetryStarted`, re-enters review. Bound by
   `max_review_retries`.
7. **Worktree preservation on Reject and Abandoned.** The normal
   cleanup path removes the worktree. Add a conditional that skips
   cleanup when `status` is `Rejected` or `Abandoned` — both are
   cases where the worker did real work but no one validated it;
   preserve for inspection. Log the preserved worktree's path so
   the human can find it. (`Success`, `Modified`, `Failed`,
   `Conflict`, `Timeout` all fall through to normal cleanup.)
8. **DelegationResult text formatter update.** The brain-facing text
   rendering of `DelegationResult` must produce distinct output for
   `Rejected` / `Modified` / `Abandoned` / `Success` / etc. so the
   brain's prompt sees actionably-different strings.
9. **End-to-end smoke.** Configure a test agent with
   `review_required = true`; run through Approve/Reject/Modify/Retry
   scenarios manually with the TUI; verify brain behavior.

## Open questions

None blocking.

- Future: crash-durability (persist `pending_reviews` + in-flight
  worker state). Out of scope — needs Temporal-style
  architecture.
- Future: mid-flight interrupt (kill a running worker before
  completion). Separate spec.
- Future: batch-review UX (one decision covers N delegations).
  Speculative — wait for real demand.
