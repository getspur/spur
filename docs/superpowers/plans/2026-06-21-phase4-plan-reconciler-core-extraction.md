# Phase 4 (Plan/Review/Reconciler) — Core Orchestration Extraction Plan

- **Status:** In progress — Stage 0 landed; Stages 1–8 remaining
- **Date:** 2026-06-21
- **Spec:** `docs/superpowers/specs/2026-06-13-mcp-crate-ownership-refactor-design.md` §8 Phase 4, §6 Worker MCP
- **Scope:** Move the plan/review/reconciler MCP tools **plus** the plan/reconciler engine
  (`crates/spur-mcp/src/plan/*`, `server/handlers/{plan,plan_execute}.rs`) **plus** the
  orchestration runtime state currently held on `McpCallbackServer` into `crates/spur-core`.
- **Precedent:** `refactor(spur-core): bd-119pt own delegation mcp tools` (commit `4dfc2fbce`)
  established the module/deps-bundle pattern this plan extends to plans.

## 0. Why this is one large coordinated move, not several small slices

Before writing this plan I mapped the actual cross-references. The conclusion is that the
plan/review/reconciler surface is an **irreducible unit** w.r.t. the hard dependency rule
`spur-mcp ↛ spur-core` (§2). The evidence:

1. **`plan/* ↔ server/*` are bidirectionally coupled.** 13 files under
   `crates/spur-mcp/src/plan/` reach into `crate::server::` (for `require_feature`,
   `feature_error_message`, `pro_feature_gate`, `run_git_capture`,
   `DetachedContinuationCtx`, `ORPHAN_CLEAR_REASON_RESTART`), and `server/mod.rs` +
   every `server/handlers/*` file references `crate::plan::*` pervasively.

2. **The plan handlers are methods on `McpCallbackServer`.** `handle_merge_plan`,
   `handle_submit_plan`, `handle_review_task`, … are `self.*` methods that read the
   orchestration state fields (`active_plans`, `plan_registry`, `reconciler_outcomes`,
   `active_plan_claim_lock`, …) and call other `self.*` engine methods
   (`load_or_project_plan`, `install_projected_plan`, `beads_version_for_epic`,
   `check_plan_owner_for_op`, `enable_reconciler`). They cannot be re-homed onto an
   Arc-handle deps bundle the way the (already-extracted) delegation handlers were,
   because they depend on the whole engine, not a handful of channels.

3. **The dispatch registry is coupled to `McpCallbackServer`.**
   `spur_mcp::registry::ToolCallContext` carries `callback_server: Option<&McpCallbackServer>`
   and `LegacyMcpToolModule`/`BrainPmMcpToolModule`/`GraphMcpToolModule`/`AnalystMcpToolModule`
   all dispatch via `ctx.callback_server()?.handle_registered_tool_call(...)`. If
   `McpCallbackServer` moves to `spur-core`, this whole composition glue must move with it
   and the generic `ToolCallContext` must drop its `callback_server` field.

4. **~40 spur-mcp integration tests exercise the plan tools through the default registry.**
   `crates/spur-mcp/tests/` (≈70 files) includes `tool_catalog.rs` (snapshots `tools_list()`
   including `submit_plan`/`execute_epic`/`merge_plan`/`review_task`/…), plus
   `submit_plan_*`, `mutation_*`, `plan_*`, `reconciler_*`, `review_task_nonadvisory_writes`,
   `preview_task_base`, `merge_plan_restart_recovery`, `epic_completion`, `g_strict_e2e`, …
   These tests construct `McpCallbackServer` and call plan tools via
   `handle_tool_call` (the **default** registry). They **cannot** compose a `spur-core`
   `PlanMcpModule` (that would be the forbidden `spur-mcp → spur-core` edge), so the moment
   plan-tool dispatch leaves spur-mcp's default registry, this entire test suite must move to
   `spur-core` **together with** the engine — 1:1, no deletions (the prior monolithic attempt
   failed precisely by deleting/weakening these tests).

**Therefore:** the catalog (tool definitions + dispatch), the engine (`plan/*`,
`server/handlers/{plan,plan_execute}.rs`, `server/{review,recovery,sync,plan_builder}.rs`),
the state fields, the worker-side read tools, and the ~40 integration tests are **one move**.
Splitting "just the catalog" or "just the engine" is impossible without either creating the
forbidden dependency edge or weakening tests. This is exactly why the task brief calls this
"the architectural CORE … and the LARGEST task."

## 1. Target architecture

`spur-mcp` keeps only infrastructure (per spec §4):
`ToolDefinition`, `ToolModule`, `ToolRegistry`/`ToolRegistryBuilder`, `ToolCallContext`
(without the `callback_server` field), `JsonRpcResponse` + RMCP error/result mapping,
`token.rs`, the feature-gate helpers (`require_feature`, `feature_error_message`,
`pro_feature_gate`, `community_feature_gate`), the git helper (`run_git_capture`), and the
streamable-HTTP/stdio server-builder transport.

