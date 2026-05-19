# bd-mp246 RCA Evaluation

Date: 2026-05-19  
Role: independent evaluator, falsification-first  
Method: first-principles + double-loop + `sequentialthinking` MCP, 10 thoughts recorded before file/code conclusions

## Section 1 — Facts Verified

### Plan persistence and current DB state

1. Current DB confirms the failing plan's epic/task mapping:
   - SQL: `issues` + `labels` show `bd-2rbtl` has `spur:plan-id:72e5f4dc-14bc-4345-ad7b-23c003ab3e41` and `spur:plan-complete`.
   - SQL: `issues` + `labels` show `bd-319jg` has `spur:plan-id:72e5f4dc-14bc-4345-ad7b-23c003ab3e41`, `spur:plan-task-id:plan-browser-times-sort-filter`, and `spur:agent:codex`.
   - SQL: both `bd-2rbtl` and `bd-319jg` are now `closed`; events show they changed from `open` to `closed` at `2026-05-18T23:02:17.555426+00:00` and `2026-05-18T23:02:16.084427+00:00`.

2. Current DB confirms the comparison plan shape:
   - SQL: `bd-3b3wr` has `spur:plan-id:3b82cc55-433b-4e4f-8f41-58fb46b2203f` and `spur:plan-complete`.
   - SQL: `bd-uiveo`, `bd-2rxd9`, `bd-afx1o`, `bd-3sx80` have the same plan-id label and task-id labels.
   - SQL: `bd-3b3wr`, `bd-uiveo`, `bd-2rxd9`, `bd-afx1o`, and `bd-3sx80` are currently `open`.

3. Event history preserves the important time ordering for the failing plan:
   - SQL `events`: `bd-2rbtl` created at `2026-05-18T22:55:19.237812+00:00`.
   - SQL `events`: `bd-319jg` created at `2026-05-18T22:55:19.241033+00:00`.
   - SQL `events`: `bd-319jg` got plan/task/agent labels at `2026-05-18T22:55:19.242101+00:00`.
   - SQL `events`: `bd-319jg -> bd-2rbtl` parent-child edge inserted at `2026-05-18T22:55:19.243589+00:00`.
   - SQL `events`: `bd-2rbtl` got owner and `spur:plan-complete` at `2026-05-18T22:55:19.248828+00:00` and `2026-05-18T22:55:19.249748+00:00`.
   - SQL `events`: force reclaim audit was written at `2026-05-18T22:58:12.609321+00:00` with `prior_owner == new_owner == 693ce1f372cf6692`.
   - SQL `events`: task/epic cleanup did not happen until about 7 minutes after creation.

4. Dependency shape is the same for failing and working first tasks:
   - SQL `dependencies`: `bd-319jg | bd-2rbtl | parent-child`.
   - SQL `dependencies`: `bd-uiveo | bd-3b3wr | parent-child`.
   - SQL also shows later comparison-plan tasks blocked by `bd-uiveo`, but that does not affect `bd-uiveo` itself.

### Ready view vs actual ready implementation

5. `ready_issues` is a view, not a table:
   - SQL `sqlite_master`: `ready_issues` has `type='view'`.
   - SQL `sqlite_master`: `blocked_issues_cache` has `type='table'`.
   - SQL `sqlite_master`: no triggers exist.

6. The `ready_issues` view CTE blocks direct `blocks` dependencies and propagates through `parent-child` edges:
   - SQL `sqlite_master`: `blocked_directly` uses `d.type = 'blocks'` and nonterminal blocker statuses.
   - SQL `sqlite_master`: `blocked_transitively` joins `dependencies` through `type = 'parent-child'`.
   - SQL simulation: `bd-319jg` and `bd-uiveo` both return `0` for membership in that recursive blocked set.

