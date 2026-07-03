# MCP Response Audit Findings

Date: 2026-07-03

## Method

The requested plan file, `docs/superpowers/plans/2026-07-03-lean-mcp-response-metadata.md`, is not present in this worktree or visible in `git log --all -- docs/superpowers/plans/2026-07-03-lean-mcp-response-metadata.md`. This audit follows the explicit candidate list from the task prompt.

Live calls for these exact MCP tools were not available in this worker context, so each measurement uses a source-grounded fixture matching the real serialized field shape. Fixtures were written under `/private/tmp/mcp-response-audit` as `*.full.json` and `*.trim.json`.

Token impact was measured with:

```bash
npx -y @toon-format/cli -e --stats -o /dev/null <file>.json
```

Only the `~N (JSON)` estimate was used.

`handle_get_loop_status` and `handle_submit_plan` were not re-audited.

## get_plan_status

Entry points read:

- `crates/spur-core/src/handlers.rs:145-174`: `get_plan_status` parses `plan_id`, loads the plan, delegates to `crate::plan::build_plan_status`, then appends `plan_state_freshness`, `recent_outcomes`, and `stuck_tasks`.
- `crates/spur-core/src/plan/mod.rs:3500-3801`: `build_plan_status` builds the top-level status, counts, merge state, next action, and `tasks[]`.
- `crates/spur-core/src/handlers.rs:105-124`: `PlanStateFreshness::to_json` adds cache/projection freshness metadata.
- `crates/spur-core/src/plan/mod.rs:1035`: `MAX_ATTEMPTS` is `3`.

Actual response shape observed from source:

- Top level: `plan_id`, `status`, `progress`, `counts`, `all_workers_done`, `ready_to_merge`, `next_action`, `merge`, `tasks`, plus appended `plan_state_freshness`, `recent_outcomes`, `stuck_tasks`.
- Each task starts with `task_id`, `task_name`, `agent`, `attempt`, `max_attempts`, `history_count`, then status-specific fields such as `delegation_id`, `remaining_attempts`, `summary`, `worker_branch`, `artifact`, `feedback`, `blocked_by`, or failure/escalation fields.

Redundant fields found:

- `progress` duplicates values already present in `counts`.
- `tasks[].max_attempts` repeats the same constant on every task.
- `tasks[].history_count: 0` is common first-attempt padding; keep only when non-zero in a compact response.
- `recent_outcomes: []` and `stuck_tasks: []` are common empty appenders; keep only when non-empty.

No redundancy counted:

- `plan_state_freshness` is not duplicated elsewhere in the payload.
- `merge.base_snapshot_branch` is useful merge context, not a duplicate of the request.

Measured token delta:

- Full fixture: `~514 (JSON)`
- Trimmed fixture: `~421 (JSON)`
- Delta: `~93 tokens`, about `18.1%`

Verdict: worth a `response_format` task. The measured savings are moderate on a 4-task fixture but scale with task count because `max_attempts` and zero/default row fields repeat per task.

## list_issues

Entry points read:

- `crates/spur-pm/src/mcp/mod.rs:68-94`: `PmMcpModule::call` routes `list_issues` through `text_json(list_issues(...))`.
- `crates/spur-pm/src/mcp/mod.rs:317-364`: `list_issues` builds an `IssueFilter`, calls `PmService::list_issues`, and serializes `Vec<IssueSummary>`.
- `crates/spur-pm/src/mcp/mod.rs:470-515`: `list_issues_def` exposes only filter arguments; there is no response-format option today.
- `crates/spur-pm/src/types.rs:54-69`: `IssueSummary` serializes `id`, `source`, `title`, `status`, `labels`, `url`, plus optional `priority`, `issue_type`, `assignee`, and `description`.
- `crates/spur-pm/src/beads_crate/issue_tracker.rs:213-227`: the beads adapter sets `source: Beads` and `url: beads://<id>` for every beads issue.

