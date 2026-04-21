# End-to-End Closure Design
- Goal: close the persisted-plan loop from plan ingress through dispatch, worker completion, review, mutation, epic completion, and restart recovery.
- Status: draft
- Authored-by: codex-acp
- Spec-version: 0
- Date: 2026-04-21
- Related:
  - rev 4 adaptive plan repair design: `docs/superpowers/specs/2026-04-20-adaptive-plan-repair-design.md` at commit `5026896`
  - async continuation design: `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`
- Scope: design only; no Rust changes in this document

---

## Problem Statement

The adaptive-plan-repair work shipped important pieces of the persisted-plan path.
Those pieces do not yet compose into one authoritative end-to-end loop.
The shipped system has a durable issue graph in beads, an in-memory `PlanState` in `spur-mcp`, and a third partial observer in the reconciler.
That is enough to demonstrate ingress, manual review, and signal mutation in isolation.
It is not enough to guarantee one recoverable operational truth for persisted plans.
The current code shape is:

| Area | Current state | Current-code anchor |
|---|---|---|
| Plan ingress via `submit_plan(persist_as_epic=true)` | The server validates tasks, creates a beads epic plus topologically ordered child issues, stamps `spur:plan-id`, `spur:plan-task-id`, `spur:agent`, and optional `spur:source-issue`, then builds an in-memory `PlanState` and spawns `run_plan`. | `crates/spur-mcp/src/server.rs:360-415`, `crates/spur-mcp/src/server.rs:543-625`, `crates/spur-mcp/src/server.rs:2347-2484` |
| Plan ingress via `execute_epic` | The server derives a plan from an existing epic and its children, inserts it into `active_plans`, and spawns `run_plan`, but the resulting `PlanState` hard-codes `epic_id: None`. | `crates/spur-mcp/src/plan/mod.rs:220-430`, `crates/spur-mcp/src/server.rs:2517-2729`, especially `crates/spur-mcp/src/server.rs:2674` |
| Runtime authority | Persisted plans still rely on `active_plans: HashMap<String, Arc<Mutex<PlanState>>>` as the live execution source. `plan_registry` is another RAM-only map used for `execute_epic` idempotency. | `crates/spur-mcp/src/server.rs:218-226`, `crates/spur-mcp/src/plan/mod.rs:180-188` |
| Dispatch | `run_plan` computes ready tasks from in-memory approvals, marks them `Dispatched`, sends `DelegationRequest` directly over the orchestrator channel, and emits only an audit comment. `review_task(approve)` also dispatches newly ready tasks directly via `dispatch_newly_ready`. | `crates/spur-mcp/src/plan/mod.rs:730-909`, `crates/spur-mcp/src/plan/mod.rs:1918-2023`, `crates/spur-mcp/src/plan/mod.rs:2509-2614` |
| Reconciler | The reconciler is wired into server startup and can observe ready tasks via `bv` primary plus `br ready` fallback, but `tick_once` only logs ready IDs; it does not dispatch. | `crates/spur-mcp/src/server.rs:1181-1205`, `crates/spur-mcp/src/plan/reconciler.rs:1-185` |
| Worker completion | Successful worker completion moves the in-memory task to `AwaitingReview` and emits a `Completion` audit sentinel. There is no durable dispatch label and no durable review-ready marker written today. | `crates/spur-mcp/src/plan/mod.rs:819-907`, `crates/spur-mcp/src/plan/mod.rs:2616-2729`, `crates/spur-mcp/src/plan/labels.rs:44-57`, `crates/spur-mcp/src/plan/mod.rs:611-669` |
| Review | `handle_review_task` mutates in-memory plan state under lock, then updates the child issue outside the lock. Approve writes `closed_status`; reject writes `"open"`; request-changes re-dispatches immediately through the channel. | `crates/spur-mcp/src/server.rs:2898-2942`, `crates/spur-mcp/src/plan/mod.rs:1918-2455` |
| Signals | `report_signal` writes an audit sentinel, the worker signal sentinel comment, and a `signal:*` label. The brain-side watcher polls every 3s, scans all issues, filters on `signal:*` and no `spur:signal-processed:*`, but hydrates `stub_plan_state()` instead of the real plan. | `crates/spur-mcp/src/server.rs:445-541`, `crates/spur-mcp/src/server.rs:1219-1230`, `crates/spur-mcp/src/plan/signal_watcher.rs:1-185` |
| Mutation durability | Mutation execution already uses write-ahead audit, `spur:mutation-id:*`, `spur:superseded-by:*`, and `spur:signal-processed:*` labels, but restart orphan resolution is still absent. | `crates/spur-mcp/src/plan/mutation_executor.rs:26-195`, `crates/spur-mcp/src/plan/labels.rs:101-134` |
| Epic completion | Persisted child issues can be closed on approve because `spec.issue_id` points at the created beads child for `submit_plan`, but there is no all-children-closed check and no epic auto-close path. `PlanState.epic_id` is described as “informational only”. | `crates/spur-mcp/src/server.rs:607-625`, `crates/spur-mcp/src/plan/mod.rs:154-173`, `crates/spur-mcp/src/server.rs:2674`, `crates/spur-mcp/src/server.rs:2068-2220` |
| Merge and PR | `merge_plan` and `create_pr` exist as manual tools. `merge_plan` reads `base_snapshot_branch`, task approvals, and `worker_branch` values only from `active_plans`. There is no automatic hook from plan completion or epic completion. | `crates/spur-mcp/src/server.rs:690-760`, `crates/spur-mcp/src/server.rs:2068-2220`, `crates/spur-mcp/src/tools.rs:431-630` |
| Review/diff recovery | `get_task_diff` and `merge_plan` depend on cached `worker_branch`, `result`, `history`, and `base_snapshot_branch` from `active_plans`. Historical attempts already admit that full diff text is not persisted. | `crates/spur-mcp/src/server.rs:2130-2214`, `crates/spur-mcp/src/server.rs:2801-2895` |
| Continuation/events | ACP continuation and plan lifecycle events already exist, but they are not the persisted operational truth. `PlanCompleted` and `PlanReadyToMerge` are emitted from the plan executor, not reconstructed from beads. | `crates/spur-mcp/src/plan/mod.rs:1012-1079`, `crates/spur-acp/src/domain/events.rs:574-624`, `crates/spur-acp/src/domain/continuation.rs:11-23` |

The core defect is not “a missing feature”.
It is an authority split.
For persisted plans, the code persists one graph into beads, then keeps driving execution out of a separate RAM graph.
That split shows up in four places.
### 1. Dispatch has two conceptual homes
`run_plan` is the actual dispatcher today.
The reconciler is the intended beads-side dispatcher, but it is observation-only.
That means the codebase already contains both:
- a RAM readiness engine (`run_plan`)
- a beads readiness observer (`Reconciler::observe_ready`)
Only one of them can be authoritative for persisted plans.
Current anchors:
- `run_plan` direct dispatch: `crates/spur-mcp/src/plan/mod.rs:748-828`
- approval-cascade direct dispatch: `crates/spur-mcp/src/plan/mod.rs:2007-2023`, `crates/spur-mcp/src/plan/mod.rs:2523-2599`
- reconciler observe-only tick: `crates/spur-mcp/src/plan/reconciler.rs:109-144`
### 2. Signals can only see a fake plan
The worker-facing signal path already writes durable comments and labels.
The brain-side watcher does not project the actual plan from those durable records.
It fabricates a stub state with:
- a synthetic `plan_id`
- no tasks
- no epic id
- no merge base
Current anchor:
- `stub_plan_state`: `crates/spur-mcp/src/plan/signal_watcher.rs:177-185`
That makes the mutation scorer seam technically present, but operationally blind for persisted-plan recovery and future MCTS inputs.
### 3. Restart loses the execution truth
`active_plans` and `plan_registry` are process memory.
Once the process dies:
- the current execution epoch is lost
- the in-flight dispatch state is lost
- the reconstructed review state is lost
- the merge base is lost unless the server stayed alive
Current anchors:
- RAM caches: `crates/spur-mcp/src/server.rs:218-226`
- `merge_plan` requires cached base snapshot: `crates/spur-mcp/src/server.rs:2160-2168`
- `execute_epic` idempotency consults RAM cache/registry: `crates/spur-mcp/src/server.rs:2549-2614`
### 4. The beads graph is already rich enough to matter
This is why abandoning beads is the wrong move.
The durable graph already carries:
- child issue identity
- dependency edges
- plan and task scope labels
- worker agent labels
- worker signal comments
- audit trail comments
- mutation lineage labels
Current anchors:
- plan issue creation and labels: `crates/spur-mcp/src/server.rs:560-593`
- signal labels/comments: `crates/spur-mcp/src/server.rs:509-534`
- audit sentinel kinds: `crates/spur-mcp/src/plan/audit_sentinel.rs:13-128`
- mutation labels: `crates/spur-mcp/src/plan/mutation_executor.rs:61-75`, `crates/spur-mcp/src/plan/mutation_executor.rs:115-126`, `crates/spur-mcp/src/plan/mutation_executor.rs:185-193`
So the right question is not:
“Should SPUR use beads or RAM?”
The right question is:
“Which one is the operational truth for persisted plans, and which one is a projection?”
That question has to be answered explicitly, or every restart, review, and signal path will keep rediscovering the same ambiguity.