7. `beads_rust::storage::sqlite::get_ready_issues` does not query the `ready_issues` view:
   - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2819` defines `get_ready_issues`.
   - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2824` delegates to `get_ready_issues_with_projection`.
   - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2844` defines `get_ready_issues_with_projection`.
   - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2882` builds SQL with `build_ready_issue_candidates_query`.
   - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2889-2890` selects `FROM issues WHERE 1=1`.

8. The actual ready query filters are:
   - Label-AND filter through `EXISTS (SELECT 1 FROM labels ...)`: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2893-2897`.
   - Label-OR filter through `EXISTS ... label IN (...)`: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2900-2911`.
   - `status = 'open'` unless `include_deferred`: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2913-2919`.
   - blocked cache exclusion when cache is trusted: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2921-2926`.
   - `defer_until IS NULL OR <= now`: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2928-2931`.
   - not pinned: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2933-2935`.
   - not ephemeral and not `%-wisp-%`: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2937-2940`.
   - not template: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2942-2943`.
   - optional issue type, priority, assignee, unassigned, and parent filters: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2945-3004`.

9. Current DB values do not identify a normal readiness filter that would have excluded `bd-319jg` while open:
   - SQL `issues`: `bd-319jg` has priority `0`, issue_type `task`, `defer_until NULL`, `pinned=0`, `ephemeral=0`, `is_template=0`.
   - SQL candidate check ignoring only current closed status: `bd-319jg_if_open_ready_candidates = 1`.
   - SQL candidate check for working task: `bd-uiveo_current_ready_candidates = 1`.

10. The current `blocked_issues_cache` table does not contain `bd-319jg`, `bd-uiveo`, or `bd-1szpx`:
    - SQL: `SELECT * FROM blocked_issues_cache WHERE issue_id IN (...)` returned no rows.

11. But the prior analysis over-weighted fact 10: current DB marks the blocked cache stale:
    - SQL `metadata`: `blocked_cache_state | stale`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2850-2862` says stale cache makes reads compute blocked IDs in memory and avoid the cache table.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:521-527` defines the stale check.
    - Therefore, "not in `blocked_issues_cache`" is not enough evidence for what `list_ready` would do while the stale marker is set.

12. The in-memory blocked computation does not mark an open child blocked merely because its parent epic is open:
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3380-3405` computes direct blockers, propagates blocked parents to children, then adds open-child blockers to parents.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3797-3815` explicitly documents that open children block epics, but open non-epic parents do not block children.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3823-3833` implements epic-only open-child blocking with `p.issue_type = 'epic'`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3910-3937` propagates a parent's blocked state to children only if the parent is already in the blocked map.
    - SQL approximation of the current in-memory algorithm returns blockers for `bd-3b3wr`, not for `bd-uiveo` or `bd-319jg`.

### Create/update semantics

13. `create_issue` is a single `mutate` call:
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1360-1361`.
    - It inserts the issue row at `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1405-1453`.
    - It inserts labels inside the same mutation closure at `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1464-1479`.
    - It inserts dependencies inside the same mutation closure at `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1481-1527`.
    - It marks the issue dirty at `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1553`.

14. `create_issue` does touch blocked-cache state when dependencies are present:
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1526` calls `ctx.invalidate_cache_for(&[issue.id, dep.depends_on_id])`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1315-1318` marks `blocked_cache_state` stale when a blocked-cache refresh plan exists.
    - The earlier statement "create_issue does not touch blocked_issues_cache or marks it stale" is wrong for a new issue created with a parent-child dependency.

15. Plain dependency edits defer blocked-cache rebuilds:
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:4430-4437` routes dependency creation through `add_dependency_with_metadata`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:4547-4552` explicitly defers blocked-cache rebuild and marks stale.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1333-1341` documents that stale reads compute blocked state in memory.

### Reconciler ready path and observability

