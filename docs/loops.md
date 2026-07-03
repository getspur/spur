# Loop Operator Guide

Loops are durable, self-re-arming plan templates stored as beads issues with a
`[[spur-loop v1]]` sentinel. Each generation is still an ordinary SPUR plan: the loop
scheduler only decides when to arm the next generation and enforces governors.

## Runtime Switches

Loops are enabled by default to preserve existing behavior:

```toml
[spur]
loops_enabled = true
pause_all_loops = false
```

Set `loops_enabled = false` to disable loop scheduler sweeps at startup. Set
`pause_all_loops = true` to start the scheduler globally paused. Operators can also
pause all loops by keeping an open beads issue labeled `spur:pause-all-loops`.

Per-loop pause uses the `spur:loop-paused` label. Pausing stops new dispatch decisions;
in-flight delegations are not cancelled and still persist their outcomes.

## Tool Reference

| Tool | Locked description |
|---|---|
| `submit_loop` | Create a durable loop issue with a `[[spur-loop v1]]` spec sentinel. Validates cadence_secs >= 60, defaults omitted autonomy to l1, requires at least one template task marked spur:loop-triage-task, rejects non-positive governor caps, mints a compact loop_id, and labels the loop spur:loop-id:<id>, spur:autonomy:<level>, and spur:loop-next-run:<now> so it fires immediately. |
| `get_loop_status` | Return loop status as JSON for a loop_id: parsed LoopSpec, last recent_runs LoopRun audit records, effective backoff interval, consecutive failure count, paused flag, and next_run timestamp. |
| `pause_loop` | Pause a loop by adding spur:loop-paused to the loop issue identified by loop_id. Existing in-flight generations are not cancelled. |
| `resume_loop` | Resume a paused loop by removing spur:loop-paused and replacing any spur:loop-next-run:* label with spur:loop-next-run:<now>, clearing failure backoff so the scheduler may run it immediately. |
| `kill_loop` | Retire a loop by appending a terminal LoopRun audit record with outcome retired, removing all spur:loop-next-run:* labels, and closing the loop issue identified by loop_id. Repeated calls on an already-closed loop return current state without writing another record. |
| `set_loop_autonomy` | Set a loop's autonomy to l1, l2, or l3. Demotions are immediate; promotions advance one level at a time and require three consecutive approved real generations at the current level. Updates the `[[spur-loop v1]]` spec body and spur:autonomy:* label together. |

The phases 4-5 control surface is MCP-only for `kill_loop` and
`set_loop_autonomy`; TUI actions for those controls are intentionally deferred.

## Autonomy

`l1` is report mode. The scheduler pushes a `LoopDue` continuation, the brain checks
status, and the authored generation should keep to the triage template. Dispatch of
non-triage action tasks is suppressed by the `ReportOnly` guard.

`l2` is assisted mode. The scheduler still pushes a `LoopDue` continuation and the
brain authors the generation, but action tasks dispatch normally through the plan
review gate.

For brain-authored `l1` and `l2` generations, `submit_plan` returns the generation
epic id. The brain then immediately applies `spur:loop-id:<id>`,
`spur:loop-generation:<n>`, `spur:autonomy:<level>`, and
`spur:loop-budget-micros:<value>` with `update_issue`. In v1, this post-submit
labeling means `LoopGenerationStarted` fires only for engine-armed `l3`
generations, not brain-authored `l1` or `l2` generations.

`l3` is unattended mode. The scheduler instantiates the stored template directly with
`persist_plan_as_epic`; the template is submitted verbatim with loop identity,
generation, autonomy, and budget labels.

`l3`-armed generations are not inserted into the server's in-memory plan cache.
`get_plan_status` and merge flows rebuild those generations from the beads
projection, which is fully supported.

An `l3` template may pin its base with a top-level `base` field:
`{"kind":"branch","name":"main"}` or
`{"kind":"commit","oid":"<commit-oid>"}`. Without `base`, the generation has no
pinned base snapshot and workers base on repo main at dispatch time. Pin the base
for reproducible unattended generations.

Promotion is ratcheted. A loop must have at least three consecutive approved real
generations at its current level before it can move up one level. Demotion is
immediate. A direct `l1` to `l3` promotion is rejected.

## Governors

`max_cost_micros_per_generation` is copied to generation epics as
`spur:loop-budget-micros:<value>` and enforced during dispatch. Once the generation
spend reaches the cap, new ready tasks are skipped with budget-exhausted state.

`max_generations_per_day` caps loop starts in a rolling 24-hour window. When the cap is
hit, the scheduler writes a skipped run record and moves `spur:loop-next-run:*`
forward by the effective cadence.

Overlap is skipped. If an earlier generation for the same `loop_id` is still live,
the scheduler writes a `skipped_overlap` run record and re-arms for the next cadence
instead of starting another generation.

`consecutive_failure_backoff` self-damps failing loops. After `k` consecutive
non-approved real generations, the effective cadence is multiplied by `factor`. At
`auto_pause_after`, the scheduler adds `spur:loop-paused`, writes an auto-pause event
and run record, and pushes an escalation continuation.