---

## Authority Decision

### Plain statement
For persisted plans, beads becomes the authoritative operational state store.
`[[spur-audit v1]]` remains the authoritative history and lineage surface.
`active_plans` is downgraded from source of truth to disposable projection/cache.
The reconciler becomes the only dispatcher for persisted plans.
`review_task` becomes a state-transition writer, not a dispatcher.
The signal watcher must project real persisted plan state from beads before proposing or scoring mutations.
Restart recovery becomes:
- beads graph projection for current operational state
- audit-sentinel replay for causal history
- explicit orphan-handling rules for partially persisted mutation and dispatch state
That is the decision.
### Why this follows from first principles
#### P1. Persisted work must be restartable from persisted state
If the plan is persisted as a beads epic, the execution truth cannot live only in process memory.
Today, the server persists the graph, then immediately spawns `run_plan` against an in-memory `PlanState` and stores that state in `active_plans` (`crates/spur-mcp/src/server.rs:2457-2484`).
That is acceptable for ephemeral plans.
It is a contradiction for persisted plans.
If the process dies, the system has kept the input graph but discarded the execution truth.
That makes “persisted plan” a half-promise.
#### P2. Only one component may own dispatch
Dispatch is the highest-leverage operational decision.
It decides:
- when a task is in flight
- which worker owns it
- what delegation id represents that ownership
- when downstream work becomes eligible
Today, that decision is made directly inside `run_plan` and again inside `dispatch_newly_ready` (`crates/spur-mcp/src/plan/mod.rs:748-828`, `crates/spur-mcp/src/plan/mod.rs:2523-2599`), while the reconciler independently computes ready work from beads (`crates/spur-mcp/src/plan/reconciler.rs:119-185`).
That is a split-brain topology.
If persisted plans remain RAM-dispatched, the reconciler can never become authoritative.
If the reconciler becomes authoritative, RAM dispatch must stop.
There is no stable hybrid here.
#### P3. The transport pipe is not the state store
The orchestrator channel is useful.
It is not durable.
`DelegationRequest` flows through `mpsc::Sender<DelegationRequest>` from server/plan code into `spur-core` (`crates/spur-mcp/src/server.rs:197`, `crates/spur-mcp/src/plan/mod.rs:3-6`, `crates/spur-mcp/src/plan/mod.rs:737`, `crates/spur-core/src/orchestrator.rs:2778-2788`).
That makes it a transport.
Not an authority.
Authorities survive process loss.
Transport pipes do not.
Therefore the right design is:
- keep the orchestrator channel as execution transport
- move dispatch authority into durable beads state transitions
#### P4. Review and signal mutation need the same projected state
A persisted plan has one dependency graph.
The review engine, the dispatch engine, and the signal/mutation engine must all reason over the same projected graph.
Right now they do not.
- review mutates in-memory `PlanState`
- reconciler reads ready work from beads only
- signal watcher reads beads comments but substitutes `stub_plan_state()`
Current anchors:
- review mutation path: `crates/spur-mcp/src/plan/mod.rs:1918-2455`
- reconciler read path: `crates/spur-mcp/src/plan/reconciler.rs:119-185`
- signal watcher stub: `crates/spur-mcp/src/plan/signal_watcher.rs:136-185`
If persisted plans are beads-truth, all three must consume the same projected state.
That projection can be cached.
It cannot be invented independently in three places.
#### P5. Audit history and operational state are different authorities
This is the same split that rev 4 already made for adaptive repair.
Operational decisions should be driven by current beads issue state.
Historical interpretation, lineage, and future MCTS reward attribution should be driven by `[[spur-audit v1]]`.
Current anchors:
- audit sentinel kinds: `crates/spur-mcp/src/plan/audit_sentinel.rs:18-77`
- operational issue service: `crates/spur-pm/src/service.rs:120-207`
This design keeps that separation:
- beads issue graph answers “what can happen next?”
- audit sentinel log answers “how did we get here?”
### Alternatives considered
#### Alternative A: keep RAM as truth and treat beads as a mirror
Rejected.
Why:
- it defeats the point of `persist_as_epic=true`
- it makes restart recovery impossible without serializing full `PlanState`
- it leaves signals reading one store and dispatch using another
- it keeps `execute_epic` idempotency dependent on RAM maps
Current contradictions:
- persisted plan still spawns RAM executor: `crates/spur-mcp/src/server.rs:2472-2484`
- restart-sensitive registry lives only in memory: `crates/spur-mcp/src/server.rs:221-226`
- signal watcher already consumes beads state directly: `crates/spur-mcp/src/plan/signal_watcher.rs:70-173`
#### Alternative B: hybrid dual authority
Rejected.
Meaning of the rejected hybrid:
- RAM decides dispatch and review sequencing
- beads decides signal mutation and restart state
- reconciliation logic arbitrates conflicts afterwards
This is not a stable architecture.
It gives no crisp answer to:
- which store wins on disagreement
- how a crash between RAM mutation and beads mutation is resolved
- whether a task is “running” when RAM says yes and beads says no
The current code is already the warning.
It has:
- a ready observer in beads
- a dispatcher in RAM
- a signal watcher in beads
That is the hybrid, and it is exactly the gap this spec is closing.
#### Alternative C: abandon beads and persist the full plan somewhere else
Rejected by constraint, and rejected technically.
beads already owns:
- issue identities
- dependencies
- labels
- comments
- graph queries
The work to replace it would be larger than finishing the durable projection model.
### Consequences of the decision
Once this authority decision is adopted, the following become non-negotiable for persisted plans:
1. Every operationally meaningful persisted-plan phase must be representable in beads.
2. `active_plans` cache misses must be recoverable from beads plus audit replay.
3. `review_task` may mutate persisted state,
but may not dispatch persisted tasks directly.
4. The reconciler owns all persisted-plan dispatch.
5. Signal mutation proposals must run against projected persisted state,
not `stub_plan_state()`.
6. Restart recovery must explicitly resolve orphaned mutation and dispatch intent.
7. Epic completion must be derived from persisted child state,
not from whether a RAM `run_plan` happened to survive long enough to emit an event.
### Scope boundary
This authority decision applies to persisted plans only:
- `submit_plan(persist_as_epic=true)`
- `execute_epic(...)`
Pure ephemeral plans may keep the existing in-memory executor for now.
That boundary matters for delivery.
It lets v0c convert the persisted path first, without forcing a full rewrite of ephemeral plan execution.

---

## Architecture

### Architectural thesis
Persisted-plan execution becomes a projected control loop.
The durable issue graph is the source.
Three components project from it:
- the reconciler
- the signal watcher
- the status/review endpoints
One component dispatches from it:
- the reconciler
`active_plans` remains useful, but only as a process-local cache of the latest projection plus rich ephemeral fields such as recomputed diff payloads.
### Closed-loop diagram

