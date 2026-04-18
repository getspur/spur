# submit_plan Epic Persistence — Design Spec

**Status:** In implementation (see plans/2026-04-18-submit-plan-epic-persistence.md)
**Author:** Brain-as-orchestrator MCTS rounds (iceberg-refined)
**Supersedes:** "ingest_plan standalone tool" proposal (rejected)

## Problem

After T1 restored truthfulness to the MCP surface, the remaining friction for brain-as-orchestrator was: how does a brain run a multi-task DAG with optional durability, without re-authoring the plan against beads via N+M primitive calls?

Pre-spec options:
1. Add a standalone `ingest_plan` MCP tool. Rejected: tool-catalog accretion against T1's contraction ethos.
2. Add a `.claude/skills/...` file. Rejected: brains aren't Claude Code main-session users; skills are invisible at the MCP layer.
3. Extend `submit_plan` with optional persistence. **Accepted.**

## Decision

Add three optional fields to `submit_plan`:
- `persist_as_epic: bool` (default `false`).
- `epic_title: String` (required when persist is true).
- `epic_body: String?` (optional free-form description).

When `persist_as_epic=true`:
- Handler verifies beads backend is configured; rejects other backends with `-32000`.
- Handler composes `PmService::create_issue` calls to build an epic + children + deps subgraph.
- Each child gets labels: `spur.plan_id=<plan_id>`, `spur.plan_task_id=<task_id>`, `spur.agent=<agent>` (+ `spur.source_issue=<id>` when `PlanTask.issue_id` is set).
- `PlanState.epic_id` records the beads epic ID; response echoes `epic_id` + `task_map`.

Atomicity is **best-effort**: beads CLI has no transaction primitive. Partial state on mid-creation failure is accepted for v1. The handler surfaces the error; cleanup is the brain/human's responsibility.

## Invariants

- **INV-9** (`submit_plan` non-destructive to catalog): adding persistence fields does not change the tool name or remove any existing field. Snapshot test in `tool_catalog.rs` stays unchanged.
- **INV-10** (persist requires beads): when `persist_as_epic=true` the handler rejects non-beads backends before any issue is created. No partial persistence on wrong backend.
- **INV-11** (topological ordering): children are created in a valid topological order so each child's `depends_on` references only already-created beads IDs.
- **INV-12** (label correlation): every child created via persist carries `spur.plan_id=<plan_id>`. review_task auto-close relies on this label.

## Non-invariants (explicit deferrals)

- **Atomic rollback on mid-creation failure.** Beads CLI composition only. Real transactionality would require a sqlite-backed adapter refactor — tracked separately.
- **Plan.md ↔ beads drift detection.** `epic_body` can link to the plan file; no runtime check that the file hasn't been edited.
- **Auto-sync back from beads to `submit_plan` state.** If a human edits a child issue's body in beads mid-execution, the orchestrator does not re-read. Deferred.
- **Standalone `ingest_plan` (author without dispatch).** Accepted non-goal per the iceberg-refined proposal. Revisit if human-gated authoring workflows become a demand.

## Testing

- Schema shape tests in `tests/submit_plan_schema.rs`.
- Pure-helper tests in `tests/submit_plan_persist.rs` exercising `plan_epic_issue_creates` against the expected `IssueCreate` shapes. Topological-order + cycle-detection guards.
- Existing `tool_catalog.rs` snapshot stays green (name unchanged).

## Out-of-scope for v1

- Brain-prompt surface auto-injection. The guide lives in `docs/spur/brain-orchestration-guide.md` as a human-readable reference. Automatic inclusion in each brain's system prompt is deferred pending the T1.6-T0 investigation of brain-prompt locations.
- TUI visualization of persisted epics. Beads CLI is the canonical UI for now.
- Multi-backend `ingest_plan` (Linear, Plane). Beads-only per existing `add_dependency` pattern.