Actual response shape observed from source:

- The payload is a bare JSON array of issue summary objects.
- In the common beads-backed SPUR path, each row includes `source: "beads"`, a `beads://...` URL, `priority`, and `issue_type`; optional fields are skipped when absent rather than serialized as `null`.

Redundant fields found:

- `source: "beads"` is the same value on every row for the beads backend.
- `url: "beads://<id>"` mechanically duplicates `id` with a fixed scheme in the beads backend.
- `labels: []` is common empty-array padding for unlabeled issues.

No redundancy counted:

- `status` may repeat under a status filter, but it is not always redundant for unfiltered lists.
- Optional `priority`, `issue_type`, `assignee`, and `description` are either real data or omitted by serde.

Measured token delta:

- Full fixture: `~196 (JSON)`
- Trimmed fixture: `~146 (JSON)`
- Delta: `~50 tokens`, about `25.5%`

Verdict: worth a low-priority `response_format` task if `list_issues` is commonly called with larger limits. The per-row savings are small, but `source` and beads URL duplication scale linearly with the result count.

## graph_plan, graph_insights, graph_alerts

Entry points read:

- `crates/spur-pm/src/mcp/mod.rs:234-264`: handlers call `analyzer.plan`, `analyzer.insights`, and `analyzer.alerts`, then return `pretty_raw_content(report.raw)`.
- `crates/spur-pm/src/mcp/mod.rs:609-643`: tool definitions for `graph_plan`, `graph_insights`, and `graph_alerts`.
- `crates/spur-pm/src/graph_engine/mod.rs:555-572`: the graph engine fills `report.raw` with the raw serializers.
- `crates/spur-pm/src/graph_engine/raw.rs:8-18`: `serialize_plan`, `serialize_insights`, and `serialize_alerts` serialize the typed report after `raw` is skipped.
- `crates/spur-pm/src/graph_engine/plan.rs:8-115`: `compute_plan` produces tracks, items, summary, and a static `usage_hints` string.
- `crates/spur-pm/src/graph_engine/insights.rs:28-128`: `compute_insights` produces Go-compatible capitalized buckets plus `top_what_ifs`.
- `crates/spur-pm/src/graph_engine/mod.rs:191-325`: `compute_alerts` produces alerts, summary counts, and static `usage_hints`.

Actual response shape observed from source:

- `graph_plan`: `generated_at`, `data_hash`, `plan.tracks[].items[]`, `plan.total_actionable`, `plan.total_blocked`, `plan.summary`, and `usage_hints`.
- `graph_insights`: `generated_at`, `data_hash`, capitalized Go-compatible arrays (`Bottlenecks`, `Keystones`, `Influencers`, `Hubs`, `Authorities`, `Cores`, `Articulation`, `Orphans`, `Cycles`), `ClusterDensity`, and `top_what_ifs`.
- `graph_alerts`: `generated_at`, `data_hash`, `alerts[]`, `summary`, and `usage_hints`.

Redundant fields found:

- `graph_plan.usage_hints` and `graph_alerts.usage_hints` are static command examples, not result data.
- `graph_plan.plan.tracks[].items[].unblocks: null` is null padding for leaf items; non-empty `unblocks` arrays should stay.
- Empty insight buckets in the captured fixture (`Keystones`, `Hubs`, `Authorities`, `Cores`, `Articulation`, `Cycles`) are default empty arrays; a compact response can omit empty buckets.
- `graph_insights.top_what_ifs[].delta.estimated_days_saved: null` is currently always `None` in `compute_top_what_ifs`.
- `graph_alerts.alerts[]` serializes default `label: null`, `details: []`, `baseline_value: null`, `current_value: null`, and `delta: null` on alert types that do not populate those fields. `issue_ids: []` is also empty padding for single-issue alerts.

No redundancy counted:

- `generated_at` and `data_hash` are response metadata, but they are not duplicated inside a single payload.
- `graph_alerts.summary` duplicates information derivable from `alerts[]`, but it is a useful aggregate and should stay in full and compact responses unless a separate `summary=false` option exists.

Measured token delta:

- Full fixture: `~788 (JSON)`
- Trimmed fixture: `~608 (JSON)`
- Delta: `~180 tokens`, about `22.8%`

Verdict: worth a `response_format` task, especially for `graph_alerts`. The static hints and default null/empty alert fields are source-grounded redundancy, and savings grow with alert count.

## external_code_callers and external_code_callees

Entry points read:

- `crates/spur-core/src/mcp/context_service.rs:204-258`: core MCP schema definitions for `external_code_callers` and `external_code_callees`.
- `crates/spur-context-service/src/mcp.rs:176-194`: `handle_tool_sync` dispatches the external code tools.
- `crates/spur-context-service/src/mcp.rs:265-296`: `handle_code_callers` and `handle_code_callees` parse args, normalize the selector, call `query::find_callers` / `query::find_callees`, and serialize the result.
- `crates/spur-context-service/src/mcp.rs:1329-1383`: context-service tool definitions mirror the core schemas.
- `crates/spur-context-service/src/query.rs:102-183`: `CodeCandidate`, `EdgeMetadata`, `CountsByKind`, `CallerRecord`, `CalleeRecord`, `CallerResult`, and `CalleeResult`.
- `crates/spur-context-service/src/query.rs:357-421`: `find_callers` and `find_callees` build the result.
- `crates/spur-context-service/src/query.rs:1034-1091`: `code_candidate_from_row` and `edge_metadata_from_row` construct the repeated row objects.

Actual response shape observed from source:

- `external_code_callers`: `{ callers: [{ caller, edge, resolved }], counts_by_kind, unresolved_sample }`
- `external_code_callees`: `{ callees: [{ callee, edge, resolved }], counts_by_kind, unresolved_sample }`
- `caller` / `callee` candidates include selector identity, URI, package identity, symbol names, file path, range, kind, and optional enclosing scope.
- `edge` includes source/target stable IDs, optional unresolved label/package, relation, edge kind, optional confidence metadata, bind method, receiver text, and scope text.

Redundant fields found:

- `CodeCandidate.id` duplicates `CodeCandidate.stable_symbol_id` exactly.
- `CodeCandidate.uri`, `source`, `package`, and `revision` duplicate identity already encoded in `selector` and `stable_symbol_id` for compact use.
- `CodeCandidate.enclosing_scope: null` is null padding for top-level symbols.
- `EdgeMetadata.relation: "calls"` is fixed by the SQL `WHERE e.relation = 'calls'` in both tools.
- `EdgeMetadata.target_label`, `target_package`, `confidence`, `confidence_score`, `receiver_text`, and `scope_text` are null-padded on ordinary resolved rows; `bind_method` is null on unresolved rows.
- `resolved: true` is redundant in the default `include_unresolved=false` path because only resolved rows are returned. A compact response can omit true and keep `resolved: false` when unresolved rows are explicitly included.
- `counts_by_kind.references_other` is always zero for these queries because the SQL filters `edge_kind IN ('calls', 'calls_dyn', 'references_hof')`. Other zero count keys and `unresolved_sample: []` are common default padding.

No redundancy counted:

- `edge.source_stable_id` and `edge.target_stable_id` are real edge identity, not duplicates of the row candidate in all directions.
- `target_label` must stay when unresolved rows are included and populated.

Measured token delta:

- Full fixture: `~1057 (JSON)`
- Trimmed fixture: `~610 (JSON)`
- Delta: `~447 tokens`, about `42.3%`

Verdict: worth a `response_format` task. This is the strongest candidate: duplicated candidate identity and null-heavy edge metadata dominate the response, and the savings are large even on a two-row callers plus two-row callees fixture.