```mermaid
flowchart LR
    B[Brain]
    M[MCP Server]
    P[(beads issues + deps + labels)]
    A[[[[spur-audit v1]] comments]]
    X[Plan Projector]
    C[active_plans cache<br/>projection only]
    R[Reconciler<br/>single persisted-plan dispatcher]
    O[Orchestrator channel<br/>execution transport]
    W[Worker]
    S[SignalWatcher]
    G[get_plan_status / get_task_diff / review_task]
    B -->|submit_plan / execute_epic| M
    M -->|persist graph + plan bootstrap| P
    M -->|plan-submit audit| A
    P --> X
    A --> X
    X --> C
    X --> R
    X --> S
    X --> G
    R -->|dispatch intent: labels + audit| P
    R -->|dispatch request| O
    O --> W
    W -->|completion result| O
    O -->|completion callback| M
    M -->|completion state + ready-for-review| P
    M -->|completion audit| A
    B -->|review_task / get_task_diff| G
    G -->|review decision persisted| P
    G -->|approval/rejection audit| A
    G -->|fast-forward| R
    W -->|report_signal| M
    M -->|signal label + sentinel| P
    M -->|signal audit| A
    S -->|projected state + proposer/scorer| A
    S -->|mutation ops| P
    R -->|epic terminal check| P
    R -->|epic completion audit| A
```

### Component responsibilities

| Component | Responsibility under this design | Boundary |
|---|---|---|
| `spur-pm` | Remains the only adapter for issue CRUD, dependency queries, ready queries, comments, and PR creation. No plan semantics move into `spur-pm`. | `crates/spur-pm/src/service.rs:120-207` is still the PM facade. |
| `spur-mcp::server` | Owns ingress normalization, completion writeback, cache rehydration, and MCP tool surfaces. Stops spawning persisted-plan `run_plan`. | Current ingress/writeback anchors: `crates/spur-mcp/src/server.rs:2347-2484`, `crates/spur-mcp/src/server.rs:2517-2729`, `crates/spur-mcp/src/server.rs:2898-2942`. |
| `spur-mcp::plan::reconciler` | Becomes the only persisted-plan dispatcher. Keeps `bv` primary plus `br ready` fallback, but now converts ready IDs into durable dispatch transitions and transport sends. | Current observe-only anchor: `crates/spur-mcp/src/plan/reconciler.rs:109-185`. |
| `spur-mcp::plan::signal_watcher` | Stops inventing `stub_plan_state()`. Projects the real persisted plan and only mutates review-ready tasks. | Current stub anchor: `crates/spur-mcp/src/plan/signal_watcher.rs:136-185`. |
| `spur-mcp::plan::mod` | Retains plan/task status rendering, diff helpers, and ephemeral-plan executor. Persisted-plan dispatch logic moves out. | Current dispatch anchors: `crates/spur-mcp/src/plan/mod.rs:730-909`, `crates/spur-mcp/src/plan/mod.rs:2509-2614`. |
| `spur-acp` | Continues to define lifecycle events and continuations. Events become projections of durable persisted-plan state, not the authority itself. | `crates/spur-acp/src/domain/events.rs:574-624`, `crates/spur-acp/src/domain/continuation.rs:11-23`. |
| `spur-core` | Continues to own worker sessions and the orchestrator request path. No durable plan semantics move here. | `crates/spur-core/src/orchestrator.rs` remains the worker transport owner. |

### Persisted-plan runtime model
For persisted plans, the projected task phase is derived from durable fields only.
The projection rule is:
1. Start from beads task identity,
labels, blocked-by edges, current open/closed status, and parsed audit sentinels.
2. Derive the phase from operational markers first,
history markers second.
3. Treat any projection ambiguity as non-dispatchable until resolved.
The operational phase mapping is:

| Durable shape | Projected phase | Why |
|---|---|---|
| `status == closed` and `spur:superseded-by:*` present | `Superseded` | Mutation lineage already uses labels for replacement children. Current writer: `crates/spur-mcp/src/plan/mutation_executor.rs:115-126`. |
| `status == closed` and latest terminal review breadcrumb is approval | `Approved` | Approval remains a review terminal. Current audit kind exists at `crates/spur-mcp/src/plan/audit_sentinel.rs:36-42`. |
| `status == closed` and latest terminal review breadcrumb is rejection | `Rejected` | Persisted reject becomes truly terminal under this design. Current code writes reject as `"open"`; that changes. Current anchor: `crates/spur-mcp/src/plan/mod.rs:2047-2058`. |
| `status == closed` and latest completion outcome is failed/cancelled | `Failed` or `Cancelled` | Worker-owned terminal outcomes must be reflected durably. Current code only stores them in RAM. Current anchor: `crates/spur-mcp/src/plan/mod.rs:852-877`, `crates/spur-mcp/src/plan/mod.rs:2675-2704`. |
| `status == open` and `ready-for-review` present | `AwaitingReview` | This is the durable handoff from worker to brain. Current label constant exists but has no writers today: `crates/spur-mcp/src/plan/labels.rs:57`, repo search only finds the definition. |
| `status == open` and `delegation-id:*` present and no `ready-for-review` | `Dispatched` | This is the durable worker-owned interval. Current label constructor exists but has no writers today: `crates/spur-mcp/src/plan/labels.rs:44-46`, repo search only finds the constructor. |
| `status == open`, no dispatch/review marker, deps satisfied | `Ready` | Ready is derived from the beads graph, not from RAM task status. Current ready query already exists in reconciler: `crates/spur-mcp/src/plan/reconciler.rs:119-185`. |
| `status == open`, no dispatch/review marker, deps unsatisfied | `Pending` | Same beads-graph derivation. |

The in-memory projection may still materialize `PlanTaskStatus` values.
It may not invent any persisted-plan phase that cannot be reconstructed from:
- issue status
- issue labels
- issue graph
- audit comments
### Projection input set
The projector for persisted plans reads:
- epic issue
- child issues in the plan scope
- child labels
- child blocked-by edges
- child comments that parse as `[[spur-audit v1]]`
- child comments that parse as `[[spur-signal v1]]`
- epic comments that parse as `[[spur-audit v1]]`
It does not need:
- the old in-memory `run_plan` task list
- the reconciler's prior observation output
- process-local `seen` sets
Projection outputs:
- `PlanState`-compatible task ordering and statuses
- attempt counts reconstructed from dispatch breadcrumbs
- `worker_branch` and summary metadata reconstructed from completion breadcrumbs
- epic linkage
- base snapshot ref reconstructed from plan bootstrap audit
### Ingress normalization
#### `submit_plan(persist_as_epic=true)`
This path already creates a self-describing task graph.
It keeps:
- epic creation
- child creation
- dependency creation
- `spur:plan-id`
- `spur:plan-task-id`
- `spur:agent`
- optional `spur:source-issue`
- `spur:plan-complete` on the epic
Current anchors:
- issue creates: `crates/spur-mcp/src/server.rs:560-593`
- completion marker on epic: `crates/spur-mcp/src/server.rs:396-414`
What changes:
- no persisted-plan `run_plan` spawn
- no persisted-plan authority handoff into RAM
- plan bootstrap audit must include enough data to rehydrate merge/review state after restart
The server still may warm the cache after submit.
That cache is advisory.
#### `execute_epic`
This path must stop being “derive once, then forget the durable source”.
Before a persisted execution epoch begins, `execute_epic` must normalize the epic so the execution is self-describing after restart.
That normalization step does four things:
1. Resolve and persist the worker agent for every child task.
If `default_agent` filled a gap, write the resolved `spur:agent:<name>` label onto the child so later projection does not depend on the original RPC argument.
2. Stamp the execution `plan_id` onto the epic and all child tasks.
If older `spur:plan-id:*` labels exist from a prior execution epoch, replace them.
3. Ensure every child has a stable `spur:plan-task-id:<id>` label.
For pre-existing epics, the child issue id is sufficient as the task id.
4. Emit the same plan bootstrap audit used by `submit_plan`.
This removes the current asymmetry where `execute_epic` derives a plan but leaves `epic_id: None` and does not persist the execution epoch (`crates/spur-mcp/src/server.rs:2668-2675`).
### Dispatch path
For persisted plans, dispatch becomes a reconciler-owned transaction-shaped sequence.
Not a literal transaction, because beads has no transaction primitive.
But one ordered durable sequence with explicit recovery rules.
The persisted dispatch sequence is:
1. Project the plan from beads.
2. Ask `bv` for ready work if available;
otherwise fall back to `br ready`.
3. For each ready task,
allocate a new `delegation_id`.
4. Persist dispatch intent on the task:
- add `delegation-id:<id>` - remove `ready-for-review` if present - emit `Dispatch` audit sentinel
5. Send `DelegationRequest` through the existing orchestrator channel.
6. If the send fails immediately:
- clear the dispatch label - leave the task open - record a warning and a compensating audit breadcrumb
7. If the send succeeds,
the task is worker-owned until completion writeback clears the dispatch label.
Why write durable intent before sending?
Because the opposite ordering is worse for recovery.
If SPUR sends first and crashes before persisting dispatch intent, it has created side effects in a worker with no durable record of who owns the task.
If SPUR persists first and the send fails, it can compensate deterministically in the same process.
### Worker completion path
The worker completion bridge stays on the current orchestrator machinery.
That is a good boundary.
The change is what happens after the result comes back.
For persisted plans, the completion writeback path must durably encode the result into beads, not only into RAM.
The success path becomes:
1. emit a `Completion` audit sentinel that carries:
- `delegation_id` - `completion_state = awaiting_review` - `worker_branch` - `result_summary` - optional artifact pointer if present
2. remove `delegation-id:<id>`
3. add `ready-for-review`
4. leave the task issue `open`
5. fast-forward the reconciler
The failure/cancel path becomes:
1. emit a `Completion` audit sentinel with
`completion_state = failed` or `cancelled`
2. remove `delegation-id:<id>`
3. set issue status to `closed`
4. do not add `ready-for-review`
5. fast-forward the reconciler so downstream failures/cascades and epic completion are observed
This is stricter than the current code, which only emits `Completion` on success and otherwise stores failure/cancel purely in RAM (`crates/spur-mcp/src/plan/mod.rs:841-893`, `crates/spur-mcp/src/plan/mod.rs:2675-2728`).
### Review path
For persisted plans, `review_task` changes from:
- “mutate RAM and maybe dispatch”
to:
- “persist review decision and wake the reconciler”
That means:
- `approve`
  - close the task issue
  - remove `ready-for-review`
  - emit `Approval`
  - do not dispatch dependents directly
  - fast-forward the reconciler
