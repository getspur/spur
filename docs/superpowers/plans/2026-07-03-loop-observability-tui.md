# Loop Observability in the TUI (loop-engine phase 6)

Date: 2026-07-03
Status: proposed
Depends on: loop-engine phases 1–5 (merged; see `2026-07-03-loop-engine-phase4-5.md`)

## Problem

The loop engine is fully operational (submit → arm → LoopDue → generation → LoopRun
records → governors → ratchet), but a TUI user has **no way to see it**. Every loop
signal that reaches the TUI today is a transient one-liner:

- `plan_browser.rs:1098-1126` maps the four loop events to `self.hint` — a single
  status-bar string overwritten by the next event.
- `plan_inspector.rs:958-987` maps the same events to `loop_event_status`, rendered
  in the footer at `plan_inspector.rs:1257` — same transience.

There is no way to answer, from the TUI: *which loops exist, what state are they in,
when do they fire next, what did their recent generations do, and how do I pause one?*
Today the only path is asking the brain to call `get_loop_status` — which itself
requires already knowing the `loop_id`.

## Gap inventory

| # | Gap | Evidence |
|---|-----|----------|
| O1 | No loop discovery anywhere — no "list loops" in TUI **or** MCP; `get_loop_status` takes a required `loop_id` | `server/handlers/plan.rs:1772` |
| O2 | Loop events in TUI are ephemeral hint strings, not state | `plan_browser.rs:1098`, `plan_inspector.rs:958` |
| O3 | No loop detail surface (spec, governors, recent runs, backoff, ratchet streak) | assembled only inside `handle_get_loop_status` (`plan.rs:1797-1894`) |
| O4 | Generation plans are indistinguishable from hand-submitted plans in plan browser/inspector — no loop provenance | `PlanSummaryEvent` (`spur-acp/src/domain/events.rs:241`) has no loop fields |
| O5 | No operator controls (pause/resume/kill) reachable from the TUI | loop mutations only via brain-side MCP tools |

## Design decision: dedicated `LoopBrowserView`, not a plan-browser filter

Model the new surface on `PlanBrowserView` (`crates/spur-tui/src/views/plan_browser.rs`)
but as its own view, because:

1. **Loops are global, plans are session-scoped.** `NavigateTo(ViewId::PlanBrowser)`
   refuses to open without an active brain session (`app/action_routing/nav.rs:68-77`).
   Loop *browsing* needs only `pm_service` (labels + issue bodies), exactly like
   `RefreshPlans` (`orchestrator/interactive_loop.rs:762-787`) — it must work with no
   brain running. Only mutations (pause/resume/kill) require the brain's
   `mcp_server`, mirroring `ClaimPlan` (`interactive_loop.rs:790-832`).
2. **Different row schema.** cadence / next-run countdown / autonomy / failure streak
   vs. owner / lifecycle / task counts.
3. **Different operations.** pause/resume/kill (+ later set-autonomy) vs. claim/start.

Generation plans keep living in the plan browser; the two views cross-link (O4).

## Data plumbing (mirrors the plans pattern 1:1)

Existing chain to copy:
`plan_browser 'r'` → `Action::RefreshPlans` (`plan_browser.rs:1006`) →
`UserInput::RefreshPlans` (`app/action_routing/pm_actions.rs:16-19`) →
`InteractiveInput::RefreshPlans` (`orchestrator/input.rs:85`) →
`load_plan_summaries` (`orchestrator/plan_ops.rs:271`) →
`SpurEventBody::PlansLoaded` (`events.rs:884`) → view `handle_spur_event`.

New chain:

- **Discovery** — `load_loop_summaries(pm)`: `pm.list_issues(IssueFilter {
  issue_type: Some("task"), include_closed: true, limit: Some(1000), .. })`, keep
  issues whose labels match `parse_loop_id` (`plan/labels.rs:231`). Same
  list-then-filter idiom as `load_plan_summaries` scanning all epics
  (`plan_ops.rs:275-288`). Per-row assembly from labels + `LoopSpec::parse(body)`:
  autonomy (`AUTONOMY_PREFIX`), paused (`LOOP_PAUSED`), retired (issue closed),
  `parse_loop_next_run`, `parse_loop_generation`, `parse_loop_budget_micros`,
  cadence/goal-preview/max-tasks from the spec. Last-run enrichment via
  `advanced.list_comments` per loop follows the existing per-epic N+1 precedent
  (`plan_ops.rs:291-309`) with warn-and-continue on error.
- **Detail** — extract the assembly body of `handle_get_loop_status`
  (`plan.rs:1797-1894`: load issue → parse spec → `collect_sorted_audits_for_issue`
  → `trailing_failed_loop_runs` → `effective_interval_secs` → next_run/paused labels)
  into a shared `build_loop_status(pm, loop_id, recent_limit)` in
  `crates/spur-core/src/plan/loops/status.rs`, reused by the MCP handler
  (behavior-preserving refactor) and by the new `InspectLoop` input.
