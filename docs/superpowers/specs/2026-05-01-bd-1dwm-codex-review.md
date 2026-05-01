# bd-1dwm — Implementation Review (codex)

## Verdict
Accept-with-changes: G-strict is the right correctness model, but BaseSpec must ship additively first and overlay conflicts must route as plan signals, not plain terminal failures.

## Touch-point inventory
- `crates/spur-mcp/src/tool_schemas.rs:11-24`, `28-45` (risky): `delegate_to_worker` / `delegate_parallel` schemas use `deny_unknown_fields`; adding required `base` breaks every existing caller.
- `crates/spur-mcp/src/tools.rs:17-43` (medium): `DelegationRequest` needs `base`, plus auditable overlay/dispatched-base metadata.
- `crates/spur-mcp/src/server.rs:496-554`, `2493-2533`, `2672-2725` (medium): ad-hoc and parallel handlers must parse, normalize, and forward BaseSpec.
- `crates/spur-mcp/src/plan/mod.rs:1740-1807` and `crates/spur-mcp/src/plan/reconciler.rs:625-656` (risky): ephemeral and persisted plan dispatch must compute approved dependency closure, topological overlay order, and the post-overlay `dispatched_base_oid`.
- `crates/spur-core/src/orchestrator.rs:5219-5331`, `6263-6318` (risky): `WorkerAttemptCtx` / `run_one_worker_attempt` must apply BaseSpec before agent init; add structured `OverlayConflict`.
- `crates/spur-worktree/src/manager.rs:117-190`, `193-248`, `309-317` (risky): existing snapshot/create assumes `HEAD` plus temporary snapshot branch. BaseSpec needs either parameterized base creation or a new helper; do not delete plan base branches as temp snapshots.
- `crates/spur-mcp/src/plan/mod.rs:92-125`, `crates/spur-mcp/src/plan/audit_sentinel.rs:35-50`, `92-106`, `crates/spur-mcp/src/plan/projector.rs:52-75`, `449-463` (medium): persist `dispatched_base_oid` on current and historical attempts, and in completion audit, or restart/projection loses it.
- `crates/spur-mcp/src/server.rs:1501-1594`, `3381-3470` (trivial): `merge_plan` can stay topo cherry-pick by worker tip; it does not need the per-task base.
- `crates/spur-mcp/src/server.rs:1188-1272`, `crates/spur-mcp/src/plan/signals.rs:15-28`, `crates/spur-mcp/src/tools.rs:624-648`, `crates/spur-mcp/src/plan/signal_watcher.rs:89-110` (risky): D/conflicts need new signal variants and watcher behavior; today only `scope_drift` exists.
- `crates/spur-acp/src/domain/peer_message.rs:10-24`, `crates/spur-core/src/orchestrator.rs:6417-6465` (medium): peer mailbox injection works after overlays, but the envelope carries no base provenance.
- `crates/spur-core/src/lineage/adapter.rs:123-190`, `crates/spur-core/src/orchestrator.rs:6291-6297` (medium): lineage only records task/issue/delegation today; base/overlay metadata would be invisible unless added to events.
- `crates/spur-worktree/src/manager.rs:201`, `252-286`, `crates/spur-core/src/worktree_authority.rs:112-124`, `248-264` (medium): current dispatch uses legacy worker branch names while authority GC only owns v2 branches.

## Backwards-compat strategy
Make `base` optional in tool schemas and `DelegationRequest` first. Normalize omitted base to explicit legacy `RepoMain`, emit a warning/telemetry, and require plan-engine callers to always send `WithOverlay`. After clients update, flip strict validation for new protocol versions. Hard-required BaseSpec on day one will break old brains because unknown/missing fields are rejected at the MCP boundary.

## Cherry-pick-in-worktree concerns
Order is: resolve/create worktree from BaseSpec base, apply overlays in the worker worktree, record `HEAD` as `dispatched_base_oid`, then initialize the agent. A brand-new worktree is a normal checkout, so cherry-pick is fine. The race is only self-inflicted: do not call `initialize` or `new_session` before overlay commands finish. Also, `snapshot_brain_state()` cannot remain the only base primitive once `Branch(plan.base_snapshot)` and pinned commits exist.

## Lineage adapter interaction
G-strict worsens the documented retry blind spot: retries already lose constraint-augmented task text at the legacy adapter boundary. If BaseSpec lives only in the retry prompt/request object, lineage cannot explain which overlay a retry saw. Add base/overlay fields to `DelegationRequested` or a dedicated executor event, and record `dispatched_base_oid` per attempt.

## Conflict-routing flow
1. Orchestrator detects overlay cherry-pick conflict before `WorkerSpawned` and returns structured `OverlayConflict { dep_task_id, files }`.
2. Plan reconciler recognizes that shape, writes `signal:integration-conflict` plus sentinel/audit to the task issue, and leaves the issue open; do not let `CompletionState::Failed` close it.
3. Plan status/UI needs a visible blocked/setup-conflict state with files and dependency id; current failed status is too lossy.
4. Brain sees a continuation/re-prompt plus the signal, then decides retry, replan, or manual integration.

## Hidden landmines I found
- `get_task_diff` reconstructs persisted diffs as `base_snapshot..worker_branch` (`crates/spur-mcp/src/server.rs:4429-4446`); with overlays, that includes dependency commits. Use `dispatched_base_oid..worker_branch` for task review diffs.
- Worker branches can contain overlay commits plus worker commits; `merge_plan` cherry-picks only the branch tip. That is acceptable only if cleanup preserves worker output as a single final commit, and should be asserted.
- Clobber detector D cannot be implemented as a worker signal without extending the signal enum/tool schema; today the worker-facing tool cannot emit `potential_clobber`.

## My counter-proposal (if any)
Keep G-strict, but implement it through a first-class `BaseSpec` on `DelegationRequest`, optional-at-wire during migration, and define `dispatched_base_oid` as the post-overlay HEAD immediately before the worker starts. Use that base for review diffs and downstream closure ranges; keep `merge_plan` topo integration unchanged.