- `reject`
  - close the task issue
  - remove `ready-for-review`
  - emit `Rejection`
  - do not dispatch anything directly
  - fast-forward the reconciler
- `request_changes`
  - keep the task issue `open`
  - remove `ready-for-review`
  - append the feedback comment
  - do not send a new `DelegationRequest` here
  - fast-forward the reconciler so it re-queues the task from beads state
This is a deliberate semantic change for reject.
Current code writes reject as `"open"` (`crates/spur-mcp/src/plan/mod.rs:2047-2058`), which is exactly why rejected tasks remain operationally eligible for the watcher.
Under beads-as-truth, a terminal reject cannot stay open without poisoning every downstream operational read.
### Signal watcher path
The signal watcher becomes a consumer of the same projection the reconciler uses.
The filtering rule for persisted plans becomes:
- task is in plan scope
- task is `open`
- task has at least one `signal:*` label
- task has no `spur:signal-processed:*` label for the signal being handled
- task has `ready-for-review`
That last bullet is the key behavioral fix.
It means:
- no mid-dispatch mutation while a worker still owns the task
- no mutation on a rejected task
- no mutation on a pending task that never reached review
This directly closes the current G1/G2 shape called out in rev 4, using the existing `READY_FOR_REVIEW` constant as an actual operational marker instead of a dead constant.
Current anchors:
- watcher current filters: `crates/spur-mcp/src/plan/signal_watcher.rs:76-104`
- `READY_FOR_REVIEW` constant with no writers: `crates/spur-mcp/src/plan/labels.rs:57`
The watcher’s projection input becomes the full projected plan.
The `MutationProposer` and `MutationScorer` seam does not change.
What changes is the quality of the `PlanState` fed into that seam.
### Cache semantics
`active_plans` remains useful.
It stops being authoritative for persisted plans.
Persisted-plan cache entries may store:
- the latest projected `PlanState`
- recomputed current diff text
- artifact handles
- merge base ref
- parsed audit history
- expensive ready-query results for one tick
But every persisted-plan cache entry must be disposable.
If it is missing, SPUR must rehydrate from beads plus audit replay.
That implies:
- `get_plan_status(plan_id)` on a persisted plan rehydrates on cache miss
- `review_task(plan_id, ...)` on a persisted plan rehydrates on cache miss
- `get_task_diff(plan_id, ...)` on a persisted plan rehydrates on cache miss
- `merge_plan(plan_id)` on a persisted plan rehydrates on cache miss
For ephemeral plans, `active_plans` stays authoritative until/unless a future spec unifies both paths.
### Restart recovery
Restart recovery is the point of the authority decision.
If restart cannot reconstruct the next legal action from beads, the decision was wrong.
The persisted-plan startup flow is:
1. Discover active persisted execution epochs from beads:
- epic/child `spur:plan-id:<id>` labels - epic `PlanSubmit` audit sentinels - open child tasks in that scope
2. Rehydrate projected state for each active plan.
3. Resolve orphaned mutation intent before running the watcher.
4. Resolve orphaned dispatch intent before running the reconciler.
5. Warm `active_plans` as cache entries.
6. Start the reconciler and watcher.
#### Mutation orphan rule
This carries forward rev 4 G7, but now it is part of the persisted-plan architecture, not a side note.
Rule:
- If a `MutationPlan` audit sentinel exists with no matching `MutationCommit`
or terminal rollback marker for the same `mutation_id`, that mutation is an orphan.
- SPUR must resolve the orphan before processing any new signal on the same parent task.
- Resolution is:
  - finish the remaining ops and emit `MutationCommit`, or
  - compensate and emit a cancel/failure terminal breadcrumb
Current anchor for the write-ahead part:
- `crates/spur-mcp/src/plan/mutation_executor.rs:31-41`
#### Dispatch orphan rule
This is new.
Rule:
- If a task is `open`,
carries `delegation-id:<id>`, and lacks both `ready-for-review` and a terminal completion breadcrumb for that delegation, then the dispatch is orphaned after restart.
Disposition:
- clear the `delegation-id` label
- leave the task `open`
- let the reconciler re-dispatch if the task is still ready
This is intentionally conservative.
SPUR does not attempt durable worker-session resumption in this spec.
That would require ACP-level durable worker ownership, which current orchestrator plumbing does not provide.
Current anchor for the existing completion bridge being process-local:
- `crates/spur-mcp/src/server.rs:947-1043`
### Epic completion rule
The epic is operationally complete when every child in the current plan scope is terminal in beads.
Not when:
- `run_plan` exits
- ACP emits `PlanCompleted`
- the brain happens to notice completion first
Those are projections.
The durable rule is:
1. Project all child states in `spur:plan-id:<id>` scope.
2. If every child is terminal:
- close the epic - emit an epic completion audit breadcrumb - if all children are approved, also emit/retain the ACP `PlanReadyToMerge` continuation path
3. If any child remains non-terminal:
- epic stays open
Why close the epic before merge/PR?
Because this epic represents operational plan execution.
Merge and PR creation are downstream integration actions, not worker-loop liveness.
That said, the epic completion breadcrumb must carry enough summary to make that distinction explicit:
- all approved / ready to merge
- terminal with failures/rejections
### Merge and diff recovery
The authority decision forces one additional persisted contract:
the merge base and reviewable branch metadata cannot remain RAM-only.
Today:
- `snapshot_plan_base()` returns an optional branch ref
(`crates/spur-mcp/src/server.rs:690-702`)
- that ref is stored only in `PlanState.base_snapshot_branch`
(`crates/spur-mcp/src/plan/mod.rs:154-173`)
- `merge_plan` errors if that field is absent
(`crates/spur-mcp/src/server.rs:2160-2168`)
That is incompatible with restart-safe persisted plans.
So the plan bootstrap audit for persisted plans must also carry:
- a stable base ref for merge reconstruction
- ideally a commit-ish rather than only a moving branch name
Likewise, current worker completion already persists `worker_branch` and summary in the `Completion` audit sentinel (`crates/spur-mcp/src/plan/mod.rs:654-658`), which is enough to rebuild review-on-demand for the current attempt.
Full diff text does not need to be stored in beads.
Current code already documents that historical attempts do not store full diff text and must be inspected through git (`crates/spur-mcp/src/server.rs:2860-2863`).
The persisted-plan rule therefore becomes:
- current-attempt review can be recomputed from `worker_branch` plus base ref on cache miss
- historical attempts may continue to expose summary/branch metadata without full diff text
### Relationship to ACP events and continuations
ACP events survive in this design.
They are not removed.
They are also not promoted to authority.
Specifically:
- `PlanCompleted` still exists as the brain re-entry signal for terminal plan execution
(`crates/spur-acp/src/domain/events.rs:607-624`)
- `PlanReadyToMerge` still exists as the brain re-entry signal for all-approved plans
(`crates/spur-acp/src/domain/events.rs:619-624`)
- `ContinuationSource::PlanCompleted` and `ContinuationSource::PlanReadyToMerge` still exist
(`crates/spur-acp/src/domain/continuation.rs:19-23`)
What changes is the producer.
For persisted plans, those events are emitted from projected durable state transitions, not from the exit condition of an in-memory executor loop.

