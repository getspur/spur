# Executor Lineage Visualization & Brain Loopback — Design

**Date:** 2026-04-13
**Status:** Proposed for review
**Owner:** spur-core / spur-tui (orchestrator-side loopback = follow-up spec)
**Scope:** B (TUI + event-model + lineage projection) · **Depth:** iii (recursive model, depth-1 rendering in v1)

## Goal

Give the human operator a live, navigable view of the brain→executor delegation
tree as it unfolds, and a structured way to review executor outcomes so the
review decision flows back into the brain's context — closing the
observe → review → next-execute loop.

## Non-goals

- Orchestrator-side plumbing that translates a user's review decision into the
  tool-call result the brain receives. That is a follow-up spec; this spec
  defines the contract the orchestrator must honor.
- Multi-channel input routing (Slack / email / webhook → brain). Separate spec.
- A new executor-spawning protocol. Executors continue to be spawned by the
  brain via its normal delegate-tool mechanism; this spec only adds richer
  observability and a review checkpoint.
- A DAG/canvas visualization. The lineage is a tree (or forest on multi-brain);
  DAG semantics are not load-bearing here.
- Persistence of review decisions to disk beyond what the existing
  `SessionHistory` replay already provides.

## Motivation

The brain agent (Claude Code / kiro-cli via ACP) delegates work to executor
agents. As of today the TUI renders a flat 2-level `AgentsTree` and a
chronological `ActivityLog` — enough to see that something is happening, but
not enough to answer:

- Which executor is doing what *right now*?
- Which executors have finished, and what did they produce?
- Which executors are waiting for human review?
- If an executor failed and was retried, what did the first attempt look like?
- What is the brain about to do next, and can I influence it?

The loopback to the brain is the load-bearing missing piece. Without it, the
human is a passive observer; with it, the brain becomes steerable mid-flight
without breaking its ACP session.

## Industry grounding

Research (`2026-04-13` round) confirmed three convergent patterns:

- **Lineage visualization** — production systems with parent/child *spawning*
  (Temporal child-workflow tree, Argo, CrewAI Studio, Claude Code sub-agent
  transcripts) converge on a **collapsible hierarchical tree**. DAG views win
  only when dependencies are load-bearing (Airflow, Dagster). Card-stacks
  (Cursor Background Agents) work for shallow parallelism, not recursion.
- **Loopback** — three primitives dominate:
  (a) checkpoint-and-resume with typed interrupt (LangGraph `interrupt()`,
  Argo `suspend`, CrewAI `human_input=True`),
  (b) external signal/approval gate (Temporal signals, GitHub Actions
  required reviewers, n8n manual approval),
  (c) tool-call permission prompt (Claude Code permissions, AutoGen
  `UserProxyAgent`).
  Spur already implements (c) for synchronous ACP permission requests.
- **TUI layout** — lazygit / k9s / gitui converge on
  **tree-on-left + detail-on-right + contextual command palette**.

This design fuses (a) + (c): the review payload is rich (diff, artifact
summary, error), but the decision is delivered back to the brain as a
tool-call result — because brain-delegates-executor is already a tool call
in the ACP model, so the brain's own lineage stays its normal tool-call tree.

## Architecture

### Load-bearing invariant

> `ExecutorLineage` is a **pure projection of the `SpurEvent` stream**.
> Replaying history (via `SessionHistory`) rebuilds an identical state.
> No TUI-local mutation. This buys free persistence, testable state
> machines, and a clean spur-core / spur-tui split.

### Three units, one interface each

**Unit 1 — `ExecutorLineage` projection** (new module in `spur-core`)
- *Does:* ingests `SpurEvent`s, maintains a recursive `ExecutorNode` forest.
- *Depends on:* `spur-acp::SpurEvent` (read-only).
- *Exposes:* `apply(&mut self, event: &SpurEvent)`, `nodes(&self)`,
  `root_ids(&self)`, `children_of(&self, id)`,
  `pending_reviews(&self) -> Vec<ExecutorId>`.
- *Testability:* deterministic — feed a sequence of events, snapshot the
  projection, compare.

**Unit 2 — `DashboardView` retrofit** (modify `spur-tui/src/views/dashboard.rs`)
- *Does:* renders the lineage tree (left), a focus-aware pane (right), and
  the existing input bar + status bar (unchanged structurally).
- *Depends on:* `ExecutorLineage` (read), existing `ActivityLog`, `InputBar`,
  `StatusBar`, `AgentsTree` (recursive-traversal upgrade).
- *Exposes:* unchanged `View` trait.
- *Testability:* render-snapshot tests over known lineage states.

**Unit 3 — Review card + typed `ReviewDecision`** (new component
`spur-tui/src/components/review_card.rs`)
- *Does:* renders the focused node's pending review inline in the detail
  pane and accepts keyboard decisions.
