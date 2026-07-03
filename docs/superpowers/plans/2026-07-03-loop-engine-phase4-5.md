# Loop Engine Phases 4–5 — Autonomy Ratchet, Lifecycle Completion, Events, Brain Skill

**Spec:** `docs/superpowers/specs/2026-07-02-loop-engine-design.md` (§D2 autonomy ladder, §3.4 persist extraction, §3.5 remaining tools, §3.6 events)
**Prereq:** phases 1–3 merged to main (`aea594145`); binary rebuilt; loop tools live.

## Gap evaluation (graph-verified 2026-07-03)

| # | Gap | Evidence anchor | Size |
|---|---|---|---|
| G1 | `persist_plan_as_epic` not extracted (spec §3.4) — blocks L3 engine-arming | `McpCallbackServer::submit_plan_as_epic_internal` in `crates/spur-core/src/server/handlers/plan.rs`; `Reconciler` holds `pm`/`feature_gate`/`dispatch` but not the server | M (pure move-refactor) |
| G2 | `kill_loop` missing (spec §3.5) | sibling of `handle_pause_loop` in `server/handlers/plan.rs`; `skipped_loop_run` idiom in `plan/loops/scheduler.rs` for the terminal record | S |
| G3 | `set_loop_autonomy` missing (spec §3.5, D2 ratchet) | `report_only_dispatch_state` at `plan/reconciler/mod.rs:600` reads `spur:autonomy:*` via `parse_autonomy` and suppresses only at `L1`; L2 already "works by absence"; ratchet needs run-record history (`collect_sorted_audits_for_issue` + `LoopRun` outcomes) | M |
| G4 | Loop events absent (spec §3.6) | `SpurEventBody` enum `crates/spur-acp/src/domain/events.rs:521`; emission anchors: `run_loop_scheduler_sweep` (`plan/loops/scheduler.rs:14-140` — push/rearm/auto-pause sites), `maybe_emit_loop_run_record` (`plan/reconciler/terminal.rs`), `handle_pause_loop`/`handle_resume_loop`. Consumer blast radius (analyst, edges into events.rs): `session_synopsis/projection.rs` (28), `executor_events_roundtrip.rs` (26), `spur-tui views/session_detail` (16), `views/plan_inspector.rs` (8), `views/plan_browser.rs` (7), `event_replay.rs` (8) | M–L |
| G5 | L3 engine-arming absent (spec D2 row 3) | `run_loop_scheduler_sweep` currently always pushes a loop-due continuation; L3 must call the extracted persist fn with the stored template verbatim | M (after G1, G3) |
| G6 | No brain-side `loop-generation-authoring` skill | bundled skills live at `assets/skills/<id>/SKILL.md` (loaded via `skills::all_bundled_raw`, override at `.spur/skills/<id>/SKILL.md`); `brain-delegation` shows the injected-into-brain pattern | S (docs-only) |

Non-gaps confirmed: L1 `ReportOnly` enforcement, budget gate, backoff/auto-pause, run
records, pause/resume, overlap skip — all live and tested on main.

## Task DAG

```
T1 persist extraction ──► T5 L3 engine-arming
T3 set_loop_autonomy  ──► T5
T2 kill_loop          (independent)
T4 loop events        (independent)
T6 authoring skill    (independent, docs)
```

### Task T1: extract `persist_plan_as_epic` (spec §3.4)

- Characterization tests FIRST: pin current `submit_plan` persistence behavior via
  existing `submit_plan_persist` tests in `server/handlers/plan_tests.rs`; add any
  missing coverage for labels/audit emission before moving code.
- Move the epic+tasks+labels+`PlanSubmit`-audit persistence from
  `submit_plan_as_epic_internal` into
  `crate::plan::persist_plan_as_epic(pm: &dyn PmLike, feature_gate: &FeatureGate, spec: &DelegationPlan, provenance: PlanProvenance) -> anyhow::Result<PlanId>`.
- Handler stays: validation + dedup + response shaping. Zero behavior change.
- Gate: `require_feature(PM_PRO_BEADS_ADVANCED)` inside the moved fn (static guard
  scans `.advanced()` call sites — same trap T3-phase1 hit).
- Commit: `refactor(spur-core): extract persist_plan_as_epic for engine arming`

### Task T2: `kill_loop` MCP tool (spec §3.5)

- `LoopIdParams` reused. Handler `handle_kill_loop`:
  load loop issue by `spur:loop-id:*` (reuse `load_loop_issue`), append terminal
  `LoopRun` record with `outcome: "retired"` (reuse `skipped_loop_run` shape via a
  `retired_loop_run` helper in `plan/loops/run_record.rs`), remove all
  `spur:loop-next-run:*` labels, close the issue.
- Scheduler safety: `run_loop_scheduler_sweep` already filters `status: open` —
  closed loop = never swept. Add a test proving a killed loop never re-arms even if
  a stale next-run label survives.
- Idempotent: kill on already-closed loop returns current state, no duplicate record.
- Registration: tool def with locked user-facing description + dispatch match +
  `PLAN_REMAINDER_TOOL_NAMES`/`PLAN_TOOL_NAMES` + `WORKER_DENIED_TOOL_CALLS` +
  `tests/tool_catalog.rs` + `tests/mcp_signals_catalog.rs` (same 6-place checklist as
  phase-3 T7).
- Tests: `kill_loop_closes_issue_and_writes_retired_record`,
  `killed_loop_is_never_rearmed_by_sweep`.
- Commits: `test(spur-core): kill_loop retires loop issue` → `feat(spur-core): kill_loop mcp tool`

### Task T3: `set_loop_autonomy` MCP tool with ratchet (spec §3.5 + D2 rule)