`spur-core` gains a real orchestration MCP surface under `crates/spur-core/src/mcp/` and the
relocated engine:

- `spur-core/src/plan/**` ← `spur-mcp/src/plan/**` (engine: projector, reconciler, mutation,
  audit_sentinel, signal_watcher, outcomes, proposers, staging, preview, ownership, labels, …).
- `spur-core/src/mcp/plan.rs` — `PlanMcpModule` (`ToolModule`) advertising the 13 tools and
  `PlanMcpDeps` (the orchestration state bundle).
- `spur-core/src/mcp/orchestrator_server.rs` (or extend `McpCallbackServer` here) — the
  brain/orchestrator server that holds the orchestration state and is built on top of
  `spur-mcp`'s transport.

The brain registry is composed in `spur-core` (already true today for delegation/signals via
`spur_core::mcp::brain_tool_registry`); it gains `.with(PlanMcpModule::new(plan_deps))`.

## 2. `McpCallbackServer` field ownership (Phase-0 acceptance — documented in code at Stage 0)

Infrastructure (STAYS in spur-mcp transport server):
`brain_session_id*`, `task_tracker`, `feature_gate` (license dep), `retiring`, `cancel_token`,
`root_handle`, `root_shutdown_tx`, `tool_registry`, `inline_wait`, `event_sink`.

Orchestration-domain (MOVES to spur-core):
`delegation_tx`, `workers`, `active_delegations`, `completed_delegations`, `pm_service`,
`pm_service_like`, `active_plans`★, `reconciler_outcomes`★, `plan_registry`★,
`active_plan_claim_lock`★, `cancellation_control`, `continuation_ctx`, `materializer`,
`outcome_store`, `version_churn_epic_for_test`, `reconciler_handle`★, `reconciler_enabled`★,
`reconciler_fast_forward`★, `startup_recovery`★, `awaiting_review_rediscovery_started`★,
`repo_root`, `auto_merge_approved_plans`★, `plan_pending_grace`★, `versioned_cache_serve`★,
`nonadvisory_review_writes`★, `dispatch_lease_duration`★. (`graph_mcp_deps` is graph-owned and
already an explicit bundle.) ★ = plan/reconciler-specific.

The 13 tools: `submit_plan`, `execute_epic`, `merge_plan`, `resume_plan`,
`force_reclaim_plan`, `get_plan_status`, `get_reconciler_status`, `get_task_diff`,
`preview_task_base`, `review_task`, `submit_plan_mutation`, `plan_truncate_and_restart`,
`recover_orphaned_dispatch`. Note `get_plan_status` + `get_task_diff` are **also worker
read tools** (not in `WORKER_DENIED_TOOL_CALLS`) served by `worker_server.rs`, so the
worker-MCP composition (spec §6) is entangled and must be handled in the same move.

## 3. Staged sequence (each stage compiles + full `spur-mcp` and `spur-core` test tails green)

> Build/test exclusively via `scripts/spur-cargo` (remote-default). Move every test 1:1 —
> **never delete or weaken a test to make a stage compile.** When a test must change crates,
> it moves verbatim; only its construction of the registry/server is rewired.

- **Stage 0 (LANDED in this change):** Phase-0 scaffolding only — no behavior, dispatch, or
  test-ownership changes. Document the field ownership in code; expose the orchestration-state
  accessors on `McpCallbackServer` that `PlanMcpDeps::from_server` needs; introduce
  `spur-core/src/mcp/plan.rs` with `PlanMcpDeps` + `from_server` + a unit test that proves the
  extraction surface is sufficient. Makes `CachedPlan` `pub`. Purely additive.

- **Stage 1 — Decouple `plan/*` from `crate::server::`.** Relocate the small shared helpers so
  the engine references stable infra paths: keep the feature-gate + git helpers in spur-mcp as
  infra (`spur_mcp::feature::*`, `spur_mcp::git::run_git_capture`) re-exported from their
  current locations; move `DetachedContinuationCtx` + `ORPHAN_CLEAR_REASON_RESTART` into a
  neutral module that will travel with the engine. After this stage `plan/*` references only
  `spur_mcp::{feature,git}::*` and crate-local items — no `crate::server::` engine coupling.
  (Intra-crate; fully green.)

- **Stage 2 — Lift the orchestration state off `McpCallbackServer` behind `PlanMcpDeps`.**
  Convert the plan engine methods (`load_or_project_plan*`, `install_projected_plan*`,
  `beads_version_for_epic`, `derive_beads_version`, `check_plan_owner_for_op`,
  `current_brain_active_owned_plan`, …) and the `PlanResolver`/`ReconcilerAutomation` impls
  to operate on a `PlanMcpDeps`-shaped receiver rather than `&McpCallbackServer`. Still in
  spur-mcp; `McpCallbackServer` delegates to it. (Green; pure internal refactor.)