---

## Data Contracts

### Contract stance
This section stays at method-surface level.
It does not dump Rust structs.
It names:
- the stable surface
- the authority
- the persisted markers
- the important payload fields
That matches the rev 4 “Stance C” discipline.
### Method-surface contracts

| Surface | Current persisted-plan behavior | Proposed persisted-plan contract |
|---|---|---|
| `submit_plan(persist_as_epic=true)` | Creates the epic/child graph, emits `PlanSubmit` on the epic, inserts a RAM `PlanState`, and spawns `run_plan` immediately (`crates/spur-mcp/src/server.rs:2408-2484`). | Creates the epic/child graph, stamps all execution labels, emits plan bootstrap audit including merge-base metadata, warms projection cache, and nudges the reconciler. No persisted `run_plan` spawn. |
| `execute_epic(epic_id, default_agent)` | Derives tasks from beads, inserts RAM state, and spawns `run_plan`. Does not persist `epic_id` into `PlanState` and does not normalize missing labels (`crates/spur-mcp/src/server.rs:2624-2729`). | Normalizes child execution labels/agents, persists the execution `plan_id`, emits plan bootstrap audit, warms cache, and nudges the reconciler. No persisted `run_plan` spawn. |
| `Reconciler::tick_once` | Reads ready IDs and logs them only (`crates/spur-mcp/src/plan/reconciler.rs:109-114`). | Reads ready IDs, projects plan scope, writes durable dispatch intent, sends `DelegationRequest`, compensates immediate send failures, and performs epic completion checks. |
| `review_task(plan_id, task_id, decision)` | Mutates in-memory state, writes beads update outside lock, and may dispatch immediately on approve/request-changes (`crates/spur-mcp/src/plan/mod.rs:1918-2455`). | Persists review decision into beads and audit, never dispatches persisted tasks directly, and fast-forwards the reconciler. Reject becomes `closed`, not `open`. |
| Worker completion writeback | Stores success/failure in RAM and emits `Completion` audit only on success (`crates/spur-mcp/src/plan/mod.rs:841-893`, `crates/spur-mcp/src/plan/mod.rs:2675-2728`). | Persists terminal worker result into beads for every terminal outcome, uses `delegation-id` / `ready-for-review` labels as the operational phase handoff, and emits completion audit for all outcomes. |
| `report_signal(task_id, signal)` | Writes audit-first, then signal sentinel comment, then `signal:<kind>` label (`crates/spur-mcp/src/server.rs:509-534`). | Keeps the same write order, but the resulting signal becomes eligible only when the task also carries `ready-for-review`. |
| `SignalWatcher::tick_once` | Scans every issue, filters by `signal:*` and absence of `spur:signal-processed:*`, then scores against `stub_plan_state()` (`crates/spur-mcp/src/plan/signal_watcher.rs:70-173`). | Projects the real persisted plan, filters to review-ready tasks only, handles one durable signal decision per task per tick, and respects restart-recovered orphan state before proposing. |
| `get_plan_status(plan_id)` | Reads only from `active_plans`; cache miss means “unknown plan” (`crates/spur-mcp/src/server.rs:2772-2799`). | On persisted-plan cache miss, rehydrates from beads/audit and then serves status from the projection. Ephemeral behavior stays unchanged. |
| `get_task_diff(plan_id, task_id)` | Reads only from `active_plans`; current-attempt diff comes from cached `DelegationResult`; historical attempts rely on summary/branch only (`crates/spur-mcp/src/server.rs:2801-2895`). | On persisted-plan cache miss, reconstructs review data from projected worker branch plus persisted base ref. Historical attempts may continue to expose summary/branch without full diff text. |
| `merge_plan(plan_id)` | Requires cached `base_snapshot_branch`, cached approvals, and cached `worker_branch` values (`crates/spur-mcp/src/server.rs:2130-2214`). | Rehydrates persisted plans from beads/audit on cache miss. Merge base becomes a persisted bootstrap field. |
| `create_pr(...)` | Manual tool only (`crates/spur-mcp/src/server.rs:2068-2102`). | Remains manual through v0d. Auto-hooking, if shipped, is opt-in and later. |

### Label vocabulary
The existing label helpers in `crates/spur-mcp/src/plan/labels.rs` remain the authoritative constructors.
That matters because label grammar and length behavior are asymmetric:
- `br create --label` enforces a 50-character cap
- `br label add` does not
Current anchor:
- `crates/spur-mcp/src/plan/labels.rs:7-23`
That asymmetry remains in force under this design.
The contract table is:

| Label | Current state | Proposed writer | Operational meaning under beads-as-truth | Create-path safety |
|---|---|---|---|---|
| `spur:plan-id:<id>` | Written at persisted plan creation time for `submit_plan` only (`crates/spur-mcp/src/server.rs:560-576`). | `submit_plan`, `execute_epic` normalization | Execution-epoch scope. Every task and the epic carry exactly one active execution `plan_id`. | Safe at create time. |
| `spur:plan-task-id:<id>` | Written for `submit_plan` child issues (`crates/spur-mcp/src/server.rs:572-576`). | `submit_plan`, `execute_epic` normalization | Stable task identity inside the projected plan. | Safe at create time. |
| `spur:agent:<name>` | Written for `submit_plan`; only read for `execute_epic` (`crates/spur-mcp/src/server.rs:572-579`, `crates/spur-mcp/src/plan/mod.rs:265-291`). | `submit_plan`, `execute_epic` normalization | Durable worker routing for restart-safe dispatch. | Safe at create time. |
| `spur:source-issue:<id>` | Written for persisted child tasks created from pre-existing issues (`crates/spur-mcp/src/server.rs:577-579`). | `submit_plan` only | Provenance link back to the source issue. | Safe at create time. |
| `spur:plan-complete` | Written on the epic after all child issues/deps are created (`crates/spur-mcp/src/server.rs:396-414`, `crates/spur-mcp/src/plan/labels.rs:58-63`). | `submit_plan` | Graph-persistence completeness marker. Still an epic-only label. | Added after create. |
| `delegation-id:<id>` | Constructor exists, but the repo currently has no writer beyond the helper definition (`crates/spur-mcp/src/plan/labels.rs:44-46`; repo search only finds the helper). | `Reconciler` dispatch; completion writeback removes it | Durable worker-ownership marker for an in-flight task attempt. | Added after create. |
| `ready-for-review` | Constant exists, but the repo currently has no writer beyond the constant definition (`crates/spur-mcp/src/plan/labels.rs:57`; repo search only finds the definition). | Completion writeback adds; review path removes | Durable handoff from worker ownership to brain review ownership. | Added after create. |
| `signal:<kind>` | Written by `report_signal` (`crates/spur-mcp/src/server.rs:527-534`). | Worker-facing signal path | Fast operational filter for watcher eligibility. | Added after create. |
| `signal:<kind>:<bucket>` | Helper exists but current `report_signal` only writes the non-bucketed kind label (`crates/spur-mcp/src/plan/labels.rs:48-54`, `crates/spur-mcp/src/server.rs:527-534`). | Optional worker-facing signal path | Optional severity bucketing for analytics/filtering. | Added after create. |
| `signal:late-arrival` | Written by late-path `report_signal` when the task is already closed (`crates/spur-mcp/src/server.rs:475-491`, `crates/spur-mcp/src/plan/labels.rs:56`). | `report_signal` | Durable record that a signal arrived after terminal closure. | Added after create. |
| `spur:signal-processed:<mutation-id>` | Written by successful mutation commit (`crates/spur-mcp/src/plan/mutation_executor.rs:185-193`). | Mutation executor | Durable “consumed” marker for signal-triggered mutation. | Not create-safe at 54 chars; add-only path. |
| `spur:mutation-id:<id>` | Written on mutation-created children (`crates/spur-mcp/src/plan/mutation_executor.rs:61-75`). | Mutation executor | Mutation batch membership. | Safe at create time because it uses compact UUID. |
| `spur:superseded-by:<child>` | Written on the superseded parent task (`crates/spur-mcp/src/plan/mutation_executor.rs:115-126`). | Mutation executor | Replacement-child lineage for closed superseded parents. | Added after create. |