- **Events** — new `SpurEventBody` variants beside the loop events at
  `events.rs:1255`:
  - `LoopsLoaded { loops: Vec<LoopSummaryEvent>, warnings: Vec<String> }`
  - `LoopDetailLoaded { detail: LoopDetailEvent }`
  - `LoopCommandError { operation: String, loop_id: Option<String>, error: String }`
  All payloads bounded (broadcast-sizing invariant): goal preview truncated like
  `issue_body_preview`, `recent_runs` capped at 20, emitted loop rows capped at 200
  with a warning when truncated. Round-trip serialization tests required per repo
  convention (`crates/spur-acp/tests/executor_events_roundtrip.rs` style).
- **Inputs** — `InteractiveInput::{RefreshLoops, InspectLoop { loop_id },
  PauseLoop { loop_id }, ResumeLoop { loop_id }, KillLoop { loop_id }}` handled in
  `interactive_loop.rs` next to the RefreshPlans/ClaimPlan blocks. Mutations route
  through new `call_pause_loop` / `call_resume_loop` / `call_kill_loop` wrappers on
  `McpCallbackServer` beside `call_claim_plan` (`plan.rs:328`), so pause/resume/kill
  reuse the full governed handlers (audit comments, `LoopPaused` emission,
  retirement-generation bookkeeping) rather than re-implementing label flips.
- **Feature gate** — loop MCP handlers gate on `PM_PRO_BEADS_ADVANCED`. Summary
  loading needs only `list_issues` + spec parse (works on base beads); run-history
  enrichment and detail need `pm.advanced()`. Degrade gracefully: rows render with
  "runs: n/a", detail shows the gate message — mirroring `plan.rs:1827-1833`.

## TUI surface

- `ViewId::LoopBrowser` (`action.rs:288`), lazy `Option<LoopBrowserView>` on App
  (`app/mod.rs:342` pattern), nav arm in `action_routing/nav.rs` (no session
  requirement), dispatch at the `app/mod.rs:567/669` and `input.rs:247` match sites.
- Entry points: `L` from plan browser (extend `STATUS_HINT`, `plan_browser.rs:26-29`)
  and `L` from dashboard; loop browser `b`/`Esc` navigate back symmetrically with
  `plan_browser.rs:1025-1035`.
- **List columns**: short loop id · title · autonomy (L1/L2/L3) · state
  (active/paused/auto-paused/retired) · cadence→effective interval (backoff marker
  when stretched) · next run countdown ("in 42m" / "due"; complement of
  `format_relative_time`, `plan_browser.rs:105`) · last gen + outcome · consecutive
  failures.
- **Sort modes**: NextRun (default) / Title / State / LastOutcome. **Filters**:
  All / Active / Paused / Retired. Same `view_index` selection-stability mechanics
  as `plan_browser.rs:187-219`.
- **Detail peek** (bottom panel, like `render_detail`): goal preview, governors
  (budget/gen, daily cap, max tasks, backoff k/factor/auto-pause), recent runs table
  (gen · outcome · cost · autonomy stamp), ratchet streak ("2/3 approved at L1"),
  today's generation count vs cap.
- **Keys**: `j/k/g/G` navigate · `r` refresh · `Enter` inspect (requests
  `InspectLoop`, renders `LoopDetailLoaded`) · `o` open the loop issue via existing
  `Action::OpenIssueInBacklog` (`plan_inspector.rs:887`) · `p` pause/resume toggle
  with confirm modal · `x` kill with confirm modal (reuse the `PlanConfirm` popup
  pattern, `plan_browser.rs:163-168`) · `S` sort · `f` filter · `Esc/q` back.
  Mutations flash "No active brain session" when `mcp_server` is absent, mirroring
  ClaimPlan messaging (`interactive_loop.rs:821-825`).
- **Live updates**: `handle_spur_event` applies `LoopArmed` (next_run, generation),
  `LoopRunRecorded` (last outcome/cost; prepend to open detail), `LoopPaused`
  (state by `paused|auto_paused|resumed|retired`) to rows in place. Unknown
  `loop_id` → hint "new loop activity — press r to refresh". Keep the one-line hint
  behavior as a secondary signal.
- **Loop provenance on plans (O4)**: add optional
  `loop_origin: Option<PlanLoopOriginEvent> { loop_id, generation }` to
  `PlanSummaryEvent` (serde-defaulted for back-compat), populated in
  `load_plan_summaries` from epic labels. Render a `⟳ gen N` badge in the plan list
  row and in the plan inspector header (`plan_inspector.rs:1034-1051`).

