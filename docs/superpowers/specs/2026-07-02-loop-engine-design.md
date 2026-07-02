# Loop Engine — Standing, Self-Re-Arming Plans on Top of the Plan Engine

**Status:** Draft for review
**Date:** 2026-07-02
**Grounding:** first-principles decomposition cross-checked against `crates/spur-core/src/plan/` (worktree graph, `graph_content_hash 340a92e5…`) and the externally indexed `cobusgreyling/loop-engineering@main` reference (`external_index` snapshot 3038, 217 files).

---

## 0. First-Principles Decomposition

Strip the "loop engineering" vocabulary to its irreducible parts. A loop is:

| # | Irreducible part | Definition |
|---|---|---|
| 1 | **Standing goal** | An intent that outlives any single execution |
| 2 | **Trigger** | A rule deciding when to look at the world again (cadence in v1) |
| 3 | **Discovery** | A step converting world-state into concrete work items |
| 4 | **Execution** | A substrate that performs work with independent verification |
| 5 | **Memory** | Durable state connecting run *N* to run *N+1* |
| 6 | **Governors** | Bounds on damage when nobody is watching (budget, autonomy, kill switch, backoff) |

The SPUR plan engine already **is** parts 4 and 5 — and a hardened version of them:
the reconciler (`Reconciler::tick_once`, `plan/reconciler/mod.rs:1120`) is a convergence
loop over beads state; review gate + `review_task` is the maker/checker split; beads +
`[[spur-audit v1]]` sentinels are the durable memory; worktree isolation with
pre-dispatch overlay preview handles parallel collision.

What the plan engine lacks is exactly parts 1, 2, 3, and 6:

- **Plans are one-shot.** `reconcile_terminal_epics` (`plan/reconciler/terminal.rs:109`)
  classifies completion, closes the epic, emits `EpicCompletion` + `PlanCompleted`, and
  nothing ever re-arms.
- **No trigger.** Nothing in the engine fires on a cadence with durable schedule state.
- **No discovery.** Tasks are authored by the brain at submit time; no sanctioned path
  derives work from world-state on a schedule.
- **No governors.** `plan_allows_dispatch` (`plan/reconciler/guards.rs:8`) gates only on
  epic state and ownership. Cost is tracked per delegation
  (`DelegationResult.estimated_cost_usd` → `estimated_cost_micros` in
  `outcome_materializer.rs`) but **nothing consumes it as a gate**, the `Completion`
  audit sentinel does not persist it, and there is no pause/kill switch.

**Core thesis.** The Loop Engine is *not* a new executor. It is a thin lifecycle
algebra over plans:

```
Loop(spec) = repeat [
    instantiate(template)          -- a normal plan ("generation")
  → execute(existing plan engine)  -- reconciler / review gate / leases, untouched
  → record(run)                    -- durable run record on the loop issue
  → govern(budget / backoff / autonomy)
  → re-arm(trigger)
]
```

Every generation is an ordinary plan and reuses 100% of the dispatch, review, signal,
mutation, and conflict machinery. The Loop Engine only adds the wrapper.

## 1. Design Decisions (with rationale)

### D1 — Generation-per-cycle, not one eternal plan

Rejected alternative: a single long-lived plan that grows via mutations forever.
Reasons:

- `plan_allows_dispatch` requires an **open** `spur:plan-complete` epic; an eternal plan
  fights `reconcile_terminal_epics` permanently.
- The projector re-derives plan state from *all* beads comments per poll; an unbounded
  epic makes every tick more expensive (state-rot failure mode, loop-engineering
  `docs/failure-modes.md::State Rot`).
- Generation-per-cycle gives the reference repo's "prune every run" rule for free:
  old generations are closed epics.

Loop-level memory lives on a dedicated **loop issue**, not the generation epics.

### D2 — Trigger and planning are separated by autonomy level

The engine's scheduler handles **trigger + governor enforcement + run records**
(deterministic, no LLM in the engine — ever). Who authors generation *N+1* depends on
the loop's autonomy level, mirroring loop-engineering's L1→L3 upgrade path
(`docs/loop-design-checklist.md::Readiness Levels`) with a literal mechanical meaning:

| Level | Generation instantiation | Dispatch behavior | Merge |
|---|---|---|---|
| **L1 report** | Brain-armed: engine pushes a *loop-due* continuation; brain reviews loop state and submits the generation | Triage task only; discovered action tasks are **suppressed** (`ReportOnly`) | n/a |
| **L2 assisted** | Brain-armed (same as L1) | Action tasks dispatch normally through review gate | Manual (`auto_merge_approved_plans = false`) |
| **L3 unattended** | **Engine-armed**: stored `PlanSpec` template re-submitted verbatim by the engine | Full dispatch | Auto-merge within allowlist only |

L1/L2 arming needs zero new persistence: the continuation is a sibling of
`push_plan_completed_continuation` (`plan/mod.rs:3141`) using the existing
`DetachedContinuationCtx` + `OutcomeMaterializer` path. L3 arming requires extracting
the epic/task persistence from `McpCallbackServer::submit_plan_as_epic_internal`
(`server/handlers/plan.rs:661`) into a core-callable `crate::plan::persist_plan_as_epic`
so the `Reconciler` (which holds `pm`, `feature_gate`, `dispatch` but not the server)
can instantiate generations.

**Rule (from the reference's own discipline): a loop must run ≥ N stable generations at
L1 before L2, and at L2 before L3. The engine enforces the ratchet; a human (or brain
with explicit user approval) flips the level.**

### D3 — Discovery is a worker task, never engine logic

Each generation's DAG has a mandatory first task: the **triage task** (cheap agent,
read-mostly). Its only write channels are `submit_plan_mutation` (add tasks to its own
generation) and `report_signal` — both already governed, audited, and rollback-capable
via the mutation executor. This encodes the reference's "triage skill must not invent
architectural work — signal only" rule structurally: the engine never authors tasks,
and triage output is bounded by a governor (`max_tasks_per_generation`) enforced in the
mutation executor path.

Empty discovery → triage reports no-op → generation completes with zero action tasks →
run record says `found: 0` → cheap cycle (the reference's "empty watchlist → exit in
<5k tokens" best practice).

### D4 — Governors live in `plan_allows_dispatch`

`PlanDispatchState` (`plan/reconciler/mod.rs:818`) is the single choke point every ready
task passes through, with `skip_reason()` already feeding observability. Extend it:

```rust
pub enum PlanDispatchState {
    Allowed,
    PlanMissingCompleteEpic,
    EpicNotOpen { epic_id: String },
    PlanHasPendingEpic { epic_id: String },
    PlanOwnedByAnotherBrain { epic_id: String, owner: String },
    // new
    ReportOnly { epic_id: String },                     // L1: task is not the triage task
    BudgetExhausted { spent_micros: u64, cap_micros: u64 },
    LoopsPaused { scope: PauseScope },                  // per-loop or global
}
```

Each new state maps to a new `SkipReason` variant, so suppressions are recorded exactly
like today's skips.

### D5 — Cost becomes durable on the Completion sentinel

Budget gating needs restart-safe per-plan spend. Add an optional
`estimated_cost_micros: Option<u64>` to `AuditSentinelKind::Completion`
(`plan/audit_sentinel.rs:71`), populated from `DelegationResult.estimated_cost_usd`
via the existing `usd_to_micros_saturating` (`outcome_materializer.rs:558`). Serde
default keeps old comments parseable (round-trip test required per repo convention).
The budget check sums `Completion` costs across the loop's generations in the current
window; the projector already collects sorted audits per issue.

### D6 — Overlap and failure semantics (control-theory stability)

- **Overlap:** if generation *N* is still live when the trigger fires, do **not** start
  *N+1*. Record `skipped_overlap`, push next-due forward one cadence. Cadence is a
  *minimum interval*. (Reference: "one owner per branch"; also avoids worktree/base
  collisions SPUR already guards against.)
- **Failure backoff:** an unattended loop with a persistent failure is a
  positive-feedback token burner. After `k` consecutive generations ending non-approved,
  multiply the interval (reuse the reconciler's `base_interval` / `idle_ceiling` /
  `backoff_factor` idiom, `ReconcilerConfig`, `plan/reconciler/mod.rs:776`); after `K`,
  **auto-pause the loop** and push an escalation continuation. Self-damping by design.
- **Kill switch:** `spur:loop-paused` label on the loop issue (per-loop) and a global
  pause (config flag + `spur:pause-all-loops` sentinel issue label). Checked in the
  scheduler sweep *and* in `plan_allows_dispatch` (pausing mid-generation stops new
  dispatches; in-flight delegations finish and persist normally).

## 2. Data Model

### 2.1 The loop issue

A beads issue (issue_type `task`, marked with `spur:loop-id:<compact-uuid>`) — the
loop's identity, memory spine, and human control surface. Its body carries the spec
sentinel:

```
[[spur-loop v1]]
{
  "loop_id": "<compact-uuid>",
  "goal": "Keep CI green on main",
  "pattern": "ci-sweeper",
  "cadence_secs": 3600,
  "autonomy": "l1",
  "template": { /* PlanSpec: tasks incl. triage task, agents, context_files */ },
  "governors": {
    "max_cost_micros_per_generation": 2000000,
    "max_generations_per_day": 24,
    "max_tasks_per_generation": 5,
    "denylist_globs": ["**/auth/**", "**/secrets*"],
    "consecutive_failure_backoff": { "k": 2, "factor": 2, "auto_pause_after": 4 }
  },
  "escalation": { "after_unresolved_generations": 3 }
}
```

### 2.2 Label vocabulary (extends `plan/labels.rs`)

| Label | On | Purpose |
|---|---|---|
| `spur:loop-id:<compact-uuid>` | loop issue + every generation epic | Loop identity / join key |
| `spur:loop-next-run:<epoch>` | loop issue | Durable trigger state (same idiom as `spur:lease-expires-at:`) |
| `spur:loop-generation:<n>` | generation epic | Ordering, overlap detection |
| `spur:autonomy:<l1\|l2\|l3>` | loop issue (copied to generation epic at instantiation) | Governor input |
| `spur:loop-paused` | loop issue | Per-loop kill switch |
| `spur:pause-all-loops` | designated control issue | Global kill switch |
| `spur:loop-triage-task` | triage task issue | Exempts it from `ReportOnly` suppression |

All values respect br's 50-char label cap (compact UUID suffix, like
`mutation_id_label`).