Escalation is informational. On loop escalation, call `get_loop_status`, summarize
recent run history, consecutive failures, backoff, and paused state, then ask for a
human decision. Do not self-promote autonomy.

## LoopDue Contract

`ContinuationSource::LoopDue` currently carries free-form summary text. The text may
include loop id, generation, and template JSON, but it is not a structured API.
`get_loop_status` is authoritative for the loop spec, pause state, recent run records,
backoff, and next run. Treat the continuation as a wake-up notice and call
`get_loop_status` before authoring or skipping a generation.

## Observing Loops in the TUI

Open the loop browser with `L` from the plan browser. The loop browser itself is
global: browsing and refresh use the issue tracker projection and do not require an
active brain session. The first open loads loops automatically; press `r` to refresh
after that.

The loop browser is an operator view, not a plan filter. Generation plans still appear
in the plan browser and inspector, where loop-created plans carry an `⟳ gen N` badge.
The badge means "this plan came from loop generation `N`"; the loop id is retained in
the plan event data even though the compact badge only prints the generation.

Loop rows use these columns:

| Column | Meaning |
|---|---|
| `Loop` | Compact loop id. |
| `Title` | Source loop issue title. |
| `Aut` | Autonomy label, rendered as `L1`, `L2`, or `L3` when present. |
| `State` | `active`, `paused`, `auto-paused`, or `retired`. |
| `Cad→Eff` | Configured cadence and current effective interval. A trailing `*` marks active failure backoff. |
| `Next run` | Countdown such as `due`, `in <1m`, `in 42m`, `in 3h`, or `—`; paused rows show `paused`. |
| `Last run` | Last generation, outcome, and cost when run history is available, for example `g4 approved $0.37`. |
| `Fails` | Consecutive failure count derived from recent loop run audits. |

Keys in the loop browser:

| Key | Action |
|---|---|
| `j` / `k`, arrows | Move selection. |
| `g` / `G` | Jump to first or last visible row. |
| `Enter` | Inspect the selected loop; pressing `Enter` again on the loaded detail returns to summary. |
| `o` | Open the source loop issue in the backlog view. |
| `p` | Open a pause or resume confirmation modal for the selected loop. |
| `x` | Open the retire confirmation modal. This calls the loop kill action after confirmation. |
| `Enter` in a modal | Confirm pause, resume, or retire. |
| `Esc` / `q` in a modal | Cancel the modal. |
| `S` | Cycle sort order: next run, title, state, last outcome. |
| `f` | Cycle filter: all, active, paused, retired. |
| `Esc` | Close detail first, then navigate back. |
| `q` | Navigate back. |
| `r` | Reload loop summaries. |

Pause, resume, and retire require the active brain session's MCP server because the
TUI routes them through the same governed loop tools as the brain. With no active
brain session, browsing still works, but mutations flash a command error instead of
changing labels. Retired rows cannot be paused or resumed from the browser.

When the beads advanced comments API is unavailable, loop discovery still renders from
labels and the `[[spur-loop v1]]` spec. Run-derived fields degrade: last run is `--`,
failure count starts at `0`, and detail/recent run history is unavailable until the
advanced comments API is available.

Live loop events update rows in place. `LoopArmed` refreshes the generation, next-run
timestamp, and active state. `LoopRunRecorded` refreshes last run, cost, and the
failure streak, and prepends the run to any open detail payload. `LoopPaused` maps
`paused`, `auto_paused`, `resumed`, and `retired` into the row state. If an event
arrives for an unknown loop id, the browser keeps the current list and hints to press
`r`.

Payloads are intentionally bounded. A summary load emits at most 200 loops, with a
warning when truncated. Detail loads request at most 20 recent runs; the detail panel
renders the newest visible rows from that bounded payload.

For implementation background, see
[`docs/superpowers/plans/2026-07-03-loop-observability-tui.md`](superpowers/plans/2026-07-03-loop-observability-tui.md)
and the static mockup at
[`docs/superpowers/design/2026-07-03-loop-browser-mockup.html`](superpowers/design/2026-07-03-loop-browser-mockup.html).

## L1 CI Sweeper Pilot

Use this as the first production-style pilot:

```json
{
  "goal": "Keep CI green on main",
  "pattern": "ci-sweeper",
  "cadence_secs": 3600,
  "autonomy": "l1",
  "template": {
    "tasks": [
      {
        "task_id": "triage",
        "agent": "codex",
        "task": "Inspect the latest CI status, identify failing jobs, and report the smallest safe next action.",
        "labels": ["spur:loop-triage-task"]
      }
    ]
  },
  "governors": {
    "max_cost_micros_per_generation": 2000000,
    "max_generations_per_day": 24,
    "max_tasks_per_generation": 3,
    "consecutive_failure_backoff": {
      "k": 2,
      "factor": 2,
      "auto_pause_after": 4
    }
  },
  "escalation": {
    "after_unresolved_generations": 3
  }
}
```

Submit it with `submit_loop`. When the first `LoopDue` arrives, call
`get_loop_status`, author the triage-only generation with `submit_plan`, review the
triage result, and let the terminal hook write the run record. Keep the loop at `l1`
until at least three consecutive approved real generations prove the template is
stable.
