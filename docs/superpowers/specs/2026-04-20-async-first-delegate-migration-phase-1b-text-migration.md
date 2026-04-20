# Phase 1b Text Migration — Review Doc

**Status:** review (draft, 2026-04-20)
**Scope:** text-only migration in `crates/spur-mcp/src/tools.rs` (5 description strings). No SKILL.md changes required.
**Parent spec:** `docs/superpowers/specs/2026-04-20-async-first-delegate-migration-design.md`
**Provenance:** drafted in-turn by the brain after a `gemini-acp` dispatch returned `Success` without producing an artifact. Scope was confirmed independently via direct grep.

## Blast-radius audit

Verified via `grep -r "poll|check_delegation_status|wait_delegation" .spur/skills/brain-delegation*`:

- **Zero matches** across `brain-delegation/SKILL.md`, `brain-delegation-claude-code/SKILL.md`, `brain-delegation-claude-code-acp/SKILL.md`, `brain-delegation-codex/SKILL.md`, `brain-delegation-gemini/SKILL.md`, `brain-delegation-kiro/SKILL.md`.
- All polling language lives in `tools.rs`.
- Phase 1b scope collapses from *"tools.rs + N skill files"* to *"tools.rs only, 5 strings"*.

## Proposed replacements

### 1. `delegate_to_worker_def` — `tools.rs:59`

**Current:**

```
Delegate a task to a worker agent. Blocks until the worker completes or a 90-second
safety timeout is reached. If the worker is still running at timeout, returns a
delegation_id — call check_delegation_status to poll for the result. Pass a
`delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step
work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT.
Use `list_available_workers` when routing is ambiguous.
```

**Proposed:**

```
Delegate a task to a worker agent. Returns inline if the worker finishes within the
inline-wait window (configurable via `delegation.inline_wait_ms`; default 0).
Otherwise returns `{status: "pending", delegation_id}` and you will be re-prompted
automatically when the worker completes — you do not need to poll. Pass a
`delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step
work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT.
Use `list_available_workers` when routing is ambiguous.
```

**Rationale:** removes the "90-second safety timeout" from the brain's mental model; names the config knob; makes the auto-reprompt contract explicit.

### 2. `delegate_parallel_def` — `tools.rs:67`

**Current:**