#### Label-specific rules
Rule L1:
`delegation-id:<id>` and `ready-for-review` are mutually exclusive on the same task.
Rule L2:
`ready-for-review` only appears on an `open` task.
Rule L3:
`spur:plan-id:<id>` is unique per active execution epoch on a task.
For `execute_epic`, older execution labels must be replaced, not accumulated.
Rule L4:
`signal:*` labels are historical, not phase markers.
They are never sufficient on their own to make a signal operationally eligible.
Rule L5:
No operational code path may infer “worker currently running” from RAM alone for a persisted plan.
It must consult `delegation-id:<id>`.
### Audit sentinel contracts
The existing sentinel envelope remains:

```text
[[spur-audit v1]]
{ ...json payload... }
```

Current anchor:
- `crates/spur-mcp/src/plan/audit_sentinel.rs:13-128`
The table below describes the method-surface contract, not the Rust enum definition.

| Sentinel kind | Current writer and payload | Proposed persisted-plan role | Proposed delta |
|---|---|---|---|
| `PlanSubmit` | Written on the epic by `emit_plan_submit_audit`, payload `{ plan_id, epic_issue_id, task_ids }` (`crates/spur-mcp/src/server.rs:423-440`, `crates/spur-mcp/src/plan/audit_sentinel.rs:19-23`). | Plan bootstrap marker for each persisted execution epoch. Used for restart discovery and cache rehydration. | Extend payload with persisted merge-base metadata and execution mode details needed for restart-safe merge/review. |
| `Dispatch` | Written as an advisory task comment with `{ delegation_id, worker, attempt }` (`crates/spur-mcp/src/plan/mod.rs:607-639`). | Durable dispatch breadcrumb paired with `delegation-id:<id>`. Projection uses it for attempt counts and history. | No shape change required for v0c; add compensating breadcrumb only if immediate send fails. |
| `Completion` | Currently success-only, payload `{ delegation_id, worker_branch, result_summary }` (`crates/spur-mcp/src/plan/mod.rs:642-669`, `crates/spur-mcp/src/plan/audit_sentinel.rs:29-35`). | Durable worker-finished breadcrumb for every terminal worker result. | Extend payload with `completion_state = awaiting_review | failed | cancelled` and optional artifact reference. |
| `Approval` | Written on approve with `{ delegation_id }` (`crates/spur-mcp/src/plan/mod.rs:672-697`). | Review terminal breadcrumb for approved work. | Keep. |
| `Rejection` | Written on reject with `{ delegation_id, feedback }` (`crates/spur-mcp/src/plan/mod.rs:699-725`). | Review terminal breadcrumb for rejected work. | Keep; reject now pairs with closing the issue. |
| `Signal` | Written by `report_signal`, payload `{ signal_id, signal_kind, severity, reason }` (`crates/spur-mcp/src/server.rs:515-523`, `crates/spur-mcp/src/plan/audit_sentinel.rs:43-49`). | Historical record of worker-emitted signal independent of mutation outcome. | Keep. |
| `LateSignal` | Written by `report_signal` late path with `{ signal_id, terminal_status }` (`crates/spur-mcp/src/server.rs:475-491`, `crates/spur-mcp/src/plan/audit_sentinel.rs:66-69`). | Historical record that a late signal was observed and ignored operationally. | Keep. |
| `MutationPlan` | Written write-ahead before mutation ops (`crates/spur-mcp/src/plan/mutation_executor.rs:31-41`). | Mutation orphan detection and replay seed. | Keep. |
| `MutationCommit` | Written after successful mutation commit (`crates/spur-mcp/src/plan/mutation_executor.rs:175-183`). | Mutation success terminal breadcrumb. | Keep. |
| `MutationInvariantViolation` | Written on rollback/cycle failure with `{ mutation_id, violation, rollback_status }` (`crates/spur-mcp/src/plan/mutation_executor.rs:144-170`). | Mutation failure terminal breadcrumb. | Extend in v0d to include structured compensation results. |

#### Why no new audit transport is introduced
The current code already rejected `br audit record` and standardized on sentinel comments.
That remains correct.
Current anchor:
- `crates/spur-mcp/src/plan/audit_sentinel.rs:1-9`
The correct fix is not a new transport.
The correct fix is to make the existing audit surface complete enough for projection and recovery.
### Operational read contract
Under beads-as-truth, operational reads use:
- issue status from `PmService`
- labels
- dependencies
- `bv`/`br ready`
Current anchors:
- PM facade: `crates/spur-pm/src/service.rs:120-207`
- closed status vocabulary: `crates/spur-pm/src/service.rs:9-15`, `crates/spur-pm/src/service.rs:178-183`
- reconciler ready reads: `crates/spur-mcp/src/plan/reconciler.rs:119-185`
Operational code must not depend on:
- in-memory task status for persisted plans
- ACP lifecycle events
- audit history alone
### Analytical read contract
Analytical reads use:
- `[[spur-audit v1]]`
- `[[spur-signal v1]]`
They answer:
- why a task changed
- which delegation id owned a step
- which mutation created a split
- which signal triggered a mutation
- which branch a worker produced
Operational code may consult these comments to classify terminal states and rebuild attempts.
It may not let stale analytical history override current open/closed task reality.
If the graph and the audit disagree, the graph wins operationally.
### Plan bootstrap contract
Because `merge_plan` and `get_task_diff` currently rely on RAM-only fields, persisted plans need one explicit bootstrap contract.
Each persisted execution epoch must durably carry:
- `plan_id`
- `epic_issue_id`
- task issue ids in scope
- base snapshot ref or commit-ish
- enough data to distinguish `submit_plan` from `execute_epic` if behavior differs
The existing `PlanSubmit` sentinel is the natural home.
This keeps the contract on the existing audit surface instead of inventing a second bootstrap store.

---

## Phased Plan

### Phase summary
The work is split into:
- v0c: authority flip for persisted plans
- v0d: closure hardening and recovery completeness
- v0e: automation and codepath retirement
Each phase ships end-to-end value.
Each phase leaves the system in a testable state.
### Mapping rev-4 gaps onto phases

| Gap from rev 4 | Phase | Why |
|---|---|---|
| G1 `READY_FOR_REVIEW` missing writer | v0c | Required to make signal mutation respect worker/brain ownership boundaries. |
| G2 rejected tasks stay signal-eligible | v0c | Fixed by making reject terminal in beads and by watcher gating on `ready-for-review`. |
| G3 multi-signal-per-tick over-processing | v0c | Needs to change with the watcher rewrite because the watcher is already moving to plan projection. |
| G4 cross-restart retry loop after failed apply | v0c | Restart semantics must be defined at the same time as beads-as-truth recovery. |
| G7 mutation orphan resolution | v0c | Core authority flip is incomplete without restart orphan rules. |
| G5 `ISSUE_SCAN_LIMIT` saturation | v0d | Important hardening, but not a blocker for making persisted dispatch authoritative. |
| G6 rollback compensation payload too weak | v0d | Important recovery introspection, but downstream of the authority flip. |

