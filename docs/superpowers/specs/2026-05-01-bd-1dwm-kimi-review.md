# bd-1dwm — API Review (kimi)

## Go/no-go

Ship G-strict + D for plan tasks immediately; defer brain-explicit-base for ad-hoc dispatches. The minimum addition before merge is a dry-run conflict preview so the brain can see stuck-at-dispatch before it happens, not after.

## Did MCTS over-correct from A+D → G-strict?

No — but the pivot trades one kind of complexity for another, and we should be honest about what we lose. A's persistent integration branch is genuinely useful ops tooling: `git log spur/plan-integration/{plan_id}` is a tangible audit trail an operator can grep when a downstream task behaves strangely. G-strict makes that state ephemeral, which complicates debugging. However, E's persistent branch requires correct reset/replay logic on reject-after-approve, supersede, and must-fix pushes — mutable shared state that has bitten us before. MCTS weighted correctly: the ~500ms cherry-pick cost is negligible against worker init, and strict isolation (workers see only declared deps) prevents the silent cross-task pollution that caused bd-2dww. I do not think the analysis over-indexed on purity; it under-weighted debuggability. That is fixable with logging, not by reverting to A.

## Cognitive load on brain agents

For plan tasks, the load is zero — the plan engine computes the overlay from `depends_on`, so the brain calls `delegate_to_worker(agent, task)` exactly as today. For ad-hoc dispatches, `BaseSpec` adds a parameter the brain must reason about. `RepoMain` is the correct default for exploratory work (most ad-hoc dispatches). Non-`RepoMain` bases matter only when the brain is intentionally chaining off a prior worker branch — a relatively rare "continuation" pattern that today is handled by passing context files, not by git state. My read: the added parameter is acceptable if the default is `RepoMain` and the tool description makes `WithOverlay` opt-in with a clear example.

## Plan engine as brain proxy

This is the right division of labor. The plan engine owns the DAG and the approved-set; the brain owns intent. Mechanical overlay computation belongs in the engine, not the brain prompt. That said, the brain should be able to *override* the computed base — not for routine plan tasks, but for exceptional cases like "I want this task to see the pre-merge state despite the dependency edge." A read-only `get_task_base_preview` tool would let the brain inspect what the engine will compute without requiring it to do the math itself. Override can ship later; preview should ship with G-strict.

## Stuck-at-dispatch — ops experience

Surfacing conflicts at dispatch is strictly better than today's silent data loss (bd-2dww). When a downstream actually needs two conflicting upstream tips, G-strict fails fast with a clear signal rather than letting the worker proceed on stale base and lose 700 LoC.

The bad news: we lack operator tooling to handle the failure gracefully. Today the brain would have to:
- Abort the plan (expensive, loses work)
- Re-spec the downstream task to exclude one dependency (requires human-in-the-loop or a new merge-task primitive)
- Manually resolve the conflict in a fresh worktree (no tool exists for this)

What we need before this is production-critical:
- A `preview_task_base(plan_id, task_id)` MCP tool that returns the computed overlay commits and any cherry-pick conflicts without creating a worktree
- A `create_merge_task(plan_id, parent_tasks[])` primitive so the brain can insert an explicit merge node into the DAG rather than aborting
- Clear event emission when dispatch is blocked on conflict, surfaced in the TUI as a distinct state (not just `Pending`)

## Boring shippable cut

- **Phase 1:** G-strict overlay computation for plan tasks only; ad-hoc delegations keep branching from `RepoMain` silently
- **Phase 1:** D (clobber detector) at review time; emit `signal:potential_clobber` when a worker recreates a file touched by an already-approved upstream tip
- **Phase 1:** `preview_task_base` dry-run tool so the brain can see conflicts before dispatch
- **Phase 2 (later):** `brain-explicit-base` for ad-hoc dispatches, with `RepoMain` as the safe default
- **Phase 2 (later):** `create_merge_task` or equivalent for DAG-level conflict resolution

## Sharpest concern

Without a dry-run preview, stuck-at-dispatch becomes a new failure mode that replaces silent data loss with an explicit but unactionable deadlock — the brain knows there is a conflict but has no tool to resolve it except abort.