```
Delegate multiple tasks in parallel. Blocks until all complete. The
`delegation_plan.decomposition` field MUST demonstrate subtasks are independent —
no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker`
serially.
```

**Proposed:**

```
Delegate multiple tasks in parallel. Returns a response array of length N; each
element is either an inline result or `{status: "pending", delegation_id}` with an
automatic re-prompt when that task completes. The `delegation_plan.decomposition`
field MUST demonstrate subtasks are independent — no shared state, no sequential
data dependencies. If unsure, use `delegate_to_worker` serially.
```

**Rationale:** captures the Phase 2 target semantics (per-task continuations, partial-inline results) while keeping the independence guardrail. Safe to land at Phase 1b because the array-length invariant (INV-ASYNC-6) is already guaranteed today.

### 3. `delegate_async_def` — `tools.rs:219`

**Current:**

```
Delegate a task to a worker agent without blocking. Returns a delegation_id that
can be collected later with wait_delegation.
```

**Proposed:**

```
[DEPRECATED — use `delegate_to_worker`; it has equivalent async semantics with
auto re-prompt on completion.] Delegate a task to a worker agent without blocking.
Returns a delegation_id; the brain is re-prompted automatically when the worker
finishes.
```

**Rationale:** prepares for Phase 3 deprecation. Tells the brain where to go instead. Keeps the tool functional until Phase 4 removes it.

### 4. `wait_delegation_def` — `tools.rs:227`

**Current:**

```
Block until an async delegation completes and return its result. Use after
delegate_async. If the worker is still running after 90 seconds, returns a
'still running' message — call check_delegation_status to poll again.
```

**Proposed:**

```
[DEPRECATED — auto re-prompt from `delegate_to_worker` / `delegate_async` makes
this unnecessary.] Block until an async delegation completes and return its result.
```

**Rationale:** removes polling-loop instructions entirely. Phase 3 marks the tool deprecated; Phase 4 removes it.

### 5. `check_delegation_status_def` — `tools.rs:244`

**Current:**

```
Non-blocking poll for a delegation result. Returns the result immediately if the
worker has finished, or {"status":"running"} if still in progress. Use after
delegate_async or when delegate_to_worker / wait_delegation returned a delegation_id
due to timeout.
```

**Proposed:**

```
Non-blocking status query for a delegation. Returns the result if finished, or
`{"status":"running"}`. Primarily a debugging affordance — brains are re-prompted
automatically when delegations complete and normally do not need to call this.
```

**Rationale:** reframes from *"poll this"* to *"debug only"*. Preserves the tool per spec non-goals ("Removing `check_delegation_status` (stays as a debugging / TUI affordance)"). Drops the stale "due to timeout" clause that no longer applies once `inline_wait_ms` defaults to 0.

## Breaking changes

When brains consume these revised descriptions:

1. Brains stop producing post-timeout `check_delegation_status` calls in most cases.
2. Brains may end their turn after receiving `{status: "pending", delegation_id}` instead of continuing to probe.
3. Any skill or system prompt that hard-codes the phrase *"90-second safety timeout"* or *"still running"* needs review. **None found in `.spur/skills/brain-delegation*/SKILL.md` (grep verified).** May exist in user-level `CLAUDE.md` or ambient context — out of scope for Phase 1b.
4. Brains that had learned to interpret the free-form "still running" string may now see structured `{status: "pending", …}` JSON instead. That is the Phase 1c response-shape change; Phase 1b aligns the description with it.

## Rollout ordering — recommendation

The parent spec currently lists sub-phases as **1a → 1b → 1c**. This review recommends reordering to **1a → 1c → 1b**, landing descriptions LAST.

Reasoning:

- Phase 1a wires `BlockTimeout` continuation. The "auto re-prompt" sentence is true immediately once 1a ships.
- Phase 1c introduces the `{status: "pending", …}` structured response shape AND the `delegation.inline_wait_ms` config.
- Phase 1b's proposed text references `delegation.inline_wait_ms` by name.
- If 1b ships before 1c, descriptions name a config that doesn't exist. Functionally harmless (default would be 0 anyway), but misleading.
- Landing 1b last keeps the text and the behavior in lockstep.

**Trade-off:** holding 1b extends the window during which brains still see polling language in tool descriptions. That window is equal to 1c's development time. Acceptable because Phase 1a already wires continuations — polling works but is no longer necessary, and brains tend to converge on the cheaper path empirically.

## Files examined

| Path | Polling matches |
|---|---|
| `crates/spur-mcp/src/tools.rs:59` (`delegate_to_worker_def`) | 1 |
| `crates/spur-mcp/src/tools.rs:67` (`delegate_parallel_def`) | 0 (surprise) |
| `crates/spur-mcp/src/tools.rs:219` (`delegate_async_def`) | 0 (but mentions `wait_delegation`) |
| `crates/spur-mcp/src/tools.rs:227` (`wait_delegation_def`) | 1 |
| `crates/spur-mcp/src/tools.rs:244` (`check_delegation_status_def`) | 1 (definitional) |
| `.spur/skills/brain-delegation/SKILL.md` | 0 |
| `.spur/skills/brain-delegation-claude-code/SKILL.md` | 0 |
| `.spur/skills/brain-delegation-claude-code-acp/SKILL.md` | 0 |
| `.spur/skills/brain-delegation-codex/SKILL.md` | 0 |
| `.spur/skills/brain-delegation-gemini/SKILL.md` | 0 |
| `.spur/skills/brain-delegation-kiro/SKILL.md` | 0 |

Verification command: `grep -r "poll\|check_delegation_status\|wait_delegation" .spur/skills/brain-delegation*` — zero output lines.

## Summary

Scope collapses to 5 description strings in `tools.rs`; skill files are already clean. One notable surprise: `delegate_parallel_def` contains no polling language today — its rewrite is about capturing the Phase 2 per-task-continuation end state, not scrubbing existing text. Recommend reordering the Phase 1 sub-phases to **1a → 1c → 1b** so descriptions ship last and never over-promise. This reordering should be reflected in the parent spec's "Phase ramp" flowchart.
