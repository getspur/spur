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

Research round (2026-04-13, refined after a second primary-source
pass) examined LangGraph, OpenAI Agents SDK, Temporal, Argo, n8n,
CrewAI, AutoGen.

**Closest architectural analog: Argo Workflows `suspend`/`resume`.**
Argo's `(workflow_id → suspended node, awaiting a typed enum
parameter)` maps 1:1 onto Spur's `(ExecutorId → oneshot<ReviewDecision>)`.
Both gate real work behind a correlation-id lookup plus a typed
decision. Argo's durable `workflow_id` is what Temporal offers in
richer form; Spur's in-memory `HashMap` is the non-durable counterpart
(durability disclaimed as non-goal).

**Closest tool-call-gating analog: OpenAI Agents SDK `needs_approval`.**
Approve → real tool result returned; Reject → synthetic refusal
injected; decision scoped per `call_id`. This matches Spur's synthetic-
`DelegationResult`-with-typed-status approach, except OpenAI's decision
vocabulary is binary. Spur's richer vocabulary (Approve / Reject /
Modify / Retry) requires the fuller enum surface.

**LangGraph `interrupt()` is *not* the right reference** despite
tempting surface similarity. Its `Command(resume=value)` does not
resume the tool coroutine at its await point — it re-runs the whole
node, with documented replay / double-execution traps
([langchain-ai/langgraph#6533](https://github.com/langchain-ai/langgraph/issues/6533)).
Rust async + `oneshot` genuinely does provide a real continuation,
which is why Spur is structurally immune to LangGraph's replay class
of bug.

**Pattern divergences matter:**
- **Retry ownership** splits across the industry: Temporal and CrewAI
  internalize retries; AutoGen and LangGraph-default surface rejection.
  **We adopt a novel split**: internal loop for `Retry`, surface for
  `Reject`. *Novel* — no surveyed system makes this distinction at the
  decision-verb level. Rationale: `Retry` is "same task, refined"
  (mechanical); `Reject` is strategic and the brain must replan.
  We own this as a design invention, not industry convention.
- **Tool-result translation** adopted: **synthetic tool result with
  typed status variants** — one `DelegationResult` whose `status`
  encodes the human's decision. This is OpenAI's shape extended from
  binary to Spur's richer vocabulary.

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
│       Ok(d) = rx => d,  // (attempt_n-matched by dispatcher)       │
│       _ = sleep(review_timeout) => {                               │
│         pending_reviews.remove(executor_id);  // explicit cleanup  │
│         TimeoutApplied(review_timeout_default)                     │
│       },                                                           │
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
│       TimeoutApplied(fallback) =>                                  │
│          status = TimedOut { waited_for, fallback },               │
│          preserve worktree                                         │
│     }                                                              │
│                                                                    │
│   emit DelegationCompleted                                         │
│   oneshot.send(final DelegationResult)  ← brain's tool call unblocks│
└────────────────────────────────────────────────────────────────────┘
```

### Three units, one interface each

**Unit 1 — Expanded `DelegationStatus` enum** (modify `spur-acp::domain::delegation`)
- *Does:* encodes all possible outcomes of a delegation.
  - Pre-existing worker-level variants: `Success`, `Failed { error }`,
    `Conflict { files }`, `Timeout` (existing 5-minute worker-hang
    timeout — kept as a separate variant from review timeout).
  - New review-level variants: `Rejected { reason }` (human-issued
    only), `Modified { reviewer_note }`,
    `TimedOut { waited_for, fallback: TimeoutFallback }` (review-gate
    timeout; `fallback` records what `TimeoutFallback` was applied).
  - The split `Timeout` (worker) vs. `TimedOut` (review) is deliberate:
    worker `Timeout` = "worker got stuck"; review `TimedOut` = "nobody
    reviewed in time." The brain must treat them differently.
- *Marked `#[non_exhaustive]`* so external crates can add catch-alls
  without recompile-breakage on future additions.