16. Global reconciler first enumerates plan-complete epics:
    - `crates/spur-mcp/src/plan/reconciler/ready.rs:36-44` calls `pm.list_issues` with label `PLAN_COMPLETE`, `issue_type: Some("epic")`, and limit `10_000`.
    - `crates/spur-pm/src/beads_crate/issue_tracker.rs:370-406` maps that to beads `list_issues`.
    - `crates/spur-pm/src/beads_crate/issue_tracker.rs:399` sets `include_closed = filter.include_closed || filter.status.is_some()`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2401-2406` excludes `closed`, `tombstone`, and `deferred` when `include_closed` is false.

17. Current DB explains why `bd-2rbtl` is not visible to the global epic query now, but not why it was absent during the bug window:
    - SQL current global-epic predicate: `bd-2rbtl_visible_by_global_epic_query_now = 0` because it is closed.
    - SQL current global-epic predicate: `bd-3b3wr_visible_by_global_epic_query_now = 1`.
    - SQL event history shows `bd-2rbtl` was open and plan-complete from `2026-05-18T22:55:19.249748+00:00` until `2026-05-18T23:02:17.555426+00:00`.

18. For each enumerated epic, global reconciler calls `list_ready` scoped only by task plan-id:
    - `crates/spur-mcp/src/plan/reconciler/ready.rs:47-64`.
    - `crates/spur-pm/src/beads_crate/beads_advanced.rs:43-74` converts `ReadyFilter` to beads `ReadyFilters` and calls `s.get_ready_issues`.
    - `crates/spur-pm/src/advanced.rs:27-35` shows `ReadyFilter` only carries assignee, labels, issue_type, priorities, and limit.

19. The silent-skip telemetry gap is real:
    - `crates/spur-mcp/src/plan/reconciler/ready.rs:57-69` only pushes summaries returned by `list_ready`.
    - `crates/spur-mcp/src/plan/reconciler/mod.rs:1043-1103` records skipped outcomes only after `observe_ready_summaries` returns hydrated items.
    - `crates/spur-mcp/src/plan/reconciler/ready.rs:278-280` records `NoReadyTasks` only if the aggregated `summaries` vector is empty.
    - In global mode `self.plan_id` is `None`, so that `NoReadyTasks` call has no plan-id context: `crates/spur-mcp/src/plan/reconciler/ready.rs:18` and `crates/spur-mcp/src/plan/reconciler/ready.rs:278-280`.
    - `crates/spur-mcp/src/plan/outcomes.rs:272-280` stores outcomes under a plan only when a plan id is supplied.
    - `crates/spur-mcp/src/plan/outcomes.rs:406-410` returns an empty vector for plan ids that have no stored buffer.

20. `get_reconciler_status` is only the in-memory outcome store, not a source-of-truth scan of beads:
    - `crates/spur-mcp/src/server/handlers/plan.rs:1054-1066` serializes `self.reconciler_outcomes.lock().await.reconciler_status()`.
    - `crates/spur-mcp/src/plan/outcomes.rs:449-454` builds status from the global outcome snapshot, stuck tasks, and last tick time.
    - `crates/spur-mcp/src/handlers.rs:304-318` shows `get_plan_status` separately loads plan state and then attaches `recent_outcomes`/`stuck_tasks`.

### Connection lifecycle and WAL hypothesis

21. The leading hypothesis's key premise is false for the normal `BeadsCrateAdapter::read` path:
    - `crates/spur-pm/src/beads_crate/adapter.rs:204-213` shows `BeadsCrateAdapter` stores paths/config/metrics/cursor, not a `SqliteStorage` read connection.
    - `crates/spur-pm/src/beads_crate/adapter.rs:309-312` documents "opens a fresh `SqliteStorage` connection for the duration of `f` and drops it on return."
    - `crates/spur-pm/src/beads_crate/adapter.rs:324-336` opens `SqliteStorage::open_with_timeout` inside `spawn_blocking`.
    - `crates/spur-pm/src/beads_crate/adapter.rs:339-354` runs the closure, drops storage, then returns.
    - `crates/spur-pm/src/beads_crate/beads_advanced.rs:43-83` implements `list_ready` by calling that `self.read(...)`.

22. Writes also use fresh storage handles and checkpoint after drop:
    - `crates/spur-pm/src/beads_crate/adapter.rs:476-480` documents single writes under flock with fresh `SqliteStorage`.
    - `crates/spur-pm/src/beads_crate/adapter.rs:491-504` acquires the flock and opens storage.
    - `crates/spur-pm/src/beads_crate/adapter.rs:510-517` drops storage/flock and performs best-effort WAL checkpoint.

23. The repo contains an unused `ReaderPool`, but the adapter does not currently use it:
    - `crates/spur-pm/src/beads_crate/reader_pool.rs` defines `ReaderPool` per `rg`.
    - `rg "ReaderPool|reader_pool"` finds no use from `adapter.rs`.
    - `crates/spur-pm/src/beads_crate/adapter.rs:204-213` confirms no reader-pool field.

24. SQLite mode in a raw `sqlite3` CLI connection is not reliable evidence of the adapter's runtime mode:
    - Raw SQL `PRAGMA journal_mode` returned `delete` for my direct CLI connection.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:574-588` opens the DB and applies runtime pragmas for current schemas.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/schema.rs:461-470` sets `PRAGMA journal_mode = WAL` if the opened connection is not already WAL.
    - Therefore raw CLI mode does not prove the SPUR adapter was not using WAL at runtime.

### Recovery-tool no-op

25. `force_reclaim_plan` only rewrites owner labels on the epic and emits an audit comment:
    - `crates/spur-mcp/src/server/handlers/plan.rs:498-505` finds the epic with `include_closed: true`.
    - `crates/spur-mcp/src/server/handlers/plan.rs:552-563` captures prior owner labels.
    - `crates/spur-mcp/src/server/handlers/plan.rs:565-583` constructs `add_labels`/`remove_labels` for owner labels only.
    - `crates/spur-mcp/src/server/handlers/plan.rs:594` calls `fast_forward_reconciler`.
    - `crates/spur-mcp/src/server/handlers/plan.rs:602-619` emits the plan-force-reclaimed audit comment.

26. Same-owner reclaim is mostly a semantic no-op:
    - `crates/spur-mcp/src/server/sync.rs:335-338` removes any label from `remove_labels` if it is also in `add_labels`.
    - `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:4823-4848` makes `add_label` return `Ok(false)` when the label already exists.
    - SQL event history confirms same-owner reclaim wrote an audit comment but did not change task readiness fields.
    - This explains why force reclaim did not unblock dispatch without needing a WAL-snapshot theory.

## Section 2 — Facts I Could Not Verify

1. I could not verify the exact `get_reconciler_status` dump that contained "107 other plans" and omitted `72e5f4dc-14bc-4345-ad7b-23c003ab3e41`.
   - What is missing: the raw JSON dump with `last_tick_at`, `recent_outcomes`, and `stuck_tasks`.
   - Why it matters: `crates/spur-mcp/src/server/handlers/plan.rs:1054-1066` returns in-memory outcomes, so historic outcomes do not prove that a tick traversed the failing plan after it became plan-complete.

2. I could not verify that multiple reconciler ticks actually executed after `2026-05-18T22:55:19.249748+00:00` and before cleanup.
   - What is missing: logs or status snapshots containing `last_tick_at` before/after force reclaim.
   - Why it matters: `crates/spur-mcp/src/plan/outcomes.rs:268-270` records `last_tick_at`, but that value is not in the facts provided here.

3. I could not verify a historical `list_ready(ReadyFilter { labels_all: [plan-id], limit: 1000 })` result during the seven-minute open window.
   - What is missing: a trace/log from `crates/spur-pm/src/beads_crate/beads_advanced.rs:43-83` during that window, or a preserved DB snapshot from before cleanup.

4. I could not prove whether a transient `fsqlite` visibility/index defect occurred.
   - Current DB passes raw `PRAGMA quick_check` with `ok`.
   - The `.beads` directory contains prior broken/corrupt DB artifacts, but those filenames alone do not prove bd-mp246's cause.

## Section 3 — Alternative Hypotheses Evaluated

### H1 — WAL reader-snapshot pinning on a long-lived `BeadsCrateAdapter::read` connection

Verdict: demoted hard.

Evidence against:

- The adapter does not keep a long-lived SQLite read connection: `crates/spur-pm/src/beads_crate/adapter.rs:204-213`.
- `read` opens fresh storage per call: `crates/spur-pm/src/beads_crate/adapter.rs:309-336`.
- `read` drops storage before returning: `crates/spur-pm/src/beads_crate/adapter.rs:339-354`.
- `list_ready` uses `self.read`: `crates/spur-pm/src/beads_crate/beads_advanced.rs:43-83`.

What remains possible:

- A short-lived read can see a legitimate point-in-time snapshot if it opens before a write commits.
- That cannot explain seven minutes of repeated invisibility unless the reconciler did not make fresh reads, did not tick, or a lower-level SQLite/fsqlite defect made fresh reads stale.

### H2 — Issue-row/label-row race

Verdict: weak.

Evidence:

- Labels are inserted inside `create_issue`'s mutation closure: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1464-1479`.
- The task's plan/task labels were recorded at `2026-05-18T22:55:19.242101+00:00`, before plan-complete at `2026-05-18T22:55:19.249748+00:00`.
- Global reconciliation does not enumerate the epic until it has `PLAN_COMPLETE`: `crates/spur-mcp/src/plan/reconciler/ready.rs:36-44`.