- *Depends on:* `ExecutorNode.pending_review`, an `Action::SubmitReview`.
- *Exposes:* a component render fn + a decision callback.
- *Testability:* pure-function decision mapping + component render tests.

### Data flow

```
spur-acp ──SpurEvent──▶ spur-core::ExecutorLineage ──read──▶ DashboardView
                                                                   │
                                                           user keystroke
                                                                   │
                                                                   ▼
                                            Action::SubmitReview { executor_id, decision }
                                                                   │
                                  spur-core emits ExecutorReviewResolved ─┐
                                                                          │
                                                              (follow-up spec:
                                                               orchestrator converts
                                                               to tool-call result
                                                               for brain's delegate
                                                               tool)
```

### Event model extensions (6 new `SpurEvent` variants)

Additive — existing variants unchanged. New variants, all emitted by
spur-acp / spur-core:

| Variant | Payload | Emitted when |
|---|---|---|
| `ExecutorSpawned` | `{ id, parent_id: Option<Id>, session_id, agent, role, task_spec, started_at }` | brain's delegate tool spawns an executor |
| `ExecutorPhaseChanged` | `{ id, phase: LifecycleState }` | any lifecycle transition |
| `ExecutorArtifact` | `{ id, artifact }` | executor produces a diff / PR url / file list / summary |
| `ExecutorReviewRequested` | `{ id, kind: ReviewKind, payload: ReviewPayload }` | executor reaches checkpoint needing human decision |
| `ExecutorReviewResolved` | `{ id, decision: ReviewDecision }` | user submits decision in TUI |
| `ExecutorRetryStarted` | `{ id, attempt_n, reason, new_session_id }` | brain retries a previously-failed executor |

`BrainSpawned` / `WorkerSpawned` remain; `ExecutorSpawned` is the
generalized form used by new code. Old consumers keep working; the
projection reads both and unifies them.

### Data model

```rust
// spur-core
pub struct ExecutorId(pub String);

pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>, // forest — supports multi-brain
}

pub struct ExecutorNode {
    pub id: ExecutorId,
    pub parent_id: Option<ExecutorId>,
    pub child_ids: Vec<ExecutorId>,
    pub agent: String,
    pub role: Role,            // Brain | Executor | SubExecutor
    pub task_spec: String,     // the delegating prompt / goal
    pub phase: LifecycleState,
    pub attempts: Vec<Attempt>,     // attempt 0 always present after spawn
    pub pending_review: Option<ReviewRequest>,
}

pub struct ReviewRequest {
    pub kind: ReviewKind,
    pub payload: ReviewPayload,
    pub requested_at: Instant,
}

pub struct Attempt {
    pub session_id: SessionId,      // per-attempt — retries open new sessions
    pub started_at: Instant,
    pub ended_at: Option<Instant>,
    pub status: AttemptStatus,      // Running | Succeeded | Failed | Cancelled
    pub cost_usd: f64,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
}

pub enum LifecycleState {
    Spawning, Running, AwaitingReview, Resuming, Succeeded, Failed, Cancelled,
}

pub enum ReviewKind { Completion, Failure, Conflict, Checkpoint }

pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

pub enum ReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },          // approve with guidance attached
    Retry { new_constraints: String },
}

pub enum Artifact {
    Diff(DiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}
```

## TUI presentation

### Layout (retrofit of `DashboardView`)

```
┌─ Lineage ───────────────────┐ ┌─ Detail / Activity ─────────────────┐
│ ◐ brain:kiro      [streaming]│ │ [tabs when focused: stream │ art │ │
│   ├─ ● worker-1     done 45s │ │  attempts │ task │ review]           │
│   ├─ ◐ worker-2  awaiting    │ │                                     │
│   │   review  ⚠              │ │ <chronological ActivityLog when     │
│   └─ ◐ worker-3   running 2m │ │  no node is focused; switches to    │
│       └─ ○ sub-a  spawning   │ │  node-scoped content on focus>      │
└──────────────────────────────┘ └─────────────────────────────────────┘
┌─ Input ─────────────────────────────────────────────────────────────┐
│ > _                                                   [kiro ▸▸▸]     │
└──────────────────────────────────────────────────────────────────────┘
  spur · 3 running · 1 review · $0.42 · 5m 12s · ?: help
```

- Left panel: lineage tree. Traversal code is fully recursive (walks
  arbitrary depth). v1 *visual* rendering shows depth-1 (brain + direct
  executors) as the default expanded state; deeper nodes exist in the
  tree and become visible when the user expands their parent via `c`.
  No depth-N rendering toggle is introduced in v1 — it falls out of the
  collapse/expand behavior.
- Right panel: **focus-aware**. No selection → chronological `ActivityLog`
  (today's behavior). Selection → tabbed node detail: `stream` (live
  output), `artifacts` (diffs/PR/files), `attempts` (retry stack),
  `task` (delegating prompt), `review` (when `pending_review.is_some()`).