- Params: `SetLoopAutonomyParams { loop_id: String, level: String /* l1|l2|l3 */ }`.
- Semantics:
  - **Demotion always allowed** (any level → lower level).
  - **Promotion ratcheted**: require ≥ `RATCHET_MIN_STABLE_GENERATIONS` (const, 3)
    consecutive `LoopRun` records with `outcome == "approved"` at the *current* level
    (scan audits via `collect_sorted_audits_for_issue`, newest-first, stop at first
    non-real-generation outcome — reuse the "real generation" predicate from the T6
    phase-1 fix: `approved|partial|failed` only; skips don't count for or against).
  - One level per call (L1→L3 direct is rejected).
- Effect: rewrite `LoopSpec.autonomy` in the sentinel body AND swap the
  `spur:autonomy:*` label on the loop issue in one `update_issue`. Both must stay
  consistent — `report_only_dispatch_state` reads generation-epic labels which the
  brain copies from the loop issue at authoring time (documented in T6 skill).
- Tests: promotion blocked below threshold (error names the shortfall), promotion
  allowed at threshold, demotion always allowed, direct L1→L3 rejected, body+label
  both updated.
- Commits: TDD pair per repo convention.

### Task T4: loop events (spec §3.6)

- New `SpurEventBody` variants (payloads bounded: ids + counters only, per the
  broadcast-sizing invariant):
  - `LoopArmed { loop_id, generation, next_run }` — emit in `run_loop_scheduler_sweep`
    after `push_loop_due_continuation` + in `rearm_loop`.
  - `LoopGenerationStarted { loop_id, generation, plan_id }` — emit in
    `persist_plan_as_epic` (T1) when the epic carries `spur:loop-id:*`.
  - `LoopRunRecorded { loop_id, generation, outcome, cost_micros }` — emit in
    `maybe_emit_loop_run_record` (`plan/reconciler/terminal.rs`).
  - `LoopPaused { loop_id, by: paused|auto_paused|resumed|retired }` — emit in
    `handle_pause_loop`/`handle_resume_loop`/`auto_pause_failed_loop`/`kill_loop`.
- Emission surface: investigate how `Reconciler` reaches the event sink — anchor
  `event_sink.rs::enforce_event_cap` and the `SpurEvent.seq` allocator; if the
  reconciler has no sink handle today, thread the existing one (do NOT invent a new
  channel).
- Round-trip serialization tests modeled on
  `crates/spur-acp/tests/executor_events_roundtrip.rs` (mandatory per repo rules).
- Consumers (from the analyst blast-radius query): update exhaustive matches in
  `session_synopsis/projection.rs` and `event_replay.rs`; add minimal display arms in
  `spur-tui` `plan_inspector.rs`/`plan_browser.rs` (loop rows: id, generation,
  outcome badge) — TUI behavior change, screenshot in PR.
- **Run the `spur-invariants-reviewer` agent on this diff before commit** (touches
  broadcast payloads + event plumbing).
- Commits: TDD pair; TUI change noted in PR body.

### Task T5: L3 engine-arming (spec D2 row 3; needs T1, T3)

- In `run_loop_scheduler_sweep`, where the loop-due continuation is pushed: if
  `spec.autonomy == AutonomyLevel::L3`, instead call
  `crate::plan::persist_plan_as_epic` with the stored template verbatim, labeling the
  epic `spur:loop-id:<id>` + `spur:loop-generation:<n>` + `spur:autonomy:l3` (+
  `spur:loop-budget-micros` mapped from `governors.max_cost_micros_per_generation`).
- Auto-merge stays within the existing v0e allowlist mechanism
  (`auto_merge_approved_plans` + allowlist config); no new merge path.
- L1/L2 keep the continuation path unchanged.
- Tests: L3 loop arms a generation with zero brain involvement (mock PM: epic exists
  with correct labels after one tick); L1/L2 still push continuations; template
  re-submitted verbatim (byte-equal task bodies).

### Task T6: `loop-generation-authoring` bundled skill

- New `assets/skills/loop-generation-authoring/SKILL.md` (spur-managed header like
  siblings). Content contract:
  - Trigger: brain receives a `ContinuationSource::LoopDue` continuation.
  - Steps: `get_loop_status` → check pause/backoff/escalation → author generation via
    `submit_plan` copying template tasks, epic labels `spur:loop-id`,
    `spur:loop-generation:<n>`, `spur:autonomy:<loop level>`,
    `spur:loop-budget-micros:<max_cost_micros_per_generation>`; mandatory first task
    keeps `spur:loop-triage-task`; never author tasks beyond the template at L1
    (ReportOnly suppresses them anyway — don't waste the tokens); review triage
    output; close out and let the terminal hook write the run record.
  - Escalation: on `ContinuationSource::LoopEscalation`, summarize run-record history
    and surface to the user; never self-promote autonomy.
- Wire into the brain injection set alongside `brain-delegation` (same mechanism).
- Update `CLAUDE.md`/`AGENTS` skill tables mentioning the new skill.
- Commit: `docs(spur-core): loop generation authoring skill`

## Verification (whole plan)

- Per-task: targeted tests as listed + `scripts/spur-cargo test -p spur-core`
  (main now has the sqlite VFS fix — full suite is safe again) +
  `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`.
- T4 additionally: `scripts/spur-cargo test -p spur-acp` (round-trips) and
  `spur-invariants-reviewer` pass.
- End-to-end after T6: submit a real L1 pilot loop (ci-sweeper template), observe one
  full generation cycle (arm → continuation → authored generation → triage → run
  record → rearm), then `kill_loop` and verify the retired record.