### 2.3 Run records — the durable run log

Appended to the **loop issue** by the engine when a generation reaches terminal state
(hook: the `EpicCompletion` branch of `reconcile_terminal_epics`):

```
[[spur-audit v1]]
{
  "kind": "loop-run",
  "loop_id": "...",
  "generation": 7,
  "plan_id": "...",
  "outcome": "approved|partial|failed|skipped_overlap|budget_exhausted|report_only",
  "tasks_discovered": 3,
  "approved": 2, "rejected": 0, "failed": 1, "cancelled": 0,
  "escalations": 0,
  "cost_micros": 812000,
  "started_at": 1782950000, "ended_at": 1782953600
}
```

This is loop-engineering's `loop-run-log.md` (`docs/operating-loops.md::Logging Each
Run`) realized in beads: append-only, queryable, and survives restarts. It also
satisfies `loop-audit`'s v1.4 "loopActivity — dynamic proof" signal.

## 3. Components

### 3.1 `LoopScheduler` sweep (new: `plan/loops/scheduler.rs`)

A per-tick sweep inside `Reconciler::tick_once`, sibling of
`sweep_expired_dispatch_leases` and `run_index_hygiene_sweep`:

```
for loop_issue in list_issues(label_prefix spur:loop-id, open):
    if global_pause || loop_paused(loop_issue):    continue (record LoopsPaused once)
    if now < parse_loop_next_run(labels):          continue
    if live_generation_exists(loop_id):            record skipped_overlap; bump next-run; continue
    if generations_today >= max_generations_per_day: record budget skip; bump next-run; continue
    match autonomy:
        L1 | L2 => push loop-due continuation (brain authors generation N+1)
        L3      => persist_plan_as_epic(template)   (engine-armed, verbatim template)
    bump spur:loop-next-run by effective_interval (cadence × backoff multiplier)