**Unit 2 — Pending-reviews routing** (new field on `Orchestrator`)
- *Does:* owns `pending_reviews: Arc<Mutex<HashMap<ExecutorId, (u32, oneshot::Sender<ReviewDecision>)>>>`. The tuple's `u32` is the currently-registered `attempt_n`, used by the dispatcher's supersession guard. The delegation task registers `(attempt_n, sender)`; a separate user-input dispatcher, after confirming attempt_n matches, pops and sends when `UserInput::SubmitReview` arrives.
- *Depends on:* `tokio::sync::{oneshot, Mutex}`, `spur_core::ExecutorId`.
- *Testability:* pure state container; unit tests with synthetic decisions.

**Unit 3 — User-input dispatcher task** (new top-level task in `Orchestrator::run`)
- *Does:* reads from a new `mpsc::Receiver<UserInput>` owned by the orchestrator; on `SubmitReview { executor_id, attempt_n, decision }`, looks up the oneshot in `pending_reviews`, **checks `attempt_n` matches the currently-registered review's attempt_n** (supersession guard — see "Attempt supersession" below), pops the sender, and forwards the decision. Mismatch or no-entry → log `tracing::warn!` and drop.
- *Spawned at orchestrator startup; runs for the orchestrator's lifetime.*
- *Depends on:* Unit 2, `spur_tui::UserInput`.
- *Why a separate channel (not extending `InteractiveInput`):* `run_interactive` serializes input behind brain-turn processing (see its `pending_messages: VecDeque` queue at `orchestrator.rs:346` and the `select!` at line 628). A `SubmitReview` delivered through that channel would incur head-of-line latency against brain I/O. A separate channel + dispatcher keeps review-decision latency bounded regardless of brain state.

### Decision → DelegationStatus mapping

| ReviewDecision | DelegationStatus | DelegationResult.diff | DelegationResult.summary | Worktree |
|---|---|---|---|---|
| `Approve` | `Success` | worker's diff | worker's summary | removed |
| `Reject { reason }` | `Rejected { reason }` | worker's diff (preserved) | worker's summary | **preserved for inspection** |
| `Modify { note }` | `Modified { reviewer_note: note }` | worker's diff | worker's summary | removed |
| `Retry { new_constraints }` | *(not a terminal decision — orchestrator re-enters the delegation loop)* | final iteration's diff | final iteration's summary | reused across attempts |
| *(review timeout)* | `TimedOut { waited_for, fallback }` | worker's diff (preserved) | worker's summary | **preserved** |

**`Rejected` is human-issued only.** Timeout-driven outcomes use
`TimedOut` (below). Keeping these separate matters: the brain treats
`Rejected.reason` as actionable feedback to address; `TimedOut` is
"no human reviewed in time" and must NOT be misinterpreted as
feedback. Collapsing them lets the brain waste turns trying to
address "review timeout" as if it were a grievance.

**Retry terminal behavior:** Retry is not a terminal decision; the
final `DelegationStatus` is determined by whichever non-Retry
decision ends the loop (or by the retry-limit backstop). Example:
Retry → Retry → Approve produces `Success`. Retry × 4 when
`max_review_retries = 3` produces
`Failed { error: "retry limit exceeded after 3 attempts" }`.

**Review timeout** produces `DelegationStatus::TimedOut { waited_for,
fallback }`, where `fallback` records what the configured
`TimeoutFallback` did:

```rust
pub enum TimeoutFallback {
    /// Auto-approve — worker's diff/summary retained as if reviewed.
    /// Brain reads TimedOut rather than Success so it can still
    /// distinguish human-approved from timeout-approved.
    Approve,
    /// Auto-reject — carries the configured reason.
    Reject { reason: String },
    /// Explicit "nobody reviewed" signal for headless/batch modes.
    /// Worktree preserved.
    Abandon,
}
```

The old `Abandoned { waited_for }` variant is retired — it was just
"TimedOut with fallback = Abandon." One variant carrying the fallback
discriminant is cleaner than two variants that must stay in sync.

