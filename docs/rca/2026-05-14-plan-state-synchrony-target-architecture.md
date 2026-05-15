# Plan-State Synchrony — Target Architecture & Strangler-Fig Migration

**Date:** 2026-05-14
**Author:** brain (audit synthesis from codex / gemini / kimi)
**Related:** bd-334 (worker_branch dropped after request_changes + reuse_prior_worktree), bd-d1r (orchestrator simulator)
**Status:** Design — pending authorization to execute Tier 0

---

## 1. Why this document exists

bd-334 surfaced a class of state-synchrony bugs that unit tests don't catch and that a downstream simulator would only paper over. Three independent audits (codex evidence-first, gemini pattern analysis, kimi first-principles) converged on the same diagnosis: the recent fix patched a symptom, the structural cause is intact, and the same failure mode will recur in other shapes until the architecture is consolidated.

This document captures the target architecture at four zoom levels and the lower-risk strangler-fig path from today's working system to that target. No code changes are proposed inline; this is the design that gates the implementation plan.

---

## 2. The three-layer failure (what bd-334 actually is)

Each auditor located the root cause at a different layer. They are not competing — they are three facets of the same layered failure that *all aligned* for bd-334 to manifest:

| Layer | Auditor | Mechanism |
|---|---|---|
| **L1 — Cache invalidation** | kimi | `derive_beads_version` counts only epic-level audits, not task-level Completion events. The `active_plans` cache diverges from the durable audit log; nobody triggers a re-fold even when one is needed. |
| **L2 — Destructive projection** | gemini | `latest_completion_facts` does `latest = Some(...)` per Completion event instead of a per-field state-machine fold. A sparse-payload Completion silently overwrites historical fields with `None`. |
| **L3 — Dual-write in command handlers** | codex / gemini | `review_task` "approve" branch emits both `PendingAuditEmit::Approval` *and* `PendingBeadsOp` imperative state mutation. Non-atomic. The cache and the log can diverge by design. |

The bd-334 fix at commit `33f5f479` forced one specific field (`worker_branch`) to always be present in attempt-2 completions. That closed *one* path through *one* layer. The other layers and the other fields are unchanged. Any new sparse audit type, any new approval-path mutation, any new task-level audit that the cache doesn't see — same bug, different shape.

---

## 3. Target architecture — four zoom levels

### L0 — System view (where the orchestrator sits)

```mermaid
flowchart TB
    Brain[Brain Agent<br/>LLM session]
    TUI[TUI / IDE]
    Worker[Worker Agents<br/>codex / gemini / claude / kimi]
    Git[Git Worktrees<br/>+ Branches]

    Orch[Orchestrator<br/>pure decide + pure project]
    Log[(Audit Log<br/>append-only<br/>Beads comments)]
    Exec[Effect Executor<br/>idempotent IO]

    Brain -->|MCP tool calls| Orch
    TUI -->|read-only queries| Orch
    Orch -->|append events| Log
    Log -->|fold into state| Orch
    Orch -->|emit effects| Exec
    Exec -->|dispatch| Worker
    Worker -->|completion audit| Log
    Exec -->|git ops| Git
    Exec -->|labels / status<br/>UI projection| Beads[(Beads UI<br/>read-model)]
```

**Key shifts from today:** the audit log is the *only* source of truth for plan state. Beads issues/labels become a *derived UI read-model*, not an input to projection. The Effect Executor is the *only* layer that performs imperative side-effects, always *after* a durable event append.

### L1 — State-machine boundary (the pure core)

```mermaid
flowchart LR
    subgraph Pure[Pure Synchronous Core]
        direction TB
        Project["fn project(&AuditLog) -> PlanState<br/><i>deterministic fold</i>"]
        Decide["fn decide(&PlanState, &Command) -> Vec&lt;Effect&gt;<br/><i>pure command handler</i>"]
        Project --> Decide
    end

    subgraph Impure[Async IO Boundary]
        direction TB
        Append["append_event(Event)<br/>→ AuditLog"]
        ExecEffect["execute(Effect)<br/>→ Git / Beads / Worker"]
    end

    AuditLog[(Audit Log)] --> Project
    Command[Command<br/>e.g. submit_plan,<br/>review_task,<br/>worker_completed] --> Decide
    Decide -->|new event| Append
    Decide -->|side-effect| ExecEffect
    Append --> AuditLog
    ExecEffect -->|observation<br/>e.g. completion| Command
```

**The invariant this enforces:** state changes happen *only* through events. Effects happen *only* after events are durable. Replay is trivial because the fold is pure.

### L2 — Pure-fold internals (how a single task's state is computed)

```mermaid
stateDiagram-v2
    [*] --> Pending: TaskSpec event
    Pending --> Dispatched: Dispatch event<br/>(attempt N)
    Dispatched --> AwaitingReview: Completion event<br/>(carries worker_branch,<br/>dispatched_base_oid)
    AwaitingReview --> Approved: Approval event
    AwaitingReview --> Dispatched: ReviewFeedback event<br/>(request_changes,<br/>attempt N+1)
    AwaitingReview --> Rejected: Rejection event
    Approved --> [*]
    Rejected --> [*]

    note right of Dispatched
        State variant owns
        Attempt struct with
        non-Optional fields.
        worker_branch: String
        (never Option after Completion)
    end note

    note right of Approved
        Approval cannot mutate
        Attempt. Type system
        prevents bd-334.
    end note
```