```

Time comes from the reconciler's existing `Clock` abstraction (already used in
`tick_once`), keeping the sweep testable under paused tokio / madsim like the rest of
the reconciler suite.

### 3.2 Generation lifecycle hook (edit: `plan/reconciler/terminal.rs`)

Where `EpicCompletion` is emitted for an epic carrying `spur:loop-id:*`:
compute the run record, append it to the loop issue, update the backoff counter,
auto-pause + escalate if the threshold is hit. No change to non-loop plans.

### 3.3 Governors (edit: `plan/reconciler/guards.rs`, `plan/reconciler/mod.rs`)

New `PlanDispatchState` arms per D4, checked in this order after the existing
epic/ownership gates: global pause → loop pause → budget → autonomy. `ReportOnly`
suppresses every ready task in an L1 generation except the one labeled
`spur:loop-triage-task`.

### 3.4 Core plan persistence extraction (refactor: `server/handlers/plan.rs` → `plan/`)

Extract the epic+tasks+labels+`PlanSubmit`-audit persistence from
`submit_plan_as_epic_internal` into `crate::plan::persist_plan_as_epic(pm, feature_gate,
spec, provenance) -> PlanId`, leaving the handler as a thin wrapper (validation +
dedup + response shaping stay in the handler). Pure move-refactor; behavior covered by
existing `submit_plan_persist` tests before the move (TDD: characterization first).

### 3.5 MCP surface (new tools in `mcp/plan.rs` + handlers)

| Tool | Action |
|---|---|
| `submit_loop` | Create loop issue with `[[spur-loop v1]]`, validate template (triage task present, governors sane), set first next-run |
| `get_loop_status` | Loop spec + last N run records + effective backoff + pause state |
| `pause_loop` / `resume_loop` | Toggle `spur:loop-paused` (resume recomputes next-run, resets backoff) |
| `set_loop_autonomy` | Ratcheted L1→L2→L3 promotion; demotion always allowed |
| `kill_loop` | Close loop issue with `status: retired` run record (reference's kill checklist) |

### 3.6 Events (edit: `spur-acp` event types)

New `SpurEventBody` variants: `LoopArmed`, `LoopGenerationStarted`, `LoopRunRecorded`,
`LoopPaused`. **Invariant compliance:** payloads are small and bounded (ids + counters,
run record referenced by audit, never embedded diffs/logs) per the broadcast-sizing
invariant; they ride the existing `SpurEvent.seq` allocator; they are engine events, not
ACP notifications, so the trailing-notification grace window is unaffected. Round-trip
serialization tests required (modeled on `executor_events_roundtrip.rs`).

## 4. What we deliberately do NOT build

- **No LLM calls inside the engine.** Discovery/judgment is always a worker or the
  brain. The engine is deterministic and simulation-testable.
- **No engine-authored tasks.** Even at L3 the engine only re-submits a human/brain
  authored template verbatim; new work enters only via the triage worker's governed
  mutations.
- **No event-driven triggers in v1** (CI webhook, PR opened, …). Cadence only. The
  trigger seam (`spur:loop-next-run`) is deliberately a label so an event source can
  later set it to `now` without schema change.
- **No new state store.** Beads is the only source of truth (spur-way). No
  `STATE.md`-style files.
- **No cross-loop scheduler fairness in v1.** `max_generations_per_day` +
  overlap-skip bound the damage; a global token budget across loops is future work.

## 5. Failure-Mode Coverage (vs. the reference catalog)

| Failure mode (loop-engineering) | Loop Engine answer |
|---|---|
| Infinite fix loop | Existing 1-auto-retry + `signal:escalated`; loop-level consecutive-failure backoff + auto-pause (D6) |
| State rot | Generation-per-cycle (D1); closed epics are the pruned history; run records are append-only and bounded per entry |
| Verifier theater | Existing review gate; spec mandates the triage/action agents differ from the reviewer (brain); harden `brain-review-gate` skill text ("find reasons to reject", must run tests) — skill change, not engine change |
| Notification fatigue | Loop events are engine events; continuations fire only on due/escalation/completion, honoring existing notification grace invariant |
| Token burn | Budget governor (D5) + cheap triage-first DAG shape (D3) + `max_generations_per_day` |
| Over-reach | `denylist_globs` governor checked at mutation acceptance + `max_tasks_per_generation`; L1 default for new loops |
| Comprehension debt / cognitive surrender | L-level ratchet (D2): a human decision is structurally required to increase autonomy; L1/L2 keep the brain authoring every generation |
| Parallel collision | Overlap-skip (D6) + existing ownership/overlay-preview machinery |
| Escalation failure | Auto-pause pushes a continuation (not just a label); `escalation.after_unresolved_generations` mirrors "surfaced 3+ days without resolution" |

## 6. Phasing (TDD; each phase independently valuable)

1. **Governors first** — `LoopsPaused` (global kill switch) + `BudgetExhausted` +
   `ReportOnly` dispatch states with skip recording. Standalone value for *existing*
   plans (a runaway plan can be paused today). Failing tests: guard-level unit tests in
   `plan/reconciler/tests.rs`.
2. **Durable cost + run records** — `estimated_cost_micros` on `Completion` sentinel
   (round-trip test), `loop-run` audit variant, emission hook in
   `reconcile_terminal_epics` behind the loop label.
3. **Scheduler + L1 loops** — loop issue schema, label vocabulary, `LoopScheduler`
   sweep, loop-due continuation, `submit_loop`/`get_loop_status`/`pause_loop` tools.
   Madsim test: cadence fire, overlap skip, restart durability of `spur:loop-next-run`.
4. **L2** — action-task dispatch in generations, backoff + auto-pause, `resume_loop` /
   `set_loop_autonomy` ratchet.
5. **L3** — `persist_plan_as_epic` extraction (characterization tests first),
   engine-armed instantiation, auto-merge allowlist integration with the existing
   `auto_merge_approved_plans` + `PlanAutomation` path.

Never skip a phase for a production loop — the phasing *is* the reference's upgrade
path (`docs/operating-loops.md::Upgrade Path`) enforced by the ratchet.

## 7. Risks & Open Questions

- **Projector cost growth on the loop issue.** Run records accumulate one comment per
  generation. Mitigation: records are small; add archival ("roll up records older than
  30 days into a summary comment") if a loop exceeds ~1k generations. Open.
- **Brain-armed loops need a live brain session.** By design for L1/L2 (headless
  standing work is exactly what L3 is for), but worth stating in user docs: an L1 loop
  whose brain session is gone parks at "due" until a brain attaches — the continuation
  materializer already handles detached delivery; verify retention semantics. Open.
- **Feature gating.** `reconcile_terminal_epics` requires `PM_PRO_BEADS_ADVANCED`;
  loops inherit that requirement (run records need `advanced()` comments). Decide the
  license key for the loop feature itself (`PM_PRO_LOOPS`?). Open.
- **Multi-brain ownership of loops.** Generations inherit the existing plan-owner
  labels, so two brains cannot both dispatch a generation; but which brain receives the
  loop-due continuation for an unowned loop is undefined. v1: the loop issue records a
  `spur:plan-owner:` at `submit_loop` time; unowned loops park. Open for multi-brain.

## 8. Provenance

- Reference concepts: `cobusgreyling/loop-engineering@main` — `docs/concepts.md`,
  `docs/primitives.md`, `docs/loop-design-checklist.md`, `docs/failure-modes.md`,
  `docs/operating-loops.md`, `docs/multi-loop.md`, `patterns/daily-triage.md`,
  `tools/loop-audit/README.md` (read via `external_code_read`, snapshot 3038).
- Code seams verified in-worktree: `Reconciler::tick_once`
  (`plan/reconciler/mod.rs:1120`), `plan_allows_dispatch` + `PlanDispatchState`
  (`plan/reconciler/guards.rs:8`, `plan/reconciler/mod.rs:818`),
  `reconcile_terminal_epics` (`plan/reconciler/terminal.rs:109`),
  `AuditSentinelKind` (`plan/audit_sentinel.rs:71`), label idioms (`plan/labels.rs`),
  `push_plan_completed_continuation` (`plan/mod.rs:3141`),
  `submit_plan_as_epic_internal` (`server/handlers/plan.rs:661`),
  `ReconcilerConfig` backoff idiom (`plan/reconciler/mod.rs:776`),
  cost conversion (`outcome_materializer.rs:558`).