Conclusion:

- A sub-millisecond label race could explain one unlucky read.
- It does not explain repeated ticks after all labels and plan-complete were durable.

### H3 — Hidden ready filter excluded `bd-319jg`

Verdict: not supported by current DB.

Evidence:

- Ready filters are enumerated in `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2893-3004`.
- Current DB values for `bd-319jg` satisfy all non-status gates.
- SQL candidate check ignoring only current cleanup status returns `bd-319jg_if_open_ready_candidates = 1`.
- Event history shows `bd-319jg` was open until `2026-05-18T23:02:16.084427+00:00`.

Weakness:

- This is not a preserved historical `list_ready` call, because the issue is closed now.

### H4 — Cache/table confusion

Verdict: partially confirmed, but not the root cause.

Correct part:

- `ready_issues` is a view and not used by `get_ready_issues`.
- The actual implementation uses `blocked_issues_cache` only when cache state is not stale: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2850-2867`.

Wrong/weak part in prior analysis:

- Current DB has `blocked_cache_state=stale`, so "not in `blocked_issues_cache`" does not prove `get_ready_issues` would include the issue.
- The in-memory blocked computation still does not appear to block `bd-319jg`, but that required reading `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3380-3405` and `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3797-3937`.

### H5 — Epic enumeration failed

Verdict: plausible only if the reconciler did not tick in the open window, or if the live reader saw stale/malformed data.

Evidence for enumeration when open:

- Global query wants plan-complete, epic, non-closed issues: `crates/spur-mcp/src/plan/reconciler/ready.rs:36-44`, `crates/spur-pm/src/beads_crate/issue_tracker.rs:399`, `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:2401-2406`.
- Event history says `bd-2rbtl` was open and plan-complete for about seven minutes.

Evidence now:

- Current SQL says `bd-2rbtl_visible_by_global_epic_query_now = 0` because it is closed.
- Current SQL says the working open plan `bd-3b3wr_visible_by_global_epic_query_now = 1`.

Conclusion:

- The current DB cannot reproduce the epic enumeration failure because cleanup changed the status.
- During the bug window, the persisted facts should have made the epic enumerable.

### H6 — Reconciler liveness/tick path did not actually traverse this plan after submit

Verdict: best remaining explanation, but not proven.

Evidence:

- `get_reconciler_status` is an in-memory outcome buffer, not a fresh DB scan: `crates/spur-mcp/src/server/handlers/plan.rs:1054-1066`.
- Existing outcomes for other plans do not prove a post-submit traversal of this plan, because `OutcomeStore` keeps prior per-plan outcomes: `crates/spur-mcp/src/plan/outcomes.rs:406-421`.
- `force_reclaim_plan` fast-forwards by notifying, but does not synchronously run reconciliation: `crates/spur-mcp/src/server/mod.rs:398-400` and `crates/spur-mcp/src/server/handlers/plan.rs:594`.
- If no tick consumed that notify, or if the loop was stalled elsewhere, same-owner force reclaim would not change ready state.

Weakness:

- The provided facts say "multiple ticks pass"; I could not verify that with raw `last_tick_at` or logs.

### H7 — Lower-level `fsqlite` visibility/index defect

Verdict: possible but unproven.

Evidence for plausibility:

- The code contains special handling/comments for `fsqlite` false constraint/index behavior: `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:42-49`, `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:1658-1665`, `/Volumes/Projects/spur/resource/beads_rust/src/storage/sqlite.rs:3481-3489`.
- The `.beads` directory contains historical `btree-corrupt` and `frankenwal-broken` DB artifacts.

Evidence against:

- Current raw `PRAGMA quick_check` returned `ok`.
- Current direct SQL sees the rows/labels/edges coherently.

## Section 4 — Ranked Best Hypothesis

1. **Telemetry bug: per-plan empty `list_ready` in global mode is invisible. Confidence: 95%.**
   - This is proven by `crates/spur-mcp/src/plan/reconciler/ready.rs:57-69`, `crates/spur-mcp/src/plan/reconciler/ready.rs:278-280`, and `crates/spur-mcp/src/plan/outcomes.rs:272-280`.
   - Falsifiable prediction: add a test where global epic enumeration returns two epics, only one has ready tasks, and the other plan's `list_ready` returns empty. Current code records no per-plan `NoReadyTasks` for the empty plan.

2. **Dispatch root cause: reconciler did not actually execute a fresh ready traversal for this plan during the open window, or the traversal was defeated by a transient lower-level visibility defect. Confidence: 45%.**
   - This is the best remaining explanation because persisted DB state should have satisfied both epic enumeration and task readiness while open.
   - Falsifiable prediction: on a copy of the DB taken before cleanup, a direct call to `observe_ready_summaries` after plan-complete would return `bd-319jg`. If yes, the bug is above the DB query path: tick/liveness/status interpretation. If no, the bug is in beads visibility/filtering.

3. **Long-lived WAL reader-snapshot pinning specifically in `BeadsCrateAdapter::read`. Confidence: 10%.**
   - The normal path opens and drops fresh storage per read: `crates/spur-pm/src/beads_crate/adapter.rs:309-354`.
   - Falsifiable prediction: instrument `BeadsCrateAdapter::read` with a connection id and open/close logs; there should be one open/drop pair for each `list_ready` call, not a single long-lived reader spanning ticks.

## Section 5 — One Discriminating Experiment

Add one test under `crates/spur-mcp/src/plan/reconciler/tests.rs` or a scratch integration test. Keep it under 30 lines by using existing test fakes.

```rust
#[tokio::test]
async fn global_reconciler_records_plan_empty_ready() {
    // Arrange two plan-complete epics: P1 has no ready tasks, P2 has one.
    // Fake PM list_issues returns both epics for PLAN_COMPLETE.
    // Fake advanced list_ready returns [] for P1 and [task] for P2.
    let reconciler = fake_global_reconciler_with_ready(vec![
        ("P1", vec![]),
        ("P2", vec![ready_task("P2", "T2")]),
    ]);

    let ready = reconciler.observe_ready_summaries().await.unwrap();

    assert_eq!(ready.len(), 1);
    let outcomes = reconciler.outcomes.lock().await.recent_outcomes("P1");
    assert!(outcomes.iter().any(|o| matches!(
        o,
        DispatchOutcome::NoReadyTasks { plan_id, .. } if plan_id == "P1"
    )));
}
```

Why this discriminates:

- If this test fails today, the silent-skip telemetry bug is real and should be fixed by recording a plan-scoped no-ready result inside the per-epic loop in `crates/spur-mcp/src/plan/reconciler/ready.rs:47-70`.
- Then run the same fake with `list_ready(P1)` returning `bd-319jg`; if dispatch proceeds, the remaining production failure is liveness/DB visibility, not dispatch gating.
- This test does not prove the historical DB root cause, but it prevents the exact "plan absent from status" failure mode from hiding the next occurrence.

## Section 6 — Blunt-Honest Assessment

The bd-mp246 analysis overreaches on WAL snapshot pinning. The specific claim that `BeadsCrateAdapter::read` has a long-lived reader connection is wrong in current code: the adapter has no storage field and opens/drops a fresh `SqliteStorage` per read. The analysis also treated `blocked_issues_cache` as decisive while the DB is marked `blocked_cache_state=stale`, which makes `get_ready_issues` compute blockers in memory instead of trusting the table. The telemetry-gap analysis is solid, but it explains why the plan was absent from `recent_outcomes`; it does not by itself prove why the task failed to dispatch. The strongest honest statement is: the observability bug is proven; the dispatch root cause is still not proven; the current WAL-reader hypothesis is too specific and is contradicted by the adapter lifecycle.