### v0c — Persisted authority flip
#### Goal
Make beads authoritative for persisted-plan execution.
#### Deliverables
1. Persisted-plan projector
- one shared projection path for reconciler, signal watcher, and status/review surfaces - cache warm on submit/execute, rehydrate on cache miss
2. Reconciler-owned persisted dispatch
- no persisted `run_plan` dispatch - no persisted `dispatch_newly_ready` from `review_task`
3. Durable dispatch/review markers
- `delegation-id:<id>` writer - `ready-for-review` writer/remover
4. Persisted completion writeback
- success, failure, and cancel all become durable task state transitions
5. Review path conversion
- approve/reject/request_changes persist state only - reject becomes terminal in beads
6. Signal watcher projection rewrite
- replace `stub_plan_state()` - require `ready-for-review` - one durable signal decision per task per tick
7. Restart recovery
- rehydrate active persisted plans from beads - resolve mutation orphans - resolve dispatch orphans
8. `execute_epic` normalization
- persist resolved agents - persist execution `plan_id` - stop dropping `epic_id`
#### Out of scope for v0c
- pagination hardening for 10k+ issue scans
- richer rollback compensation payloads
- auto-merge or auto-PR
- event-driven reconciler
#### Testable user-visible progress
After v0c, the following flow must work end-to-end:
1. call `submit_plan(persist_as_epic=true)` or `execute_epic`
2. let reconciler dispatch ready tasks
3. let a worker finish successfully
4. see the task become `ready-for-review` in beads
5. emit a worker signal
6. see the signal watcher project the real plan and mutate only if the task is review-ready
7. call `review_task(approve|reject|request_changes)`
8. observe no direct re-dispatch from the review call itself
9. restart the server mid-plan
10. observe the plan continue from beads state without relying on the old `active_plans`
#### Acceptance tests
- T-v0c-1 persisted `submit_plan` does not spawn persisted direct dispatch
- T-v0c-2 reconciler dispatch writes `delegation-id` and `Dispatch`
- T-v0c-3 completion success writes `ready-for-review` and `Completion`
- T-v0c-4 reject closes the task and does not remain watcher-eligible
- T-v0c-5 request-changes leaves task open and reconciler redispatches it
- T-v0c-6 signal watcher uses projected plan state, not stub state
- T-v0c-7 restart rehydrates a persisted plan from beads on cache miss
- T-v0c-8 orphaned `delegation-id` is re-queued on restart
- T-v0c-9 orphaned `MutationPlan` is completed or compensated before new signals run
### v0d — Closure hardening
#### Goal
Finish the hard edges the authority flip exposes.
#### Deliverables
1. Epic auto-close
- close epic when all scoped children are terminal - emit explicit epic completion breadcrumb - preserve `PlanReadyToMerge` continuation on all-approved outcome
2. Persisted merge/review bootstrap
- finalize plan bootstrap payload - make `merge_plan` and `get_task_diff` fully rehydratable on cache miss
3. Pagination for mutation scans
- close G5
4. Structured rollback compensation audit
- close G6
5. Signal retry hardening
- finalize signal retry markers for rollback/failure outcomes if v0c leaves a temporary coarse rule
6. Cache invalidation discipline
- prove persisted-plan cache cannot become a shadow authority
#### Testable user-visible progress
After v0d, the following must work:
1. run a persisted plan to terminal
2. observe epic closure derived from child terminal states
3. restart after all tasks are approved
4. call `merge_plan`
5. see merge reconstruction succeed from persisted bootstrap metadata
#### Acceptance tests
- T-v0d-1 epic closes when all child tasks are terminal
- T-v0d-2 all-approved epic still yields `PlanReadyToMerge`
- T-v0d-3 `merge_plan` works after restart on a persisted plan
- T-v0d-4 `get_task_diff` works after restart for the latest attempt
- T-v0d-5 mutation scans paginate past 10k issues
- T-v0d-6 rollback audit payload enumerates succeeded/failed compensations
### v0e — Automation and retirement
#### Goal
Remove remaining ambiguity and optional manual glue.
#### Deliverables
1. Retire persisted direct-dispatch codepaths
- persisted `run_plan` use disappears - `dispatch_newly_ready` becomes ephemeral-only or is deleted
2. Optional auto-hook from all-approved plan completion to merge/PR workflow
- opt-in only - remains outside the core authority decision
3. Event-driven reconciler/watcher wakeups where possible
4. Cleanup of compatibility shims added in v0c/v0d
#### Testable user-visible progress
After v0e, the codebase should make the authority decision obvious from structure:
- persisted plans project from beads
- persisted dispatch happens only in the reconciler
- review does not dispatch
- restart path and live path use the same projector
#### Acceptance tests
- T-v0e-1 no persisted path calls direct dispatcher helpers
- T-v0e-2 optional auto-merge/PR path stays behind configuration
- T-v0e-3 event-driven fast-forward does not change correctness relative to polling

---

## Invariants

This section restates the rev-4 invariant set for the persisted-plan authority decision.
The goal is not to invent a new numerology.
The goal is to state the deltas explicitly.
### I1 — Durable recoverability before next action
Rev 4 I1 was mutation-focused:
- write-ahead before destructive mutation op
- restart orphan resolution
That still stands.
Under beads-as-truth for persisted plans, I1 broadens:
**I1**
Before SPUR takes a persisted-plan action whose effects would matter after crash, it must leave enough durable state in beads plus audit to recover the next legal action.
This applies to:
- mutation batches
- dispatch intent
- worker completion handoff
- review decisions
- plan bootstrap
Concrete consequences:
- `MutationPlan` remains write-ahead before mutation ops
- dispatch writes a durable in-flight marker before the transport send
- completion writes a durable handoff marker before the task can be reviewed
- persisted-plan bootstrap writes merge/recovery metadata before the plan is considered active
Delta from rev 4:
- I1 is no longer mutation-only
- dispatch orphan handling joins mutation orphan handling under the same recoverability principle
### I2 — Graph legality is checked in beads, not inferred from RAM
Rev 4 I2 was post-mutation acyclicity.
That remains true.
Under beads-as-truth, the important delta is:
the authoritative graph being checked is the beads graph itself.
Not an in-memory copy that may have drifted.
**I2**
Any persisted mutation that changes dependency edges must leave the beads graph acyclic, and the projector must derive readiness from that graph rather than from stale in-memory status.
Delta from rev 4:
- readiness derivation now explicitly belongs to the same graph authority as cycle checking
### I3 — Ownership and late-signal safety
Rev 4 I3 focused on late-signal safety using `closed_status()`.
That stays.
Under beads-as-truth, I3 gets a stronger ownership rule:
**I3**
For persisted plans, signals may only drive mutation while the task is brain-owned.
Operationally, brain-owned means:
- task is `open`
- task carries `ready-for-review`
- task does not carry `delegation-id:<id>`
Worker-owned means:
- task carries `delegation-id:<id>`
- task does not carry `ready-for-review`
Closed tasks remain late-signal territory and are still gated by `PmService::closed_status()`.
Delta from rev 4:
- I3 is no longer just “closed tasks cannot mutate”
- it now also defines the worker/brain handoff boundary for open tasks
### I4 — Single brain, single persisted dispatcher
Rev 4 I4 was single brain session per `.beads/`.
That stays.
Under beads-as-truth, the architectural corollary becomes explicit:
**I4**
For persisted plans, there is exactly one authoritative dispatcher in the brain process: the reconciler.
No other codepath may directly dispatch persisted work.
Current single-brain anchor:
- pidfile and server startup comments: `crates/spur-mcp/src/server.rs:249-253`, `crates/spur-mcp/src/server.rs:1243-1246`
Delta from rev 4:
- I4 now includes a single-dispatcher rule for persisted execution
### I5 — Vocabulary compression still rules
Rev 4 I5 already made the important point:
beads status is lossy.
That remains unchanged.
Current anchor:
- `PmService::closed_status()`: `crates/spur-pm/src/service.rs:178-183`
- signal/report code already uses it: `crates/spur-mcp/src/server.rs:468-476`, `crates/spur-mcp/src/plan/signal_watcher.rs:77-87`
**I5**
For persisted plans, the beads status column answers only terminal/non-terminal questions.
Fine-grained runtime phase is reconstructed from:
- labels
- audit sentinels
- dependency graph
Never from imaginary beads status strings.
Delta from rev 4:
- I5 now governs full persisted-plan projection,
not only mutation/signal paths
### Additional derived rule: cache non-authority
This is not a new numbered invariant.
It is a direct consequence of I1 through I5.
Rule:
If a persisted-plan cache entry disagrees with freshly projected beads state, the cache is wrong by definition.
The cache must be replaced.
Not reconciled.
Not merged.
Replaced.