**Review-timeout default:** `TimedOut { waited_for,
fallback: TimeoutFallback::Reject { reason: "review timeout" } }`.
The safer default (treats no-response as no) but the brain-visible
variant is `TimedOut`, not `Rejected`, so it is not confusable with
human-issued rejection.

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
- **Semaphore permit held across the retry loop.** Because retry
  happens inside the same spawned `execute_delegation` task that
  acquired a `max_concurrent` permit, the permit is retained across
  all attempts. Known v1 scaling limitation: with `max_concurrent = 4`
  and 4 concurrent delegations simultaneously hitting `Retry`, the
  pool stalls until a human resolves one. Acceptable for v1 because
  reviews are human-gated.

### Worker side-effect idempotency contract

`Retry` despawns and respawns the worker. Any side-effect the worker
performed in attempt N is **not rolled back** — it simply runs again
in attempt N+1. Same replay class of bug LangGraph users keep hitting
([langchain-ai/langgraph#6533](https://github.com/langchain-ai/langgraph/issues/6533)).

**Contract** (enforced by worker implementations, not by this spec's code):

- Workers MUST NOT commit external side-effects during or before the
  review gate. Only allowed side-effect: worktree-local file changes
  (rolled back by worktree despawn).
- External side-effects — PR creation, Linear comments, webhook posts,
  remote git pushes — MUST happen *after* the orchestrator receives
  `ReviewDecision::Approve`, in orchestrator code or a post-Approve
  worker invocation, not inside the worker path that runs before the
  gate.

**Failure mode if violated**: attempt 1 creates a Linear comment;
human clicks Retry; attempt 2 re-runs the worker and creates a
*second* Linear comment. By attempt 3, there are three. The brain
never sees any of this because it's downstream of the review gate.

### Attempt supersession

When `Retry` fires, the old attempt's `oneshot::Sender` was consumed
on dispatch. A fresh `Sender` is registered under the same
`executor_id` for attempt N+1. Without discipline this admits a
stale-UI hazard:

> User opens the review card for attempt 1, pauses. Someone clicks
> Retry. Orchestrator respawns; new sender registered for attempt 2.
> First user, unaware, hits `a` for Approve. Dispatcher pops the
> attempt-2 sender and delivers `Approve` — accidentally approving
> attempt 2's output nobody has seen.

**Fix**: propagate `attempt_n` end-to-end so the dispatcher can
reject stale decisions.

- `SpurEventBody::ExecutorReviewRequested` carries `attempt_n: u32`.
  The TUI's review card captures this.
- `spur_tui::UserInput::SubmitReview` carries `attempt_n: u32`.
- The dispatcher's `pending_reviews` stores
  `(attempt_n, oneshot::Sender<ReviewDecision>)`. On `SubmitReview`
  receipt, it compares incoming `attempt_n` against the stored one
  *before* popping. Mismatch → `tracing::warn!` and drop the
  decision; keep the sender in place.

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
    /// What to apply on timeout. Default:
    /// `TimeoutFallback::Reject { reason: "review timeout" }`.
    pub review_timeout_default: TimeoutFallback,
    /// Cap on retry loops from `ReviewDecision::Retry`. Default: 3.
    pub max_review_retries: u32,
}
```

**Type unification**: the config's timeout-default field and the
`TimedOut` variant's `fallback` field share a single `TimeoutFallback`
type (defined in the "Decision → DelegationStatus mapping" section).
No conversion / `From` impl needed.

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
- **Attempt-n mismatch on `SubmitReview`**: log `tracing::warn!` with
  `got` and `expected` attempt_n, drop the decision, keep the
  registered sender in place (see "Attempt supersession" above).
- **Retry limit exceeded**: final status = `Failed { error: "retry
  limit exceeded after {n} attempts" }`. Worktree state: the last
  attempt's worktree is removed normally. Historical attempts'
  worktrees were already removed as part of the despawn-respawn
  cycle.
- **Review timeout fires while decision is in-flight** (race):
  timeout wins. The timeout branch **must explicitly remove the
  entry from `pending_reviews`** before applying the fallback
  (`pending_reviews.lock().await.remove(&executor_id)`); otherwise a
  late-arriving `SubmitReview` finds a stale sender whose receiver has
  already been dropped.
- **Brain tool call cancelled while review is pending**: the MCP
  server's oneshot receiver drops. `oneshot.send(result)` returns
  `Err`. Before cleanup, the orchestrator emits
  `SpurEventBody::ExecutorReviewCancelled { id, reason: "brain call
  cancelled" }` (new event body variant) so the lineage projection
  records the abandonment. Orchestrator then cleans up the worktree
  and drops the pending review's sender.

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
   `TimedOut { waited_for, fallback: TimeoutFallback }` variants with
   `#[non_exhaustive]`; add `TimeoutFallback` enum. Update every
   `match status` site with stub arms (semantic handling in later
   stages). `Timeout` retained (worker hang); `Abandoned` retired in
   favor of `TimedOut { fallback: Abandon }`.
2. **Add `attempt_n` to `ExecutorReviewRequested`** event body; add
   `attempt_n: u32` to projection's `ReviewRequest`.
3. **Add `ExecutorReviewCancelled`** body variant; projection clears
   `pending_review` on receipt.
4. **Add `AgentReviewPolicy` config.** Parse from agent config TOML
   with defaults; `review_timeout_default: TimeoutFallback` (shared
   with `DelegationStatus::TimedOut.fallback`).
5. **Create `ReviewSink`** (new `crates/spur-core/src/review_sink.rs`):
   `register(executor_id, attempt_n)`, `submit(executor_id, attempt_n,
   decision)` with attempt_n supersession guard, `remove`.
6. **Extend `UserInput::SubmitReview`** with `attempt_n` (TUI + action
   + dashboard view plumbing).
7. **Add `review_sink` state + dispatcher task + spur-cli wiring.**
   `InteractiveInput::SubmitReview` variant; `review_dispatcher_loop`
   task; spur-cli replaces TODO stub with routing into dispatcher.
8. **Plumb `review_sink` into `execute_delegation`.** Mechanical
   parameter threading from `handle_delegations` → `execute_delegation`.
9. **Insert the review gate.** When `review_required`, register on
   `review_sink`, emit events, `select!` on (decision_rx, timeout),
   shape `DelegationStatus` per decision. Timeout branch explicitly
   removes the sink entry before applying fallback.
10. **Retry loop + supersession.** Wrap worker-spawn + gate in a loop
    on `ReviewDecision::Retry`; bump `attempt_n`; emit
    `ExecutorRetryStarted`. Bounded by `max_review_retries`.
11. **Worktree preservation on `Rejected` and `TimedOut`.** Skip
    cleanup; log preserved path. `Success`, `Modified`, `Failed`,
    `Conflict`, worker `Timeout` all fall through to normal cleanup.
12. **Brain-cancellation audit event.** When `respond_to.send` fails
    with a pending review, emit
    `SpurEventBody::ExecutorReviewCancelled { id, reason }` before
    cleanup.
13. **DelegationResult text formatter distinctness.** Regression test:
    `Rejected` / `Modified` / `TimedOut` / `Success` render as
    distinguishable JSON so the brain's prompt can pattern-match.
14. **End-to-end smoke.** Configure a test agent with
    `review_required = true`; run through Approve/Reject/Modify/Retry
    scenarios; verify brain behavior; verify stale-UI double-submit is
    rejected by attempt_n guard; verify timeout produces `TimedOut`.

## Open questions

None blocking.

- Future: crash-durability (persist `pending_reviews` + in-flight
  worker state). Out of scope — needs Temporal-style
  architecture.
- Future: mid-flight interrupt (kill a running worker before
  completion). Separate spec.
- Future: batch-review UX (one decision covers N delegations).
  Speculative — wait for real demand.