## Tasks (linear chain)

| ID | Crate | Summary |
|----|-------|---------|
| T1 | spur-acp | `LoopSummaryEvent`/`LoopDetailEvent` types; `LoopsLoaded`/`LoopDetailLoaded`/`LoopCommandError` variants; `loop_origin` on `PlanSummaryEvent`; round-trip serialization tests |
| T2 | spur-core | Extract `build_loop_status` into `plan/loops/status.rs` (refactor `handle_get_loop_status` to reuse, behavior-preserving); add `load_loop_summaries`; unit tests over label/spec assembly incl. missing-advanced degradation |
| T3 | spur-core + spur-tui + spur-cli | `InteractiveInput::{RefreshLoops,InspectLoop,PauseLoop,ResumeLoop,KillLoop}` + matching `UserInput` variants (`spur-tui/src/app/mod.rs:89`) + the `UserInput → InteractiveInput` mapping in `spur-cli/src/main.rs:~1998`; `call_pause_loop`/`call_resume_loop`/`call_kill_loop` wrappers; interactive-loop wiring emitting the new events; tests |
| T4 | spur-tui | `LoopBrowserView` (table, sort/filter, detail peek, confirm modals, live event application); `ViewId::LoopBrowser`; nav/dispatch/keybinding wiring incl. `components/status_bar.rs` hints; in-file view tests mirroring `plan_browser.rs:1341+` **plus** an integration snapshot suite `crates/spur-tui/tests/loop_browser_snapshot.rs` modeled on `plan_browser_snapshot.rs` |
| T5 | spur-tui | Plan-side provenance: `⟳ gen N` badge in plan browser rows + plan inspector header; upgraded loop hints (deep-link `LoopGenerationStarted` → `focus_plan_id`, `plan_browser.rs:235`); update `plan_inspector_snapshot.rs` / `plan_browser_snapshot.rs` expectations for the badge |
| T6 | docs | `docs/loops.md` "Observing loops in the TUI" section; keybinding/help updates; run `spur-invariants-reviewer` over the event-plumbing diff |

## Grounding review (2026-07-03, graph + analyst pass)

Verified with `code_*` graph tools and spur-analyst SQL (analyst DB stale vs live
pointer — doc-only commits since build; loop symbols present with matching line
anchors, so aggregates trusted with `allow_stale`):

- **`handle_get_loop_status` has exactly one caller** — the tool dispatch
  `PlanMcpModule::call_with_server` (`crates/spur-core/src/mcp/plan.rs`). The T2
  extraction refactor is graph-verified low-risk.
- **`load_plan_summaries` has one calling symbol** (`Orchestrator::run_interactive`,
  three call sites within it) — the T5 `loop_origin` extension ripples nowhere else.
- **Co-change ring for `plan_browser.rs`** (top partners, all-history):
  `plan_inspector.rs` ×11, `tests/plan_browser_snapshot.rs` ×10, `issue_browser.rs`
  ×10, `spur-acp/src/domain/events.rs` ×9, `tests/executor_events_roundtrip.rs` ×9,
  `plan_inspector_snapshot.rs` ×8, `action.rs` ×7, `components/status_bar.rs` ×7.
  This drove the T4/T5 additions: snapshot test suites and status-bar surface are
  empirically part of every view change; the events.rs + roundtrip pairing
  confirms T1's test requirement.
- **Discovery hardening**: rows whose issue body fails `LoopSpec::parse` must be
  skipped with a warning (defends against any future task issue that carries a
  `spur:loop-id:*` label without being a loop root — e.g. mislabeled triage tasks).
  `load_loop_issue_with_closed` (`plan.rs:191`) takes `.first()` of `limit: 2`, so
  the engine already assumes label uniqueness; the browser must not.
- `PlanSummaryEvent` is declared at `events.rs:243` (doc comment at 241).

## Invariant callouts

- **Broadcast sizing**: `LoopsLoaded` is the only new potentially-large payload —
  bounded by the 200-row cap + truncated previews + 20-run cap. `PlansLoaded`
  already carries `Vec<PlanSummaryEvent>` as precedent.
- **TUI drain cap / SpurEvent.seq / append_message walkback / ACP grace**: untouched
  (no new channels, no seq changes) — but T6 runs the invariants reviewer to confirm.

## Verification

- `scripts/spur-cargo test -p spur-acp` (round-trips), `-p spur-core` (status/summary
  loaders, input wiring), `-p spur-tui` (view tests).
- Manual: submit a loop (e.g. the live ci-sweeper `e86b57b056904776`), open the loop
  browser with no brain session (rows render), start a brain, watch `LoopArmed` /
  `LoopRunRecorded` update the row live, pause/resume from the view.