---

## Risks & Open Questions

This section is intentionally blunt.
The authority decision is correct, but it forces some questions into the open.
### R1. What happens to the orchestrator channel?
Answer:
it stays.
It is still the right place to hand work from `spur-mcp` into `spur-core`.
Current anchors:
- server owns `delegation_tx`: `crates/spur-mcp/src/server.rs:197`
- plan code already sends `DelegationRequest` into that channel: `crates/spur-mcp/src/plan/mod.rs:782-793`
- `spur-core` owns the actual worker execution path: `crates/spur-core/src/orchestrator.rs:2778-2788`
What changes is not the transport.
What changes is the authority boundary:
- channel send is transport
- beads markers are truth
Risk:
the ordering around durable dispatch intent and channel send is subtle.
Mitigation:
- persist dispatch intent first
- compensate immediate send failure in-process
- clear orphan dispatch intent on restart
Open question:
Do we want a dedicated compensating audit sentinel for failed immediate sends, or is warning + label clear sufficient for v0c?
My recommendation:
warning + label clear is sufficient for v0c, provided restart orphan handling is implemented and tested.
### R2. What happens to worker completion notification flow?
There are two completion flows today.
1. The low-level delegation completion bridge for detached worker results
(`crates/spur-mcp/src/server.rs:947-1043`)
2. The plan lifecycle event flow where `PlanCompleted` and `PlanReadyToMerge`
are emitted from the executor loop (`crates/spur-mcp/src/plan/mod.rs:1012-1079`)
Under this design:
- the low-level worker completion bridge stays
- persisted-plan writeback happens immediately off that bridge
- plan lifecycle events become projections of the durable task graph
Risk:
if completion is persisted but ACP event emission fails, the brain may miss a continuation turn even though the durable plan state is correct.
Mitigation:
- treat ACP continuation as a derived delivery mechanism
- make the brain able to poll/recover from durable plan state on the next turn
Open question:
Should epic completion and `PlanReadyToMerge` emission live in the reconciler, or in a thin “persisted-plan lifecycle projector” invoked by both reconciler and completion writeback?
My recommendation:
keep one small lifecycle projector helper in `spur-mcp` and call it from both reconciler and completion/review writeback paths.
The rule should be “derive from projected beads state”, not “derive in whichever codepath happened to notice first”.
### R3. Does the MCTS scoring path in `run_plan` survive?
There is no real MCTS scoring path in `run_plan` today.
That is the important correction.
Current anchors:
- `run_plan` is a DAG dispatcher, not a scorer: `crates/spur-mcp/src/plan/mod.rs:730-909`
- the mutation scorer seam lives in `proposers.rs`: `crates/spur-mcp/src/plan/proposers.rs:13-103`
- the signal watcher is the current caller of `MutationScorer::score`: `crates/spur-mcp/src/plan/signal_watcher.rs:136-145`
So the real question is:
does the scorer seam survive the persisted authority flip?
Answer:
yes, and it improves.
The seam survives unchanged.
What changes is the state passed into it:
- today: `stub_plan_state()`
- after v0c: projected persisted `PlanState`
That is strictly better for future MCTS work.
Open question:
Do we need richer projected history than the current audit/comment set to support future MCTS reward attribution?
My recommendation:
not for v0c.
The current scorer seam only needs the real projected graph.
If later MCTS experiments need more reward context, they can extend audit payloads in v1 without reopening the authority decision.
### R4. Merge-base persistence is easy to miss
This is the largest non-obvious knock-on effect of downgrading `active_plans`.
Today, merge reproducibility depends on `base_snapshot_branch` stored only in `PlanState` (`crates/spur-mcp/src/plan/mod.rs:160-163`, `crates/spur-mcp/src/server.rs:2160-2168`).
If that value is not persisted, `merge_plan` after restart will either fail or silently use the wrong base.
Mitigation:
- persist the merge base as part of plan bootstrap
- prefer an immutable commit-ish over a moving branch name
Open question:
Should the bootstrap store the branch ref returned by `snapshot_plan_base()`, or should it immediately resolve that branch to an OID for persisted plans?
My recommendation:
store both if cheap, but treat the OID as authoritative for recovery.
### R5. `get_task_diff` after restart needs a principled degradation model
Current `get_task_diff` uses cached `DelegationResult` for the latest attempt and already admits that historical attempts do not store full diff text (`crates/spur-mcp/src/server.rs:2828-2864`, `crates/spur-mcp/src/server.rs:2869-2895`).
Under beads-as-truth, that asymmetry becomes more visible.
Mitigation:
- reconstruct current-attempt diff from `worker_branch` and persisted base ref on cache miss
- keep historical-attempt behavior as summary/branch only unless a later design persists richer artifacts
Open question:
Do we need to persist artifact pointers in the `Completion` audit payload now, or can that wait for v0d?
My recommendation:
include an optional artifact reference in the completion payload as soon as the completion contract changes, because it is additive and makes restart review materially better.
### R6. `execute_epic` normalization mutates user-owned issues
This is unavoidable if persisted execution is to be restart-safe.
Current `execute_epic` can use a caller-supplied `default_agent` without persisting that choice (`crates/spur-mcp/src/plan/mod.rs:265-281`).
That is fine only while RAM owns the execution truth.
Under beads-as-truth, the normalized labels are not incidental metadata.
They are recovery-critical state.
Risk:
some users may not want SPUR to rewrite existing labels on an epic they created by hand.
Mitigation:
- document `execute_epic` as an execution-epoch normalization step
- keep the normalized label vocabulary limited to SPUR-prefixed machine labels
- do not overwrite non-SPUR labels
Open question:
Should `execute_epic` preserve prior `spur:plan-id:*` labels for history, or replace them?
My recommendation:
replace them on issues, keep history in audit comments.
Operational scope labels should describe the current execution epoch, not become an append-only log.
### R7. Epic auto-close before PR creation may surprise users
This is a semantics question, not a correctness question.
The proposed rule closes the epic when worker-loop execution is terminal.
That means an all-approved plan may have:
- closed epic
- no merge branch yet
- no PR yet
I still think that is the correct operational model.
Why:
- the epic here is the execution graph,
not the final Git integration state
- ACP already has a separate `PlanReadyToMerge` event for the “now integrate” step
(`crates/spur-acp/src/domain/events.rs:619-624`)
Mitigation:
- make the epic completion comment explicit about the outcome
- retain manual merge/PR through v0d
### R8. The projector must be fail-closed on ambiguous state
This is an implementation risk.
If the projector sees:
- closed task with no recognizable terminal breadcrumb
- both `delegation-id:*` and `ready-for-review`
- multiple active `spur:plan-id:*` labels
then the safe behavior is:
- mark the task non-dispatchable
- surface a warning
- refuse to “guess” a more convenient state
If the projector guesses, it becomes a new hidden authority.
That would violate the entire design.

---

## Future Work

This section is intentionally outside v0c/v0d/v0e closure scope.
- Auto-merge and auto-PR on all-approved plans.
Keep opt-in even if implemented.
- Event-driven reconciler and watcher wakeups via file-tail or future beads watch surface.
- Richer persisted review artifacts for historical attempts.
- Multi-brain coordination over the same beads graph.
- More expressive signal types beyond scope drift.
- Full ephemeral/persisted plan-path unification after the persisted authority model proves stable.

---

## Final Position

The design choice is not subtle:
for persisted plans, beads must own operational truth.
Anything else preserves the current split:
- RAM dispatch
- beads observation
- RAM review state
- beads signal state
- RAM restart loss
That split is the bug.
The fix is not “more synchronization”.
The fix is one authority, one dispatcher, one projection model, and explicit restart rules for the moments where beads has durable intent but transport or process lifetime does not.
That is the end-to-end closure design.