- Review cards render **inline in the `review` tab**, not as a modal.
  Modal overlays remain only for synchronous ACP permission prompts
  (existing `handle_permission_request` — unchanged).

### Keybindings (additions only)

| Key | Action |
|---|---|
| `j` / `k` | Move selection down / up in the lineage tree |
| `Enter` | Focus the selected node (right pane enters node-detail mode) |
| `Esc` | Unfocus (right pane returns to chronological `ActivityLog`) |
| `Tab` | Cycle focus between panels (unchanged) |
| `r` | Jump to next node with `pending_review.is_some()` |
| `a` | In `review` tab: Approve |
| `d` | In `review` tab: Deny / Reject (prompts for reason) |
| `m` | In `review` tab: Modify+approve (prompts for note) |
| `R` | In `review` tab: Retry with new constraints (prompts) |
| `c` | Collapse/expand subtree under selected node |

### Status bar

Aggregate counters (running, awaiting-review, total cost, elapsed) replace
the current cost-only display. `awaiting-review > 0` is emphasized.

## Loopback contract

TUI side (this spec):

1. User selects a node with `pending_review = Some(_)` and presses `a`/`d`/`m`/`R`.
2. TUI dispatches `Action::SubmitReview { executor_id, decision }`.
3. App layer sends a `UserInput::SubmitReview { … }` up the channel
   (new `UserInput` variant).
4. spur-core emits `ExecutorReviewResolved { id, decision }`.
5. Projection clears `pending_review` on that node and records the
   decision on the current `Attempt`.

Orchestrator side (follow-up spec): consume `ExecutorReviewResolved` and
translate into the tool-call result that the brain's delegate-tool
invocation receives. The brain continues its session unchanged.

## Error handling

- **Event out-of-order** (e.g., `ExecutorPhaseChanged` before
  `ExecutorSpawned`): the projection buffers orphan events up to a small
  bounded queue (e.g., 128 per executor id) and replays them when the
  spawn arrives. After the bound, drops with a `tracing::warn!`.
- **Missing parent on `ExecutorSpawned`**: node joins the forest as a new
  root (same fallback behavior as today's orphan handling in
  `agents_tree.rs`). Logged.
- **Review submitted on a node that has moved past `AwaitingReview`**
  (race): TUI refuses to dispatch; surfaces a toast "review already
  resolved" in the status bar.
- **Review deadline**: reviews have no hard deadline in v1 (unlike the
  30s ACP permission flow). A future variant may add one.
- **Projection inconsistency during replay**: the projection exposes a
  `validate()` method checked in tests; runtime errors log and fall back
  to dropping the offending event rather than panicking.

## Testing strategy

**Unit (spur-core)**
- Projection determinism: for each new event variant, feed a crafted
  sequence, snapshot `ExecutorLineage`, compare.
- Orphan buffering: emit phase change before spawn, assert eventual
  consistency after spawn arrives.
- Replay equivalence: `live == fold(apply, init, event_log)` on a
  realistic 50-event stream.

**Unit (spur-tui)**
- `ReviewDecision` key → decision mapping (pure function).
- Recursive tree traversal: given forest of depth 3 with 10 nodes,
  rendered lines match golden snapshot.

**Integration**
- Full-flow test: synthesize brain spawn → two parallel executors →
  one review request → TUI submit approve → `ExecutorReviewResolved`
  emitted with correct payload.
- Backward-compat: existing `SpurEvent` test suites continue to pass.

**Manual / smoke**
- Run a real two-executor scenario in the TUI, verify tree, focus,
  tab switching, review approve/deny/modify/retry, jump-to-review.

## Build stages

Each stage is independently shippable and testable.

1. **Projection + events.** Add 6 `SpurEvent` variants (derive data from
   existing `DelegationRequested/Completed`, `ToolCall`, `TurnComplete`
   where possible — no orchestrator changes). Add `ExecutorLineage` in
   spur-core with tests.
2. **Recursive tree render.** Replace the 2-level filter in
   `agents_tree.rs` with recursive traversal off `ExecutorLineage`
   (traversal is unbounded depth; default expanded state renders
   depth-1, deeper nodes appear on `c` expand). Existing visual
   affordances preserved.
3. **Focus-aware detail pane.** Add selection state + tabs (stream,
   artifacts, attempts, task) to the right pane. Chronological log is
   the default; switches to node-detail on `Enter`.
4. **Review card + typed decision.** New `review_card` component,
   `review` tab, new keybindings, `Action::SubmitReview`, new
   `UserInput::SubmitReview`, emit `ExecutorReviewResolved`.
5. **Aggregate status bar + `r` jump.** Replace cost-only status with
   multi-counter display; wire `r` to iterate `pending_reviews()`.

## Open questions

None blocking. The orchestrator-side translation of `ReviewDecision`
into a tool-call result is explicitly deferred to a follow-up spec and
does not gate v1 of the TUI surface (v1 is fully testable end-to-end
within the TUI and projection).