- **Stage 3 — Relocate the engine to `spur-core`.** `git mv` `spur-mcp/src/plan/**` →
  `spur-core/src/plan/**`; flip `crate::plan` → `crate::plan` (same crate now) and
  `crate::server::{feature,git}` → `spur_mcp::{feature,git}`. Move
  `outcome_materializer.rs`, `submit_plan_dedup.rs`, the orchestration handlers
  (`server/handlers/{plan,plan_execute}.rs`, `server/{review,recovery,sync,plan_builder}.rs`),
  and the domain tool defs/schemas. spur-core's existing `mcp/delegation.rs` + `mcp/signals.rs`
  imports of `spur_mcp::server::{DetachedContinuationCtx, WorkerInfo, …}` /
  `spur_mcp::outcome_materializer` / `spur_mcp::handlers` flip to `crate::*`.

- **Stage 4 — Move `PlanMcpModule` dispatch + the 13 tool definitions into `spur-core`.**
  Compose `PlanMcpModule` in `spur_core::mcp::brain_tool_registry`; remove the plan defs from
  spur-mcp's `tools.rs` legacy lists and the `legacy_*` registry builders.

- **Stage 5 — Worker MCP composition (spec §6).** Re-express the worker registry's
  `get_plan_status` + `get_task_diff` + `fetch_outcome_artifact` read tools as a spur-core
  `worker_module` composed by the orchestrator, preserving the exact worker authority,
  denial-list, audit aggregation, and feature gates. Move `worker_server.rs`'s plan-status /
  signal handling accordingly (signals already live in `spur-core/src/mcp/signals.rs`).

- **Stage 6 — Slim `McpCallbackServer` to infra / relocate it to spur-core.** Drop the
  `callback_server` field from `ToolCallContext`; move the brain/orchestrator server
  (axum + rmcp `ServerHandler`, `enable_reconciler`, `start`, recovery spawns) into spur-core
  on top of spur-mcp's transport builder. spur-mcp's `server/mod.rs` retains only the generic
  transport helpers.

- **Stage 7 — Migrate the ~40 integration tests 1:1.** Move `crates/spur-mcp/tests/{submit_plan_*,
  mutation_*, plan_*, reconciler_*, review_task_nonadvisory_writes, preview_task_base,
  merge_plan_restart_recovery, get_task_diff_*, epic_completion, g_strict_*, startup_recovery,
  pending_sweep, tool_catalog (plan rows), worker_mcp_*}` into `crates/spur-core/tests/`,
  rewiring only server/registry construction. Keep `spur-mcp/tests/tool_catalog.rs` for the
  infra-owned tools; add the plan rows to a spur-core catalog test.

- **Stage 8 — Dependency cleanup (spec §6/§12).** Confirm `spur-mcp` no longer references any
  plan/reconciler/delegation domain type; tighten `lib.rs` re-exports to infrastructure only;
  add/extend the dependency-direction guard test.

## 4. Invariants to preserve (do not regress)

- Worker authority: `WORKER_DENIED_TOOL_CALLS` denials must hold in both `tools/list` and
  `tools/call` (spec §6, §10 "Worker Authority Regression"). Keep the
  `worker_registry_denies_brain_only_tool_calls_with_authorization_error` coverage.
- Response-shape stability: `content` formatting, JSON-RPC error codes, and the `code_search`
  alias must stay byte-stable (spec §10 "Response Shape Drift"). `tool_catalog.rs` /
  `tool_schema_stability.rs` snapshots gate this.
- Reconciler/projection correctness: versioned-cache token derivation, ownership classification,
  lease durations, and audit-sentinel projection are correctness-critical for the live engine —
  move verbatim; do not "simplify."
- Beads is the single source of truth; ephemeral `reconciler_outcomes`/`active_plans` must never
  be persisted.

## 5. Stage 0 deliverable in this change

See commit. Adds (additive, zero behavior change, no test moved/weakened):
- `crates/spur-mcp/src/server/mod.rs`: section the struct fields by target owner (code doc),
  make `CachedPlan` `pub`, add `active_plans_handle`, `plan_registry_handle`,
  `plan_claim_lock_handle`, `pm_service_handle`, `pm_like_handle`, and the plan/reconciler
  config getters mirroring the existing delegation accessors.
- `crates/spur-core/src/mcp/plan.rs`: `PlanMcpDeps` + `PlanMcpDeps::from_server` + a unit test
  asserting the bundle captures the orchestration handles (the concrete input to Stage 2).