**The key type-system trick** (from kimi's first-principles derivation): once `Completion` populates an `Attempt` struct, the `Attempt` is owned by the state variant. `Approval` changes only the outer variant tag — it cannot drop or alter the `Attempt`. There is no code path through which `worker_branch` can become `None` after Approval, because there is no field to set to None and no logic that touches the Attempt. **bd-334 becomes impossible by construction, not impossible by convention.**

### L3 — Event taxonomy (the irreducible vocabulary)

```mermaid
classDiagram
    class Event {
        <<enumeration>>
        +TaskSpec
        +Dispatch
        +WorkerStarted
        +Completion
        +ReviewFeedback
        +Approval
        +Rejection
        +Signal
        +PlanMutation
    }

    class Dispatch {
        +TaskId task_id
        +Attempt attempt
        +DelegationId delegation_id
        +BaseSpec base_spec
    }

    class Completion {
        +TaskId task_id
        +Attempt attempt
        +String worker_branch
        +String dispatched_base_oid
        +Option~Diff~ diff
        +bool mark_noop
    }

    class ReviewFeedback {
        +TaskId task_id
        +Attempt attempt
        +Decision decision
        +bool reuse_prior_worktree
        +String prior_branch
    }

    class Approval {
        +TaskId task_id
        +Attempt attempt
        +Option~String~ summary
    }

    Event <|-- Dispatch
    Event <|-- Completion
    Event <|-- ReviewFeedback
    Event <|-- Approval
```

**Schema discipline:** every event carries enough information to be folded without consulting external state. `Completion` carries `worker_branch` non-optionally (a Completion *without* a worker_branch is a different event, e.g. `CompletionFailed`). `Approval` carries only the *attempt index it approves*, not the branch — the branch is already in the state machine. **No event has a field that can be silently dropped, because nullable fields go in separate event variants.**

---

## 4. Today vs. target — concrete deltas

```mermaid
flowchart TB
    subgraph Today
        direction TB
        T_Cache[active_plans cache<br/>mutable<br/>weak invalidation]
        T_Proj[projector.rs<br/>reads audit log AND labels<br/>destructive overwrite fold]
        T_Cmd[command handlers<br/>emit event AND<br/>imperative beads op<br/>non-atomic dual-write]
        T_Beads[(Beads<br/>both event store<br/>and mutable state)]

        T_Cmd -.->|event| T_Beads
        T_Cmd -.->|imperative| T_Beads
        T_Beads --> T_Proj
        T_Proj --> T_Cache
    end

    subgraph Target
        direction TB
        X_Proj[pure project()<br/>audit log only<br/>per-field fold]
        X_Decide[pure decide()<br/>event-only output]
        X_Exec[Effect Executor<br/>idempotent IO<br/>after append]
        X_Log[(Audit Log<br/>sole truth)]
        X_Beads[(Beads UI<br/>derived read-model)]

        X_Decide -->|events only| X_Log
        X_Log --> X_Proj
        X_Proj --> X_Decide
        X_Decide -->|effects| X_Exec
        X_Exec --> X_Beads
        X_Exec --> X_Log
    end
```

**The four concrete deltas:**

1. **Severing.** `projector.rs` stops reading `issue.labels` and `issue.status`. Status derives from the audit fold alone.
2. **Folding correctly.** `latest_completion_facts` becomes a per-field state-machine fold. A property established at attempt N propagates unless explicitly nullified by a later event.
3. **Purifying handlers.** Command handlers (`review_task`, `request_changes`, dispatch, escalation) drop `PendingBeadsOp` imperative state mutations. They emit events only. The Effect Executor handles imperative UI updates *after* the event is durable.
4. **Cache as read-model.** `active_plans` becomes a maintained read-model that is *updated by event appends*, not by direct mutation. Invalidation token covers all task-level audit writes.

---

## 5. The honest costs of the target architecture

Naming these explicitly because gemini and kimi both flagged them:

| Cost | Why it's real | Mitigation |
|---|---|---|
| **Replay performance** | 50+ JSON comments per task to determine readiness on cold start. | Maintained read-model + per-plan version token covers warm path. Cold replay is acceptable. |
| **Schema evolution friction** | `AuditSentinelKind` JSON variants live forever in old audit logs. Adding a field requires backwards-compat or an explicit upcaster. | Establish schema versioning discipline up front; upcaster pattern is well-understood. |
| **Debugging changes shape** | "Read one DB row" becomes "fold the event stream." | Mermaid sequence-diagram dump on assertion failure (already in simulator design). Event log is genuinely *more* debuggable for state-synchrony bugs. |
| **Effect-after-append discipline** | Every IO operation must be idempotent and replayable. | Already partially true (audit emission is idempotent). Discipline applied per Effect type. |

---

## 6. Strangler-fig migration — four tiers

```mermaid
gantt
    title Strangler-Fig Migration (calendar weeks)
    dateFormat YYYY-MM-DD
    axisFormat %m-%d

    section Tier 0 (Lock the door)
    Cache token fix              :t0a, 2026-05-15, 1d
    Defensive invariants         :t0b, after t0a, 1d
    Shadow projector             :t0c, after t0b, 3d

    section Tier 1 (Fix the fold)
    Observe shadow data          :t1a, after t0c, 5d
    Per-field fold rewrite       :t1b, after t1a, 4d
    Promote, retire old fold     :t1c, after t1b, 1d

    section Tier 2 (Sever labels)
    Flag-gated pure projector    :t2a, after t1c, 5d
    Staging soak                 :t2b, after t2a, 5d
    Production flip, retire flag :t2c, after t2b, 2d

    section Tier 3 (Purify handlers, per path)
    approve path                 :t3a, after t2c, 5d
    request_changes path         :t3b, after t3a, 4d
    dispatch path                :t3c, after t3b, 4d
    escalation path              :t3d, after t3c, 3d

    section Tier 4 (Verification tooling)
    spur-sim on pure substrate   :t4a, after t3d, 5d
```

### Tier 0 — Lock the door (3–5 days, zero behavior change)

```mermaid
flowchart LR
    Code[existing<br/>projector + handlers]
    Cache[active_plans cache]
    Shadow[shadow projector<br/>pure fold<br/>observe-only]
    Assert[invariant asserts<br/>at mutation sites]
    Token[cache token fix<br/>task-level audits<br/>invalidate]

    Code -.unchanged.-> Cache
    Code -.parallel.-> Shadow
    Shadow -- log mismatch --> Warn[structured warning]
    Code -.guarded by.-> Assert
    Token -.fixes.-> Cache
```

- **Cache invalidation token** advances on task-level audits (kimi's quick win).
- **Defensive invariants** at every state-mutation site for `worker_branch`, `dispatched_base_oid`, attempt info. Silent drop → loud panic with context.
- **Shadow projector** runs alongside the existing one. Pure-fold-only. Mismatches log warnings, production uses the existing projector's result. Buys empirical data about how broken the existing projector actually is.

**Risk: near-zero.** Asserts reversible. Shadow observe-only. Token fix is a 1-line change.

### Tier 1 — Fix the fold (3–7 days, isolated to one function)

Rewrite `latest_completion_facts` from `latest = Some(...)` overwrite to a per-field state-machine fold. Surgical change, one function, one file. Continuously validated against shadow projector from Tier 0.

**Risk: low.** Bounded surface. Reversible.

### Tier 2 — Sever label reads from projector (1–2 weeks, feature-flagged)

```mermaid
flowchart TB
    Flag{pure_projector_enabled?}
    Old[old projector<br/>reads labels + audit]
    New[new projector<br/>audit only]

    Flag -->|off, default| Old
    Flag -->|on| New
    Old --> Result
    New --> Result[PlanState]
```

Runtime flag, default off. CI enables for parity tests. Staging soak. Production flip. Delete flag + old code path.

**Risk: medium, bounded.** Flag-reversible. No data loss possible (audit log was always canonical).

### Tier 3 — Purify command handlers, one path at a time

```mermaid
sequenceDiagram
    participant Brain
    participant Handler as command handler<br/>(pure)
    participant Log as Audit Log
    participant Exec as Effect Executor
    participant Beads
    participant Git

    Brain->>Handler: review_task(approve)
    Handler->>Handler: decide() → Vec<Effect>
    Handler->>Log: append Approval event
    Log-->>Handler: ack
    Handler-->>Brain: response
    Note over Exec: async, observes log
    Log->>Exec: Approval event
    Exec->>Beads: update labels<br/>(idempotent)
    Exec->>Git: detach worktree<br/>(idempotent)
```

`approve` path first (bd-334 epicenter). Then `request_changes`. Then dispatch. Then escalation. Each is its own PR with its own e2e test.

**Risk: low per path.** No big-bang. Reviewable in normal cadence.

### Tier 4 — Verification tooling on the pure substrate

Now `fn project(&[Event]) -> State` is trivially testable. proptest over event sequences. The original bead-d1r 30-scenario list is *mostly subsumed* by property invariants; a small integration suite covers real-IO edges (worktree errors, real branch collisions).

---

## 7. Decision matrix

| Path | Calendar | Risk concentration | Mid-flight reversible? | Half-state ever exists? | Best fit if… |
|---|---|---|---|---|---|
| **Tier 0 only** | ~1 week | Near-zero | Trivially | No | Goal is purely bd-334-class regression protection. |
| **Tier 0 → 1** | ~2 weeks | Low | Yes | No | Goal includes hardening the projector against the broader anti-pattern. |
| **Tier 0 → 3** | ~6–8 weeks distributed | Low per step | Yes per tier | No | Goal is full architectural consolidation, paced over normal sprints. |
| **Tier 0 → 4** | ~10 weeks distributed | Low per step | Yes per tier | No | Includes building the simulator on the cleaned-up substrate. Full bead-d1r scope. |
| **(Rejected) Big-bang 5-week sprint** | 5 weeks concentrated | Medium-high concentrated | No | Yes, for weeks | Deadline pressure forcing concentration. Not recommended. |

---

## 8. What this means for bd-d1r

The original bead-d1r framed a 30-scenario simulator as the primary deliverable. After the audit:

- The simulator is demoted to **Tier 4 verification tooling**.
- Most of the 30 scenarios are subsumed by property tests over the pure `step()` function.
- A small integration suite remains for genuine I/O edges.
- The simulator's value depends on Tiers 1–3 happening first; otherwise it's testing a fragile substrate.

Recommended re-scoping: keep bd-d1r as the umbrella, open child beads per tier (`bd-d1r-t0`, `bd-d1r-t1`, `bd-d1r-t2`, `bd-d1r-t3`, `bd-d1r-t4`). Each tier independently dispatchable.

---

## 9. Open questions for the next decision cycle

1. **Tier 0 authorization** — ship this week, low risk, high protection?
2. **Tier-by-tier authorization** vs. blanket Tier 0–3 authorization?
3. **Shadow-projector mismatch threshold** — what observed mismatch rate during Tier 0 triggers aggressive Tier 1/2 vs. relaxed cleanup?
4. **Schema evolution policy** — when Tier 3 lands, do we version `AuditSentinelKind` JSON explicitly, or rely on additive-only evolution?
5. **Effect Executor placement** — new module in `spur-mcp` or new crate `spur-effects`?

---

## 10. Tier-1 update (2026-05-15) — scope refinement post-Tier-0 landing

**Status:** Tier-0 delivered. This section refines Tier-1 in light of what Tier-0 surfaced and ties the original `Sever labels` (then "Tier 2") into a unified work-stream coordinated with the **bd-1va dual-write** fix.

### 10.1 What Tier-0 delivered

Three commits merged to local main on 2026-05-15:

| Commit | Bead | What it did | Substrate impact |
|---|---|---|---|
| `6172b027` | bd-d1r-t0c-fu1 (bd-3lo) | Surgical field-skip in `emit_shadow_projector_mismatch_warnings` so legacy `Superseded { mutation_id, by }` no longer triggers shadow-vs-legacy mismatch noise on the one field shadow *cannot* reconstruct from audits alone (the label-only `mutation_id`/`by` metadata). All other field comparisons preserved. | Restores shadow projector's signal quality. Validates that the architectural axiom "labels carry data audits cannot" is real and must be respected, not hand-waved. |
| `cef0fd47` | bd-d1r-fu-stale-delegation-cleanup (bd-1xt) | Scrubs every existing `spur:delegation-id:*` and legacy `delegation-id:*` label on both `dispatch_intent_update` and `clear_dispatch_intent_update` paths. Closes the "stale label survives across attempts → projector picks stale ID via first-label-wins `.find_map(...)` → invariant panic on closure" hazard class. | First substantive step toward severing labels from authority: the *label state* now has a strict-uniqueness invariant maintained at every write boundary. Audit-sentinel JSON remains the durable lineage record. |
| `82aa852b` | bd-d1r fixture cluster | Brings test harness to fidelity with the T0 invariant `assert_dispatched_base_oid_for_active_status` (projector.rs:885): adds `mock_worker_completion` helper, unignores 4 tests that bypassed worker completion via label-poking, deletes one test that verified state structurally unreachable under T0. | Locks in the projector load-time invariant by ensuring no test harness can construct an invariant-violating state. |

### 10.2 The architectural axiom Tier-0 surfaced — and its bifurcation

A first draft of this section claimed "audits are the source of truth, labels are projections." A subsequent first-principles audit (gemini delegation `d1a6aa6e-670c-49c1-be0a-4381e385429b`) refuted that framing as too clean: it ignores two substrate constraints that force part of the system to keep labels *authoritatively*. The honest axiom is bifurcated:

> **For state-determining concerns, the audit-sentinel comment stream is the source of truth. Labels representing this state are indexed read-models, maintained as caches so the Beads backend can answer `list_issues({labels: [...]})` queries (it cannot server-side filter on comment-body JSON). For high-frequency ephemera (lease heartbeats) and out-of-band triggers (signals), the audit stream is intentionally bypassed — labels are the authoritative storage primitive with no audit counterpart by design.**

This bifurcation is not a wart; it is a deliberate response to two real constraints:

1. **Query indexability**: `crates/spur-mcp/src/plan/reconciler/guards.rs:22,136`, `terminal.rs:127,181`, `server/recovery.rs:121`, `server/handlers/plan.rs:499` all call `pm.list_issues(IssueFilter { labels: [...] })` to find work. If the index label is missing, the issue is invisible — no reconciler tick will discover it. Labels are how the backend index works.
2. **Heartbeat economics**: `crates/spur-mcp/src/plan/mod.rs:2143-2167` (`update_dispatch_lease`) refreshes `spur:lease-expires-at:*` every few minutes with a label-only write. An audit-comment per heartbeat would saturate the timeline within hours. Likewise `crates/spur-mcp/src/plan/signal_watcher.rs:98-118` and `crates/spur-mcp/src/server/handlers.rs:789` treat `signal:*` as authoritative — there is no `AuditSentinelKind::Signal` variant.

### 10.2.1 Label bifurcation — Type A (Indexed Read-Model) vs. Type B (Authoritative Ephemera)

| Type | Examples | Source of truth | Maintenance | Reconciler responsibility |
|---|---|---|---|---|
| **A — Indexed Read-Model** | `spur:plan-id:*`, `spur:plan-task-id:*`, `spur:agent:*`, `spur:plan-pending`, `ready-for-review`, `spur:delegation-id:*` (current attempt only), `spur:superseded-by:*`, `spur:mutation-id:*` | The **audit stream**. Each label is a denormalized projection of state derivable by folding `AuditSentinelKind::{TaskSpec, Dispatch, Completion, Approval, Rejection, …}`. | **Eager write-through** inline with the audit append (write-ahead log). Idempotent. | **Index hygiene sweeper**: detect drift between label state and audit-derived state; refresh idempotently. This is §10.3.1's actual role. |
| **B — Authoritative Ephemera** | `spur:lease-expires-at:<ts>`, `signal:scope-drift`, `signal:escalated`, `signal:blocked`, `signal:risk` | The **label itself**. No audit-stream copy exists by design (would be timeline spam, or the data is out-of-band-injected by external tooling). | Direct label writes via `update_issue`. No write-ahead log; nothing to reconcile against. | Lease expiry sweep (existing). Signal label handling stays as-is. **No change under Tier-1.** |

Cited reads outside the projector:
- Type A index readers: `reconciler/guards.rs:22,136`, `reconciler/terminal.rs:127,181`, `server/handlers/plan.rs:499`, `server/recovery.rs:121`, `mutation_executor.rs:1516-1534`.
- Type A status readers (today, target Tier-1 §10.3.2): `projector.rs:362-468`, `signal_watcher.rs:98-118` (for `ready-for-review` filter), `server/handlers/delegation.rs:217,236` (`recover_orphaned_dispatch`), `plan_builder.rs:230`, `reconciler/leases.rs:49`, `server/sync.rs:520`.
- Type B authoritative readers: `reconciler/leases.rs:60` (parses `spur:lease-expires-at:*`), `signal_watcher.rs:98-118` (filters by `signal:*`).
- Type B writers: `mod.rs:2143-2167` (`update_dispatch_lease`), `server/handlers.rs:789` (signal attach).

This bifurcation directly governs the three Tier-1 components: §10.3.1 maintains Type A index integrity; §10.3.2 demotes Type A *status* labels from projection-authoritative to index-only; §10.3.3 handles a Type A label-only metadata field (`Superseded.{mutation_id, by}`) in the shadow comparator. **Type B labels are untouched.**

### 10.3 Tier-1 scope — three components

#### 10.3.1 bd-1va — index-maintenance reconciler (Type A label cache hygiene)

**Reframing note.** An earlier draft of this subsection called this work "self-healing reconciler / audit-WAL" and described its job as *race repair*: detect a torn dual-write state and finish the second write. That framing was wrong under the bifurcation in §10.2.1. Once §10.3.2 demotes Type A status labels from projection-authoritative to index-only, **no consumer derives state from labels anymore**, so there is no race window between "label says X, audit says Y" — the projector always reads Y. The reconciler's job is not race repair; it is **index integrity maintenance** for the read-model. This subsection's verbs and invariants are reframed accordingly.

**Plain English (before, today's regime).** Today, when a worker completes a task, `persist_completion_result_with_retry_for_task` (`crates/spur-mcp/src/plan/mod.rs:~2451`) writes the Completion audit comment, then the Type A label update — two separate I/O calls. A crash between them leaves *two* things visibly inconsistent: (a) the projector reads `Dispatched` from the stale label even though the audit says `AwaitingReview` (today's projector is label-first); (b) `list_issues({labels: [ready-for-review]})` does not return the issue, so reviewers can't find it. Today, (a) is the visible-race symptom that bd-334 / bd-1va surfaced.

```mermaid
sequenceDiagram
    participant W as Worker
    participant Compl as persist_completion_result
    participant PM as Beads PM
    participant Proj as Projector (label-first)
    participant Q as list_issues consumers

    W->>Compl: completion outcome
    Compl->>PM: add_comment(Completion sentinel)
    Note over PM: comment durable<br/>worker_branch + summary persisted
    Note right of Proj: ⚠ projector reads labels first<br/>(today's regime)
    Proj->>PM: read labels
    PM-->>Proj: spur:delegation-id:foo still present
    Proj->>Proj: project_status_for_issue → Dispatched (wrong)
    Q->>PM: list_issues({labels:[ready-for-review]})
    PM-->>Q: ∅ (issue invisible to query)
    Compl->>PM: apply_issue_update(labels) [eventually]
    Note over PM: labels consistent;<br/>projection only correct now
```

**Plain English (after, under §10.3.2 + this section).** Under the bifurcation:
- §10.3.2 makes the projector audit-first → state is correctly `AwaitingReview` the instant the Completion comment lands, regardless of label state. No race in projection.
- The label-update is now *pure cache maintenance*. A failed/delayed label write does not corrupt state; it only delays *queryability*. The issue is correctly `AwaitingReview`, but `list_issues({labels:[ready-for-review]})` won't find it until the label catches up.
- **This subsection's job**: ensure the catch-up happens reliably and idempotently. The reconciler periodically reads each open issue, derives the expected Type A label set from `(issue.audits)` via audit-derivation helpers, and patches any drift via an idempotent `update_issue` call. Drift is structured-logged as a `label_index_drift` counter (telemetry, not panic).

```mermaid
sequenceDiagram
    participant W as Worker
    participant Compl as persist_completion_result
    participant PM as Beads PM
    participant Proj as Projector (audit-first §10.3.2)
    participant Recon as Index-Maintenance Reconciler
    participant Q as list_issues consumers

    W->>Compl: completion outcome
    Compl->>PM: add_comment(Completion sentinel)
    Note over PM: WAL: audit durable<br/>STATE IS NOW CORRECT
    Proj->>PM: read audits
    PM-->>Proj: Completion sentinel present
    Proj->>Proj: project → AwaitingReview ✓<br/>(audit-derived; labels irrelevant)
    Compl->>PM: apply_issue_update(labels) [eager refresh]
    alt happy path
        Note over PM: index consistent immediately
    else crash or partition
        Note over PM: index temporarily stale<br/>(state still correct via projector)
        Q->>PM: list_issues({labels:[ready-for-review]})
        PM-->>Q: ∅ (issue not yet findable)
        Note over Recon: next tick
        Recon->>PM: read (issue, audits)
        Recon->>Recon: expected_labels = derive_from_audits(audits)<br/>diff vs current → emit label_index_drift counter
        Recon->>PM: update_issue(remove_stale, add_missing) [idempotent]
        Note over PM: index converged;<br/>issue now findable via labels
    end
```

**Delta from today.**
- The two-step write is *not eliminated* — Beads provides no multi-statement transaction across comments and labels (per §10.3 of bd-1va analysis). It's *demoted*: the label write is no longer a state-correctness operation, just an index-cache refresh.
- The reconciler's verb changes from "complete the missing commit" to "reconcile the index against the audit-derived projection." Same mechanism (idempotent `update_issue`), different invariants in tests.
- Telemetry: `label_index_drift{type=label-name, direction=missing|stale|mismatched}` counter replaces the panic. Operators can alert on sustained nonzero drift (real bug) vs. transient nonzero drift (expected during writes).
- The Tier-0 relaxed invariants at `projector.rs:443-466` can be tightened back **only after §10.3.2 lands** (when state stops being derived from labels). This subsection on its own does *not* enable the tighten; §10.3.2 does.

**Out of scope.** Type B labels (`spur:lease-expires-at:*`, `signal:*`) are not maintained by this reconciler. Their authority remains label-direct; their write paths (`update_dispatch_lease`, signal-attach handlers) are unchanged.

**Production-code surface.** `persist_completion_result_with_retry_for_task` (`mod.rs:~2451-2540`) keeps comment-first ordering. The reconciler gains an `index_hygiene_sweep(&issue, &audits)` step per tick (new helper in `reconciler/mod.rs` or a new module `reconciler/index_hygiene.rs`). Audit-derivation helpers shared with §10.3.2 (`current_delegation_from_audits`, `awaiting_review_from_audits`, `terminal_status_from_audits`) produce the expected Type A label set; the diff against current labels is the patch. Test surface: property tests for "crash anywhere between audit-write and label-write → next tick converges the label index to match the audit-derived projection; intermediate state is queryably-stale but not incorrect." Estimated ~200-400 LoC production + ~150-250 LoC tests — smaller than the original "race repair" framing because it reuses §10.3.2's helpers and has weaker invariants (cache, not commit).

#### 10.3.2 projector-prefers-audits

**Plain English (before).** Today `project_status_for_issue` (`projector.rs:362-467`) uses a *label-first cascade*: it asks "is there a `spur:delegation-id:*` label?" *before* it consults the audit stream. Labels are the truth; audits are decoration. This is why every label-drift bug we've seen (bd-334, the dual-label race, bd-1xt's stale-label hazard, bd-1va's torn-state) manifests as a projector panic or a misclassification: when labels are wrong, projection is wrong.

```mermaid
flowchart TD
    Start[project_status_for_issue]
    Closed{issue closed?}
    Conflict{has integration-<br/>conflict label?}
    Deleg{has delegation-id<br/>label?}
    Esc{has signal:<br/>escalated label?}
    Ready{has ready-for-<br/>review label?}
    ReadyNow{ready_now?}

    Start --> Closed
    Closed -->|yes| ClosedFlow[project_closed_status<br/>scans audits]
    Closed -->|no| Conflict
    Conflict -->|yes| Blocked[BlockedOnSetupConflict<br/>from audit]
    Conflict -->|no| Deleg
    Deleg -->|yes| Disp[Dispatched<br/><b>label is truth</b>]
    Deleg -->|no| Esc
    Esc -->|yes| Escal[EscalatedToBrain<br/><b>label is truth</b>]
    Esc -->|no| Ready
    Ready -->|yes| Await[AwaitingReview<br/><b>label is truth</b><br/>summary from audit]
    Ready -->|no| ReadyNow
    ReadyNow -->|yes| Rdy[Ready]
    ReadyNow -->|no| Pend[Pending]

    Disp:::bad
    Escal:::bad
    Await:::bad
    classDef bad fill:#fee,stroke:#c33
```

**Plain English (after).** The cascade is inverted. Audit-derivation helpers (`current_delegation_from_audits`, `awaiting_review_from_audits`, `escalated_from_audits`) become the *primary* input. Labels are consulted only when audits are silent or to retrieve metadata the audit stream genuinely cannot carry (e.g. `Superseded.mutation_id`/`by`). A label that disagrees with audits emits a structured `label_audit_drift` diagnostic counter — *not* a panic — and the audit-derived state wins.

```mermaid
flowchart TD
    Start[project_status_for_issue]
    Audits[fold audits<br/>once]
    Cur{audit says<br/>current delegation?}
    AReview{audit says<br/>awaiting review?}
    AEsc{audit says<br/>escalated?<br/><i>future: audit-only</i>}
    Closed{issue closed?}
    Conflict{integration-conflict<br/>label set?}
    ReadyNow{ready_now?}

    Start --> Audits
    Audits --> Closed
    Closed -->|yes| ClosedFlow[project_closed_status<br/>audits authoritative]
    Closed -->|no| Cur
    Cur -->|yes| Disp[Dispatched<br/><b>from audit</b><br/>label drift → diag counter]
    Cur -->|no| AReview
    AReview -->|yes| Await[AwaitingReview<br/><b>from audit</b><br/>summary + worker_branch from audit]
    AReview -->|no| AEsc
    AEsc -->|yes| Escal[EscalatedToBrain<br/><b>from audit</b>]
    AEsc -->|no| Conflict
    Conflict -->|label only| Blocked[BlockedOnSetupConflict<br/>label hint + audit detail]
    Conflict -->|no| ReadyNow
    ReadyNow -->|yes| Rdy[Ready]
    ReadyNow -->|no| Pend[Pending]

    Disp:::good
    Await:::good
    Escal:::good
    classDef good fill:#efe,stroke:#393
```

**Delta.** Three concrete shifts: (1) the strict mutual-exclusivity invariants at `projector.rs:443-466` can be *tightened back* (the relaxation was a band-aid for label-drift; once label-drift no longer determines state, there's no race to tolerate). (2) Every downstream consumer that reads labels today — `recover_orphaned_dispatch` (`server/handlers/delegation.rs:217,236`), `signal_watcher` (`plan/signal_watcher.rs:98,119`), `plan_builder` recovery (`server/plan_builder.rs:230`), reconciler `leases.rs:49`, `sync.rs:520` — gets the same inversion: audit-first, label-as-hint. (3) `label_audit_drift` becomes the canonical observability signal for hygiene issues; the panics retire.

Estimated ~500-800 LoC across 5 call sites; best landed as 3-4 sequential sub-PRs (projector core first, then each consumer).

#### 10.3.3 Partial-order `PlanTaskStatus` comparator

**Plain English (before).** Today `emit_shadow_projector_mismatch_warnings` (`projector.rs:~610-668`) compares legacy vs. shadow projections field-by-field via `format!("{:?}", ...)` string equality. Tier-0 added a surgical skip: when legacy is `Superseded`, skip the `status` field check (because shadow can't reconstruct `mutation_id`/`by`). This is correct as far as it goes, but it discards a real signal — the shadow *does* know the fact of supersession from `CompletionState::Superseded` in the audit stream; it just can't reconstruct the label-only metadata.

```mermaid
flowchart LR
    Legacy["Legacy:<br/>Superseded { mutation_id: m-1, by: [t1a] }"]
    Shadow["Shadow:<br/>Pending<br/>(supersession fact: discarded)"]
    Cmp{format-debug<br/>equal?}
    Skip[Tier-0 skip:<br/>if legacy is Superseded<br/>→ skip status check]
    Out[no warning]

    Legacy --> Cmp
    Shadow --> Cmp
    Cmp -->|no| Skip
    Skip --> Out

    Out:::warn
    classDef warn fill:#ffd,stroke:#990
```

**Plain English (after).** Replace the format-debug equality with a *partial-order comparator*. The shadow projector emits `Superseded { mutation_id: None, by: vec![] }` when it sees `CompletionState::Superseded` in the audit stream (preserving the *fact*). The comparator treats legacy's `Some("m-1")` / `vec!["t1a"]` as *matching* shadow's `None` / `vec![]` under the partial-order rule "shadow ≤ legacy on this field is OK." The Tier-0 status-field skip becomes redundant and is removed. The fact-of-supersession round-trips through the audit-only fold; only the label-only metadata is acknowledged as irreducibly out-of-reach for the shadow.

```mermaid
flowchart LR
    Legacy["Legacy:<br/>Superseded { mutation_id: Some(m-1), by: [t1a] }"]
    Shadow["Shadow:<br/>Superseded { mutation_id: None, by: [] }<br/>(fact preserved)"]
    Cmp{partial-order<br/>compare?}
    Out1[Exact: shadow exactly matches legacy]
    Out2[PartialMatch:<br/>shadow ≤ legacy on irreducible fields<br/>mutation_id, by]
    Out3[Mismatch:<br/>genuine divergence on derivable fields]

    Legacy --> Cmp
    Shadow --> Cmp
    Cmp -->|equal| Out1
    Cmp -->|shadow None, legacy Some on label-only fields| Out2
    Cmp -->|shadow ≠ legacy on audit-derivable fields| Out3

    Out2:::ok
    Out3:::warn
    classDef ok fill:#efe,stroke:#393
    classDef warn fill:#fee,stroke:#c33
```

**Delta.** The comparator becomes future-proof: any future status variant that gains label-only metadata declares it in one place (the partial-order rule for that variant), instead of accumulating ad-hoc `if field == "..." && matches!(status, ...)` skips in the comparator loop. The supersession-fact signal — discarded today — re-enters the integrity-proof surface. Property tests over generated audit streams guarantee: every legacy/shadow pair derived from the same audits returns `Exact` or `PartialMatch`, never `Mismatch`.

Estimated ~150-250 LoC + property tests. Lowest-risk component; can land last.

### 10.4 Sequencing and dependencies

```mermaid
flowchart LR
    T0[Tier-0 delivered<br/>2026-05-15]
    Helpers[10.3.2 audit-derivation helpers<br/>current_delegation_from_audits<br/>awaiting_review_from_audits<br/>etc.]
    PlanC[10.3.1 bd-1va Plan C<br/>self-healing reconciler]
    Sweep[10.3.2 projector-prefers-audits<br/>sweep through 5 consumers]
    Cmp[10.3.3 partial-order comparator]
    Tighten[Cleanup PR:<br/>tighten projector.rs:443-466<br/>remove relaxation<br/>remove Tier-0 status-skip]

    T0 --> Helpers
    Helpers --> PlanC
    Helpers --> Sweep
    Helpers --> Cmp
    PlanC --> Tighten
    Sweep --> Tighten
    Cmp --> Tighten

    T0:::done
    Helpers:::next
    classDef done fill:#efe,stroke:#393
    classDef next fill:#ffd,stroke:#990
```

The audit-derivation helpers (built as part of §10.3.2's first sub-PR) are the foundation for all three components. §10.3.1 and §10.3.3 are independent of each other once helpers exist and can be parallelized.

### 10.5 What lands in the cleanup PR

## Status: shipped 2026-05-15

The cleanup PR is the proof the bug class is retired *for Type A status labels*. **Type B authoritative-ephemera labels are out of scope** — their authority is by-design and the cleanup PR makes no changes to lease or signal paths.

Scope of the cleanup:

- Tighten `projector.rs:443-466` back to strict mutual exclusivity. The relaxed `|| matches!(status, Dispatched)` and `|| matches!(status, ... AwaitingReview ...)` escape hatches are removed *because state is no longer derived from labels* (§10.3.2). A residual Type A label drift now manifests as a `label_index_drift` counter increment via the §10.3.1 sweeper, not a projector panic.
- Delete the bd-3lo Tier-0 status-field skip in `emit_shadow_projector_mismatch_warnings` — the partial-order comparator from §10.3.3 subsumes it.
- The shadow projector becomes the *correctness oracle* for the legacy projector. Any mismatch is now genuine (since Type A label drift no longer affects projection; only audit-fold logic does).
- A subsequent step can then *delete the legacy projector entirely* and promote the shadow path, completing the original Tier-2 goal at §6.

Explicitly **not** in cleanup scope:
- `spur:lease-expires-at:*` writes by `update_dispatch_lease` (`mod.rs:2143-2167`) — Type B, label-authoritative by design.
- `signal:*` writes and reads (`signal_watcher.rs:98-118`, `server/handlers.rs:789`) — Type B, no audit copy by design.
- Label-based query indexes used by `IssueFilter` in `reconciler/guards.rs`, `reconciler/terminal.rs`, `server/handlers/plan.rs`, `server/recovery.rs` — these remain Type A read-models; §10.3.1's sweeper keeps them honest.

### 10.6 Calendar estimate

Single-worker serial execution: **2-3 weeks** for Tier-1 components (~5 child tasks under one epic via `submit_plan`). Parallelized after helpers land: **1-1.5 weeks**. Cleanup PR: ~1-2 days post-soak.

### 10.7 Pre-flight validations

Two cheap reads to do before filing the epic:

1. Re-read this document end-to-end against the proposed §10 plan to confirm consistency with §3-§7's architectural commitments.
2. Spike the §10.3.2 audit-derivation helper API in a throwaway branch against 2-3 of the 5 call sites; confirm the abstraction holds without leaking. If it doesn't, the shape becomes an `IssueProjection` struct wrapping `(Issue, audits)` as canonical input everywhere — bigger refactor, different scope, file a separate decision.

---

## Appendix A — Audit references

- **codex audit** (delegation `a0c4f127`): evidence-first code review, identified `worker_branch` in 4 stores, found bd-334 fix commit `33f5f479`, predicted 3 latent bugs.
- **gemini audit** (delegation `8a0259ff`): pattern analysis, named "Dual-Write" and "Destructive Projection" anti-patterns, 80%-event-sourcing diagnosis, three minimum-invasive structural changes.
- **kimi audit** (delegation `5c0eabc7`): first-principles derivation, type-system invariant proof, 2-day quick-win identified, 4–6 week full-pivot estimate, intermediate-alternatives ranking.

## Appendix B — Cited code locations

- `crates/spur-mcp/src/plan/projector.rs:43` — informational parse drop
- `crates/spur-mcp/src/plan/projector.rs:98` — `latest_completion_facts`
- `crates/spur-mcp/src/plan/projector.rs:337` — `project_status_for_issue` reads labels
- `crates/spur-mcp/src/plan/audit_sentinel.rs:71, 119, 125, 152, 169` — sentinel taxonomy
- `crates/spur-mcp/src/plan/mod.rs:167, 173, 222` — `PlanState` / `PlanTaskEntry`
- `crates/spur-mcp/src/plan/mod.rs:4314` — `reuse_prior_worktree` runtime check
- `crates/spur-mcp/src/plan/reconciler/mod.rs:670, 1083, 295` — reconciler impurities
- `crates/spur-mcp/src/plan/snapshot.rs:29` — owner-state seam (always None)
- `crates/spur-core/src/plan_projection/mod.rs:1` — ACP snapshot cache
- Commit `33f5f479` — bd-334 fix

---

## 11. Tier-1 §10.3.2 second-pass reframe (2026-05-16)

**Status:** Synthesis approved. Bead graph re-issued. Implementation pending dispatch of bd-3us.

### 11.1 Trigger and method

After bd-1r8 merged (commit `5773c5bf` — collapsed parallel projection algorithms into a single chronological forward-fold, see §10.3.3), a proposal was drafted to extend Tier-1 §10.3.2 with a `PlanTaskProjection` foundation refactor + per-call-site inversion sweep across `delegation.rs:229`, `sync.rs:539`, `signal_watcher.rs:129`, plus a Tier-2 collapse via richer projection struct.

Two rounds of dual review were performed:

| Round | Reviewers | Method | Outcome |
|---|---|---|---|
| 1 | gemini + codex | Evidence-first review of the proposal | Corrections: removed `plan_builder.rs:230` (already audit-first); enforced Type A/B boundary; merged bd-3sk with projector-core inversion per §10.7 pre-flight gate |
| 2 | gemini + kimi | First-principles + double-loop with **multi-sub-agent grounding** (3 sub-agents each) | Major reframe: corrected proposal is solving a non-problem; the targeted call sites are already audit-first; bd-3sk's `PlanTaskProjection` would trip its own §10.7 escape-hatch |

Round-2 sub-agent delegations: gemini `5a7be2a8-ed04-499f-ab8f-91eac08745b4`, kimi `067d87cb-69be-471f-8432-fa27616884ae`.

### 11.2 What the second-pass review surfaced (grounded findings)

| Finding | Source | Citation |
|---|---|---|
| The 3 consumer call sites are **already audit-first with label-as-hint** at HEAD — labels are read only for drift telemetry / pre-filter optimization | gemini sub-agent C, kimi direct read | `server/handlers/delegation.rs:229` derives `delegation_id` via `current_delegation_from_audits`; `server/sync.rs:539` mirrors the pattern; `plan/signal_watcher.rs:129` gates on `awaiting_review_from_audits` |
| `index_hygiene_sweep` exists but covers only **3 of 8+ Type A label families** | gemini sub-agent A, kimi sub-agent B | `crates/spur-mcp/src/plan/reconciler/mod.rs:1190-1235`; tests at `reconciler_tick.rs:2229,2301`. Missing families: `plan-id`, `plan-task-id`, `agent`, `plan-pending`, `plan-complete`, `superseded-by`, `mutation-id` |
| `PlanTaskProjection` as a richer audit-only struct **fundamentally leaks** per §10.7 — cannot supply Type B `signal:*` labels | gemini sub-agent B | `projector.rs:657` (`signal:integration-conflict`), `projector.rs:812` (`signal:escalated`); callers would still parse raw `Issue` |
| A `ProjectedIssue` newtype type-system seal is **infeasible at reasonable cost** — `PmService::list_issues` returns `IssueSummary` consumed by ~75+ call sites | kimi sub-agent C | Workspace census; structural mismatch between `IssueSummary` and `Issue` consumers |
| Of the 8 direct label reads in `project_status_for_issue`, **6 are already replaceable by audit helpers**; 2 are genuinely label-only (`superseded-by:*`, `mutation-id:*`) | kimi sub-agent A | `projector.rs:651-654, 655, 657, 787, 809-812, 966` (audit-derivable); `projector.rs:607-618` (irreducible) |
| bd-1va's body is the **retracted race-repair framing** of §10.3.1 ¶1 | doc cross-check | bd-1va body describes "audit-comment + label-update atomicity"; §10.3.1 ¶1 explicitly retracts that framing on 2026-05-15 |

### 11.3 Before/after bead graph

**Before (round-1 corrected proposal):**

```mermaid
flowchart TB
    bd1r8[bd-1r8: substrate-alignment collapse<br/><b>DONE</b>]
    bd42v[bd-42v: proptest generator<br/>strengthening]
    bd3sk_old[bd-3sk OLD: foundation<br/>PlanTaskProjection +<br/>projector-core inversion]
    cs1[§10.3.2 callsite:<br/>delegation handler]
    cs2[§10.3.2 callsite:<br/>sync orphan]
    cs3[§10.3.2 callsite:<br/>signal-watcher Type-A]
    bd1va_old[bd-1va: audit-label<br/>atomicity race repair]
    cleanup_old[§10.5 cleanup PR]

    bd1r8 --> bd42v
    bd1r8 --> bd3sk_old
    bd3sk_old --> cs1
    bd3sk_old --> cs2
    bd3sk_old --> cs3
    bd1r8 --> bd1va_old
    cs1 --> cleanup_old
    cs2 --> cleanup_old
    cs3 --> cleanup_old
    bd1va_old --> cleanup_old

    bd3sk_old:::wrong
    cs1:::wrong
    cs2:::wrong
    cs3:::wrong
    bd1va_old:::stale
    cleanup_old:::wrong

    classDef wrong fill:#fee,stroke:#c33,stroke-width:2px
    classDef stale fill:#fed,stroke:#960,stroke-width:2px
```

Red = solving a non-problem (sites already inverted, abstraction leaks). Orange = retracted framing.

**After (round-2 synthesized graph):**

```mermaid
flowchart TB
    bd1r8[bd-1r8: substrate-alignment collapse<br/><b>DONE</b>]
    bd42v[bd-42v: proptest generator<br/>strengthening<br/><i>strengthens bd-1r8 lock</i>]
    bd3us[bd-3us: §10.3.1 reframed<br/>complete index-hygiene sweep<br/>for all Type A families]
    bd3sk_new[bd-3sk SHRUNK: projector core<br/>transitional cleanup ONLY<br/><i>§10.5 merged in</i>]
    bd1va_super[bd-1va SUPERSEDED<br/>retracted framing<br/><i>doc-only retraction or close</i>]

    bd1r8 --> bd42v
    bd1r8 --> bd3us
    bd1r8 -.replaces.-> bd1va_super
    bd3us --> bd3sk_new
    bd42v --> bd3sk_new

    bd1r8:::done
    bd42v:::active
    bd3us:::next
    bd3sk_new:::active
    bd1va_super:::superseded

    classDef done fill:#efe,stroke:#393,stroke-width:2px
    classDef active fill:#ffd,stroke:#990,stroke-width:2px
    classDef next fill:#dfd,stroke:#393,stroke-width:3px
    classDef superseded fill:#eee,stroke:#999,stroke-width:1px,color:#666
```

Green = done. Yellow = active. Bold green = next dispatch. Grey = superseded.

### 11.4 Scope of work — active beads

#### bd-3us (next dispatch) — §10.3.1 reframed: index-hygiene sweep expansion

**Scope:** Extend `index_hygiene_sweep` at `reconciler/mod.rs:1190-1235` to derive expected Type A label sets for 5+ missing families from `(issue, audits)` via the bd-1r8 helpers, then patch drift via idempotent `update_issue` calls.

| In scope | Out of scope |
|---|---|
| `spur:plan-id:*` reconciliation | Type B `signal:*` (no audit counterpart by design) |
| `spur:plan-task-id:*` reconciliation | Type B `spur:lease-expires-at:*` (heartbeat economics) |
| `spur:agent:*` reconciliation | Modifying `projector.rs:443-466` invariants (bd-3sk owns the tighten-back) |
| `spur:plan-pending` / `spur:plan-complete` | Removing existing Tier-0 defensive panics (bd-3sk owns retirement) |
| Decision: reconcile or exempt `spur:superseded-by:*`, `spur:mutation-id:*` (kimi-sub-agent-A irreducibles) | New event taxonomy or audit-sentinel additions |
| `label_index_drift{label_family, direction}` counter per family | Type-system newtype refactor (`ProjectedIssue` — kimi-sub-agent-C rejected) |
| Property tests: idempotent convergence; correct drift on injected mismatch | |

Single commit. Title: `feat(spur-mcp): tier-1 §10.3.1 — complete index-hygiene sweep for all Type A label families`.

#### bd-42v (active, filed) — proptest generator strengthening

**Scope:** strengthen `arb_audit_kind` to emit gap-triggering variants — `Signal { kind: "integration-conflict" | "integration_conflict" }` and `EscalationRequested { delegation_id: None }`. Locks the bd-1r8 identity proptest against the original substrate-drift bug paths. Independent of bd-3us; can land in parallel.

#### bd-3sk (depends on bd-3us + bd-42v) — projector core transitional cleanup

**Scope after re-scope** (was: PlanTaskProjection foundation + sweep):

| Action | Citation |
|---|---|
| Remove transitional Dispatched branch | `projector.rs:799-803` (explicitly marked "Transitional workaround until t1-cleanup tightens invariants") |
| Remove redundant drift warnings in `project_status_for_issue` now covered by bd-3us telemetry | `projector.rs:873-900` (drift-tolerant mismatch warnings) |
| Tighten mutual-exclusivity invariants to strict form | `projector.rs:443-466` (Tier-0 relaxations retired) |
| Retire bd-3lo Tier-0 status-field skip — subsumed by bd-1r8 partial-cmp comparator | `emit_shadow_projector_mismatch_warnings` |
| Merge §10.5 cleanup into this bead | Surface too small to justify standalone bead after bd-3us lands |

**Hard constraints:**
- Type-B prohibition: NO modification of `signal:*` or `spur:lease-expires-at:*` writes/reads.
- No new label-derived state; all state derivation goes through `project_status_from_audits` (bd-1r8).

### 11.5 Sequencing diagram

```mermaid
gantt
    title Synthesized Tier-1 sequencing
    dateFormat YYYY-MM-DD
    axisFormat %m-%d

    section Done
    bd-1r8 substrate collapse  :done, b1, 2026-05-15, 1d

    section Parallel-able
    bd-42v proptest strength   :active, b2, 2026-05-16, 2d
    bd-3us index-hygiene sweep :active, b3, 2026-05-16, 5d

    section Sequenced
    bd-3sk projector cleanup   :b4, after b3, 3d
    soak + verification        :b5, after b4, 3d
```

bd-42v and bd-3us are fully parallelizable. bd-3sk strictly depends on bd-3us landing (cannot retire drift-tolerance without confirmed convergent index) and on bd-42v landing (strengthens the proptest gate that protects the cleanup).

### 11.6 What's NOT in the new graph (and why)

| Rejected / deleted | Reason |
|---|---|
| Per-call-site inversion beads (`delegation-handler`, `sync-orphan`, `signal-watcher-typeA`) | Sites are **already audit-first** at HEAD; filing them creates fake progress (gemini sub-agent C, kimi direct read) |
| `bd-new-type-level-seal` / `ProjectedIssue` newtype | `PmService::list_issues` returns `IssueSummary` consumed by ~75+ call sites; type-system trick is local clarifier, not global enforcer (kimi sub-agent C) |
| Standalone §10.5 cleanup bead | Surface too small after bd-3us lands; merged into bd-3sk |
| Standalone §10.3.2 epic | Children deleted; no umbrella needed |
| Extending bd-1va as the index-hygiene-sweep bead | Body describes retracted race-repair framing (§10.3.1 ¶1); cleaner to file fresh bead (bd-3us) and supersede |

### 11.7 Decisions still owed

1. **bd-1va disposition** — close as superseded, or convert to doc-only retraction note? (Brain decision; bd-3us inherits its substantive role.)
2. **bd-3us scope on irreducibles** — `spur:superseded-by:*` and `spur:mutation-id:*` are genuinely label-only per kimi sub-agent A. Reconcile-with-exception or exempt-and-document? Recommendation: exempt-and-document, citing `partial_compare_status` (`projector.rs:412-439`).
3. **Doc §10.3.1 ¶1 follow-up** — update to reflect that the reframed work is now tracked under bd-3us, and the existing partial sweep (3 of 8+ families) is the starting point, not the end state.
4. **Effect Executor pacing** — §11 closes Tier-1. Tier-3 (purify command handlers, introduce Effect Executor per §3 L1 / §6) remains a separate decision cycle.

---

## 12. Cumulative state per tier — before/after with pros/cons

A trajectory view: what the system *is* at the end of each tier, presented as before/after data-flow diagrams plus honest cost ledgers. Use this section as the standing reference for "where are we today?" and "what does the next tier actually buy us?"

### 12.1 Terminology reconciliation

The §6 strangler-fig had 5 tiers; the §10 update (2026-05-15) merged the original Tier-2 "Sever labels" into Tier-1 §10.3. The current naming used in this section:

| Label here | Doc section mapping | State as of 2026-05-16 |
|---|---|---|
| **Tier-0** | §6.0 — Lock the door | Shipped (3 commits, §10.1) |
| **Tier-1** | §6.1 (Fix the fold) + §10.3 (Sever labels, formerly §6.2) | bd-1r8 done; bd-3us + bd-42v + bd-3sk pending (§11) |
| **Tier-2** | §6.3 — Purify command handlers → Effect Executor | Not started |

§6.4 (spur-sim verification tooling) is intentionally omitted here — it is testing infrastructure layered on top of Tier-2, not an architectural state of the system.

### 12.2 Tier-0 — Lock the door

```mermaid
flowchart LR
    subgraph Before_T0[Before Tier-0]
        direction TB
        Cmd_B[handlers] --> Cache_B[active_plans<br/>weak invalidation]
        Cache_B --> Read_B[reader]
        Drift_B[silent drift<br/>no panic, no signal]:::bad
    end
    subgraph After_T0[After Tier-0]
        direction TB
        Cmd_A[handlers] --> Cache_A[active_plans<br/>task-level token]
        Cache_A --> Read_A[reader]
        Assert_A[invariant panics<br/>at mutation sites]:::ok
        Shadow_A[shadow projector<br/>observe-only]:::ok --> Warn[label_audit_drift<br/>warnings]
        Cmd_A -.-> Assert_A
        Cmd_A -.parallel.-> Shadow_A
    end
    classDef ok fill:#efe,stroke:#393
    classDef bad fill:#fee,stroke:#c33
```

| Pros | Cons |
|---|---|
| Near-zero risk — reversible asserts, observe-only shadow | Doesn't fix anything; just exposes it |
| Buys empirical data on actual drift rate | Adds a 2nd projection algorithm → eventual drift between them (manifested as bd-1r8 substrate-alignment gaps) |
| Closes the bd-334 panic class | Defensive asserts only fail loud; silent drift still possible in unguarded paths |

### 12.3 Tier-1 — Fix the fold + sever labels

State at the end of Tier-1 = the world after bd-3us, bd-42v, and bd-3sk all land (per the §11 reframe). bd-1r8 already delivered the single forward-fold.

```mermaid
flowchart TB
    subgraph Before_T1[Before Tier-1]
        direction TB
        Audits_B[(Audit Log)]
        Labels_B[(Labels)]
        Fold_B[shadow forward-fold]
        Scan_B[helpers backwards-scan<br/><b>drifts on Signal/Escalation/Superseded</b>]:::bad
        Cascade_B[project_status_for_issue<br/><b>label-first cascade</b>]:::bad
        State_B[PlanState<br/><b>derived from labels</b>]:::bad
        Reco_B[reconciler<br/>no index hygiene]:::bad

        Audits_B --> Fold_B
        Audits_B --> Scan_B
        Labels_B --> Cascade_B
        Audits_B -.decoration.-> Cascade_B
        Cascade_B --> State_B
    end

    subgraph After_T1[After Tier-1]
        direction TB
        Audits_A[(Audit Log<br/><b>sole authority for Type A state</b>)]:::ok
        Labels_A[(Labels<br/>Type A: indexed cache<br/>Type B: authoritative ephemera)]
        Fold_A[project_status_from_audits<br/><b>single forward-fold</b>]:::ok
        Wrappers_A[helpers as wrappers<br/><i>annihilates drift class</i>]:::ok
        Inverted_A[project_status_for_issue<br/><b>audit-first; labels = hint</b>]:::ok
        Sweep_A[index_hygiene_sweep<br/>all Type A families]:::ok
        State_A[PlanState<br/><b>derived from audits</b>]:::ok
        Drift_A[label_index_drift counter<br/>telemetry, no panic]:::ok

        Audits_A --> Fold_A
        Fold_A --> Wrappers_A
        Wrappers_A --> Inverted_A
        Audits_A --> Inverted_A
        Inverted_A --> State_A
        Labels_A -.drift hint.-> Inverted_A
        Audits_A --> Sweep_A
        Sweep_A --> Labels_A
        Sweep_A --> Drift_A
    end
    classDef ok fill:#efe,stroke:#393
    classDef bad fill:#fee,stroke:#c33
```

| Pros | Cons |
|---|---|
| Drift class bd-334 / `Signal(integration-conflict)` / bare-None Escalation **mathematically impossible** — one fold, helpers wrap it | `superseded-by` / `mutation-id` remain irreducibly label-only (audit stream silent on lineage) |
| `list_issues({labels: […]})` queryability preserved — labels remain a cache, just not a source of truth | Type A/B bifurcation is real complexity; future writers must respect it (footgun documented in §10.2.1) |
| Reconciler sweep makes drift self-healing with `label_index_drift` counter observability | Replay cost on cold-start projection — fold over 50+ comments per task |
| Tier-0 defensive panics can be retired (bd-3sk) — telemetry replaces panics | Schema evolution of `AuditSentinelKind` requires explicit versioning (Open Question §9.4) |
| Per-call-site inversion not actually needed (3 listed sites were already audit-first) — work shrinks to bd-3us + bd-3sk + bd-42v | Effect-after-append discipline must be maintained at every IO site (cultural cost) |

### 12.4 Tier-2 — Purify command handlers + Effect Executor

State going into Tier-2 = state at the end of Tier-1 above. Command handlers still do non-atomic dual-write (emit event AND imperative beads mutation in the same call). The Tier-1 work made the *read* path pure; the *write* path is still impure.

```mermaid
flowchart TB
    subgraph Before_T2[Before Tier-2 — post-Tier-1]
        direction TB
        Brain_B[Brain]
        Handler_B[command handler<br/><b>impure: dual-write</b>]:::bad
        Audit_B[(Audit Log)]
        Beads_B[(Beads labels/status)]
        Cache_B[active_plans cache]

        Brain_B --> Handler_B
        Handler_B -.emits event.-> Audit_B
        Handler_B -.imperative mutation.-> Beads_B
        Handler_B -.invalidates.-> Cache_B
        Note_B[<b>race window:</b><br/>event durable BEFORE labels<br/>OR labels written BEFORE event]:::bad
    end

    subgraph After_T2[After Tier-2 — pure handlers + Effect Executor]
        direction TB
        Brain_A[Brain]
        Handler_A["decide(&State, &Cmd)<br/><b>pure synchronous core</b>"]:::ok
        Audit_A[(Audit Log<br/><b>sole input</b>)]:::ok
        Exec_A[Effect Executor<br/>idempotent, replayable<br/>observes event log]:::ok
        Beads_A[(Beads UI<br/>derived read-model)]
        Git_A[(Git worktrees)]
        Worker_A[(Worker dispatch)]

        Brain_A --> Handler_A
        Audit_A --> Handler_A
        Handler_A -->|new event| Audit_A
        Handler_A -->|effects list| Exec_A
        Audit_A --> Exec_A
        Exec_A -->|idempotent labels| Beads_A
        Exec_A -->|idempotent ops| Git_A
        Exec_A -->|dispatch| Worker_A
        Worker_A -->|completion audit| Audit_A
        Note_A[<b>invariant:</b><br/>events durable before any effect<br/>replay is trivial]:::ok
    end
    classDef ok fill:#efe,stroke:#393
    classDef bad fill:#fee,stroke:#c33
```

| Pros | Cons |
|---|---|
| `fn decide()` and `fn project()` both pure → trivially proptestable. Subsumes most of bd-d1r's 30 scenarios | Calendar: ~6 weeks distributed (§6 estimate), one handler path at a time |
| Replay is trivial: re-run `project` then `decide` on the same audit log | Every effect must be made truly idempotent (worktree create, label update, dispatch ack) — real engineering cost |
| Schema evolution policy becomes urgent (Open Question §9.4): every event variant lives forever in old audit logs | Open Question §9.5: Effect Executor placement — new module in `spur-mcp`, or new `spur-effects` crate? |
| Effect Executor can implement backpressure, retries, and audit-driven recovery uniformly | Debugging shape changes: "read a DB row" → "fold the event stream" (mitigated by mermaid dump on assertion failure — already in simulator design) |
| Closes the Dual-Write anti-pattern (named in §2 L3) for the entire system | The Tier-1 `index_hygiene_sweep` (bd-3us) becomes one specific Effect, not a separate component — possible re-architecture |

### 12.5 Cumulative invariant ledger

| Invariant | Tier-0 | Tier-1 | Tier-2 |
|---|---|---|---|
| Cache invalidates on task-level audits | ✅ | ✅ | ✅ |
| Single projection algorithm (no drift between forward-fold and backwards-scan) | ❌ | ✅ (bd-1r8) | ✅ |
| State derives from audit log alone (Type A) | ❌ | ✅ (post bd-3sk) | ✅ |
| Labels are queryable index, not authority | ❌ | ✅ (post bd-3us) | ✅ |
| Drift is telemetry, not panic | ⚠️ defensive panics | ✅ (post bd-3sk) | ✅ |
| Command handlers are pure | ❌ | ❌ | ✅ |
| Effects happen after durable event append | ❌ | ⚠️ partial (comment-first only) | ✅ |
| `decide(&State, &Command)` testable in isolation | ❌ | ❌ | ✅ |
| Replay is trivial | ❌ | ⚠️ project is, decide isn't | ✅ |

### 12.6 What Tier-2 specifically buys (beyond Tier-1)

Tier-1 makes the *read* substrate honest. Tier-2 makes the *write* substrate honest. The concrete deltas Tier-2 unlocks:

1. **No more dual-write race classes.** bd-1va's original race-repair framing (since retracted under §10.3.1's reframe) becomes structurally impossible: handlers don't write labels, the executor does, and only after the event is durable.
2. **Property tests over command sequences.** `decide` is pure; you can proptest arbitrary `Vec<Command>` sequences against any starting `PlanState` and assert invariants over the resulting effect lists. The bd-d1r simulator's 30 hand-curated scenarios collapse to ~3 property tests.
3. **Schema evolution becomes a first-class concern, not a footgun.** Once events are the only input to both decide and project, additive-only evolution + an explicit `AuditSentinelKind` version field forces every event-touching change through a single schema gate.
4. **Cold-start replay is bounded.** Today, replay requires re-folding the audit log AND re-running handler side-effects from observed labels. After Tier-2, replay is `for event in log { exec(decide(project(prior), event)) }` — deterministic, idempotent, and parallelizable per task.

### 12.7 What Tier-2 does NOT buy

Naming these to avoid scope creep when the Tier-2 decision cycle opens:

- **Does not retire the Beads backend dependency.** Beads remains the audit log substrate (comments) and the queryable index (labels). The Effect Executor still uses `PmService` for both.
- **Does not eliminate the Type A/B bifurcation.** Type B labels (`signal:*`, `spur:lease-expires-at:*`) remain authoritative for their concerns; the Effect Executor handles them through dedicated effect variants, not through the audit log.
- **Does not solve cross-plan transactions.** A command that touches multiple plans still serializes through the executor; atomicity is per-event, not per-command.
- **Does not eliminate worker non-determinism.** Workers still produce diverse completion audits; the executor is downstream of that variability and must handle it through retry / signal pathways already designed in Tier-1.

---

## 13. Operator / executive view — what each tier means for `submit_plan` and plan execution

The architectural tiers in §6 / §10 / §12 are stated in engineering terms (folds, projections, executors). This section restates them in *operator-facing* terms: what changes about the two core flows the system exists to serve — `submit_plan` (brain decomposes work) and plan execution (workers carry it out).

### 13.1 Per-tier flow impact

#### Tier-0 — observability + defensive panics

| `submit_plan` | Plan execution |
|---|---|
| Cache invalidation advances on each task-level audit, so a freshly submitted plan is **immediately visible** to `get_plan_status` / reconciler reads (was: stale-cache window). | Invariant violations now **fail loud at the mutation site** instead of silently corrupting `active_plans`. Shadow projector logs every label-vs-audit drift, so operators learn *how often* the substrate is wrong in production. |

**Operator-visible benefit:** same flows as before; corruption shows up as a panic with context, plus a metric for how broken the old substrate was. No new capabilities — pure signal.

#### Tier-1 — state derives from audits, not labels

| `submit_plan` | Plan execution |
|---|---|
| Brain submits → audit comments are written → **state is correct the instant the comment lands**. Label writes follow as an index cache; if a label write fails, the plan is still in the correct state — only `list_issues({labels:[…]})` queries are briefly stale, and the next reconciler tick patches them via `bd-3us`. | Worker writes Completion audit → status **projects as `AwaitingReview` immediately**. Brain can call `review_task` straight away. The bd-334 class of races (duplicate dispatch after label-lag, spurious `request_changes`, stale `delegation-id` re-fired) becomes **structurally impossible** — labels can't lie because no consumer derives state from them anymore. Drift is a counter (`label_index_drift`), not a panic. |

**Operator-visible benefits:**
- `submit_plan` becomes **idempotent on retry**: if the brain re-issues a submit because of a transport hiccup, the audit-derived state is the same regardless of which label-write half-landed.
- Plan execution becomes **immune to label desync**: workers that crash mid-completion, brains that interleave `review_task` with worker writes, reconcilers that tick during a label update — all converge to the same answer (audit truth) with at most a query-visibility lag.
- New worker outcomes (escalation, integration-conflict signal, supersession via mutation) all propagate to correct status without manual cleanup.
- `bd-3us` adds self-healing: a label that gets manually deleted or hand-edited via the Beads UI is automatically restored on the next reconciler tick.

#### Tier-2 — pure handlers + Effect Executor

| `submit_plan` | Plan execution |
|---|---|
| Handler becomes `decide(&PlanState, SubmitPlan) -> Vec<Effect>` — **pure, synchronous**. Brain gets a response the instant the event is durable; the Effect Executor handles issue creation, label writes, and initial worker dispatches downstream and idempotently. If the orchestrator crashes mid-submit, restart **replays the event log** and the executor finishes the half-done work. Brain can also "dry-run" a submit by calling `decide` on a snapshot — no IO. | Worker completion → single Completion event → executor handles all downstream effects (label updates, lease release, next-dispatch trigger, escalation handoff) uniformly. `review_task`, `request_changes`, `report_signal`, `escalate` are all pure decision functions; the executor is the only IO surface. **Crash recovery is automatic** — on restart, the executor resumes any in-flight effects from the event log. Property tests over arbitrary command sequences replace most of the bd-d1r scenario suite. |

**Operator-visible benefits:**
- `submit_plan` becomes **transactional and replayable**: an OK response means the plan is durable, and any remaining IO will complete (or be safely retried) without intervention.
- Plan execution becomes **crash-resilient**: orchestrator restarts during dispatch / completion / review handoff don't leave half-state. The executor's idempotent ops mean "did this dispatch actually happen?" is always answerable from the log.
- New effect types (a new worker pool, a Slack notification, an audit summarization step) plug in by adding an Effect variant — **handler logic doesn't change**.
- The simulator (`spur-sim` / bd-d1r Tier-4) becomes trivially correct because both `decide` and `project` are pure — most of the 30 scenarios collapse into ~3 property tests.

### 13.2 Cumulative end-to-end flow after Tier-2

```mermaid
sequenceDiagram
    participant Brain
    participant Handler as decide()<br/>(pure)
    participant Log as Audit Log<br/>(sole truth)
    participant Exec as Effect Executor<br/>(idempotent IO)
    participant Beads as Beads UI<br/>(read-model)
    participant Worker

    Note over Brain,Worker: submit_plan
    Brain->>Handler: submit_plan(plan_spec)
    Handler->>Log: append TaskSpec events
    Log-->>Handler: durable
    Handler-->>Brain: OK (plan_id)
    Note over Exec: async, observes log
    Log->>Exec: TaskSpec batch
    Exec->>Beads: create_issues + labels (idempotent)
    Exec->>Worker: dispatch ready tasks (idempotent)

    Note over Brain,Worker: execution
    Worker->>Log: Completion event
    Log-->>Worker: durable
    Note right of Log: status IS AwaitingReview now
    Brain->>Handler: review_task(approve)
    Handler->>Log: append Approval event
    Log-->>Handler: durable
    Handler-->>Brain: OK
    Log->>Exec: Approval event
    Exec->>Beads: relabel + close (idempotent)
    Exec->>Worker: detach worktree (idempotent)
```

Every arrow is either pure (no IO) or idempotent (safe to retry). The brain's view of the world is always derivable by folding the log; an operator can answer "what state is this plan in?" with a single read from the log, no matter what the labels say.

### 13.3 Bottom line — operator question each tier answers

| Tier | Operator question it answers |
|---|---|
| **Tier-0** | "How broken is the substrate right now?" — gives you a metric (`label_audit_drift`) and a panic at corruption sites |
| **Tier-1** | "Can I trust that `submit_plan` and `report_progress` produce the same state I'll observe later?" — yes, because labels stop being the truth |
| **Tier-2** | "If the orchestrator crashes during a submit/dispatch/review, do I lose work?" — no, because the event log is the truth and the executor is replay-driven |

### 13.4 What this does NOT change

To avoid setting wrong expectations when reviewing this document with stakeholders:

- **Worker quality / output variance.** Tiers fix the *substrate*; worker prompt quality, model selection, and review rigor remain the operator's responsibility.
- **Plan decomposition quality.** `submit_plan` becomes more reliable, but the brain's ability to decompose a goal into a tractable DAG is a separate concern (Plan Mutation tooling, bd-3lo lineage, future planning skills).
- **Beads backend latency.** Comment writes and label writes still hit the Beads PM substrate; Tiers make those operations safer, not faster.
- **Cross-plan coordination.** Plans remain independent; multi-plan transactions are not a Tier goal.
- **Cost / token usage.** Architectural changes are about correctness and recoverability, not LLM economics.
