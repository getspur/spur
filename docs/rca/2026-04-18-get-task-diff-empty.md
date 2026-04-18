# RCA: `get_task_diff` empty result for bd-1mh.2

**Date:** 2026-04-18 (v1 investigation), 2026-04-18 (v2 recommendation sharpened to Option E after MCTS review)
**Severity:** medium — brain reviews blind (no diff text); no user-visible crash, no data loss.
**Status:** investigation complete; recommended fix = Option E (C + handler marker); deferred to follow-up spec.

---

## Observed

On 2026-04-17, during live dogfooding of epic `bd-1mh`, the brain called
`get_task_diff(plan_id="8251a6df-ac24-4391-8a7e-3aff378734a7", task_id="bd-1mh.2")`
and received a response containing `task_id`, `agent`, `status`, `summary`, `task_description`,
and `worker_branch` — but **no `diff` field at all**. The task had been completed by
`claude-code-acp` with commit `95e8b73` on branch
`spur/worker-claude-code-acp-42193276-4e62-4673-a136-664d074818c5`. The brain was forced
to call `git show 95e8b73` directly to read the diff.

**Expected (per design intent):** `get_task_diff` returns a JSON object with a `"diff"`
key containing the full unified diff of the worker's changes.

**Actual:** The JSON response had no `"diff"` key; `entry.result.diff` was `None`.
Current implementation silently omits the key when no diff text is stored.

---

## Reproduction

**Cannot reproduce interactively** — the plan state is only live during a `spur run` session
and is held in memory (not persisted to disk). The specific plan state from the dogfooding
run no longer exists.

However, the root cause is **deterministic and structurally reproducible**: any worker that
commits its own changes before the orchestrator calls `collect_diff` will trigger this
issue. The worker in this case (`claude-code-acp`, via the `finishing-a-development-branch`
skill) always commits before exiting — the worker's summary confirms "Committed as `95e8b73`".

Reproduction steps (requires a live `spur run` session):
1. Run `execute_epic` with an agent that commits its own work before reporting done.
2. Poll `get_plan_status` until a task reaches `awaiting_review`.
3. Call `get_task_diff(plan_id, task_id)` — the response will lack a `"diff"` field.

---

## Code path traced

**Tool definition:**
`crates/spur-mcp/src/tools.rs:671` — `get_task_diff_def()`. Input: `plan_id`, `task_id`, optional `attempt`.

**Handler:**
`crates/spur-mcp/src/server.rs:1555` — `handle_get_task_diff`. Reads `entry.result.diff`
from the in-memory `PlanTaskEntry`. If `result.diff` is `None`, the `"diff"` key is simply
omitted from the response (lines 1638–1648). No error, no warning, silent omission:

```rust
if let Some(ref result) = entry.result {
    if let Some(ref diff) = result.diff {
        resp.insert("diff".into(), json!(diff));      // only inserted if Some
    }
    // diff_summary and summary follow the same pattern
}
```

This is already a contract drift from the original review-loop design, which described
`get_task_diff` as always returning the full unified diff for the task.

**How `diff` is populated:**
`crates/spur-core/src/orchestrator.rs:3687` — `collect_diff` is called after the worker
connection shuts down but **before** `apply_worktree_cleanup` commits anything:

```rust
let _ = connection.shutdown().await;

// 4. Collect diff.
let diff = worktrees.collect_diff(&worker_session).await.unwrap_or(None);
```

`collect_diff` (`crates/spur-worktree/src/manager.rs:206`) runs `git diff HEAD` in the
worktree:

```rust
let diff = self.run_git(&["diff", "HEAD"], Some(&info.path)).await?;
if diff.is_empty() { Ok(None) } else { Ok(Some(diff)) }
```

`build_diff_summary` in `crates/spur-core/src/orchestrator.rs:3701` only runs when
`diff.is_some()`, and its helper (`orchestrator.rs:3822`) also uses `git diff --numstat HEAD`.
So once a worker self-commits, both the raw `diff` and the structured `diff_summary` disappear.

**The problem:** `git diff HEAD` compares the working directory against the current HEAD
commit. If the worker itself committed its changes before exiting (which `claude-code-acp`
does via the `finishing-a-development-branch` skill — confirmed in the worker's summary:
"Committed as `95e8b73`"), then by the time `collect_diff` runs, HEAD already points to
`95e8b73` and the working tree is clean. `git diff HEAD` outputs nothing, so `collect_diff`
returns `Ok(None)`, and `DelegationResult.diff` is `None` for the entire downstream path.

**Where the `None` propagates:**
`crates/spur-mcp/src/plan.rs:652` — `entry.result = Some(result)` stores the
`DelegationResult` with `diff: None`.

`crates/spur-mcp/src/server.rs:1638` — the handler skips inserting the `"diff"` key, so
the brain receives a structurally valid but diff-less response.

---

## Root cause

**H3: Empty-success path.** `collect_diff` runs `git diff HEAD` after the worker commits its
own changes. The working tree is clean at that point; `git diff HEAD` exits 0 with empty
stdout. `collect_diff` correctly returns `Ok(None)` (an empty diff is semantically "no
uncommitted changes"), and this `None` propagates to `DelegationResult.diff` and then to
the MCP response. No error is raised; the MCP tool silently omits the `"diff"` field.

Evidence:
1. Event log `73678-1776436102161-0.ndjson` seq=1708: the `get_task_diff` response for
   `bd-1mh.2` contains `worker_branch`, `summary`, `status`, `task_description` — but no
   `diff` key.
2. Worker summary in the same event explicitly states: "Committed as `95e8b73`. Work is on
   the worker branch — the orchestrator handles merge/PR from here."
3. `git show 95e8b73` confirms the commit exists locally, and `git branch --contains 95e8b73`
   shows it on `spur/worker-claude-code-acp-42193276-4e62-4673-a136-664d074818c5`.
4. `collect_diff` at `manager.rs:210` uses `git diff HEAD`, which is empty once the worker
   commits. There is no fallback to `git show <HEAD>` or `git diff HEAD~1..HEAD`.
5. The other hypotheses are ruled out: H1 (base branch wrong) is inapplicable since
   `collect_diff` uses HEAD-relative diff, not main-relative. H2 (branch not fetched) is
   inapplicable — the worker branch existed in the local worktree. H4 (missing commit_sha)
   is not the code's model — the field is `diff: Option<String>`, not `commit_sha`. H5
   (timing/race) is inapplicable — the diff was `None` at worker-completion time, not stale.

---

## Options considered

### Option A: Use `git show HEAD` / `git diff HEAD~1..HEAD` as a last-commit fallback

When `collect_diff` returns `None` (no uncommitted changes), the orchestrator could fall
back to `git show HEAD` (or `git diff HEAD~1..HEAD`) to capture any commit the worker made
as its last action. This recovers the common case where the worker made exactly one final
commit, but it is only a heuristic. In the current architecture, the worktree may branch
from a synthetic snapshot commit, and workers are not guaranteed to make only one commit.
So `HEAD~1..HEAD` is **not** "the task diff"; it is only "the last commit diff". It also
does not fix the matching `diff_summary` gap unless the summary path adopts the same fallback.
Implementation would be small, but semantics are weaker than they first appear.
Regression risk: **medium** — it can silently miss earlier commits within the same task.

### Option B: Add a `commit_sha` field to `DelegationResult`; populate it from the worktree HEAD

After `collect_diff`, the orchestrator could read `git rev-parse HEAD` in the worktree
and store it as `DelegationResult.commit_sha`. The `handle_get_task_diff` handler would
then run `git show <commit_sha> --unified` on demand when `result.diff` is `None`. This
separates diff collection from worker runtime (diffs can be large; on-demand is cheaper).
Implementation touches `spur-acp/src/domain/delegation.rs` (+1 field, ~5 lines),
`orchestrator.rs` (+git rev-parse call, ~10 lines), and `server.rs` (+git show call, ~15
lines). Regression risk: **low-medium** — requires serialization schema bump for
`DelegationResult`; any code that deserializes old records would need a `#[serde(default)]`
annotation on the new field (already the pattern in this codebase).

### Option C: Use the recorded `base_commit` when the working tree is clean

The worktree manager already records `base_commit` at creation time. A stronger fix is:
keep the current `git diff HEAD` path for uncommitted changes, and when that returns empty,
compute the task delta as `git diff <base_commit>..HEAD` instead. The same basis should be
used for `diff_summary`, otherwise the raw diff and structured stats diverge.

This captures all task changes even if the worker made multiple commits, and it does not
depend on `HEAD~1` being meaningful. Implementation would require plumbing the recorded
`base_commit` into the fallback path in `collect_diff` (or adding a parallel helper) and
teaching `build_diff_summary` to use the same basis.
Regression risk: **low-medium** — this is a semantic correction, but it aligns the tool
with what reviewers actually need: the delta produced by the task, not merely the set of
uncommitted files left behind.

**Weakness Option C alone does not address:** when the worker genuinely made no changes
(investigation task, no-op), `base_commit..HEAD` is empty, `result.diff` stays `None`, and
the handler continues to silently omit the `"diff"` key. The brain still cannot
distinguish "correctly produced no changes" from "collection silently failed."

### Option E: Option C + handler-side explicit "no changes" marker (recommended)

Same collection-side fix as Option C, PLUS a small change in `handle_get_task_diff`
(`crates/spur-mcp/src/server.rs:1638`): when `result.diff` is `None`, ALWAYS insert the
`"diff"` key with a structured marker object instead of silently omitting it. Shape:

```json
{
  "diff": null,
  "diff_status": "no_changes_detected",
  "diff_basis": "base_commit..HEAD"
}
```

The brain then receives ONE key consistently and can distinguish a correct no-change
outcome (approve confidently) from a collection failure (escalate). This directly
addresses the "absence of diff = ambiguous" mental-model gap that Options A, B, and C
leave intact.

Implementation delta over Option C: ~10 LOC in `server.rs` for the marker insertion.
Total Option E: ~35 LOC + 4 tests. Regression risk: **low-medium** — the new key only
appears in the previously-empty-response case, so existing consumers that already
receive `"diff"` are unaffected.

---

## Recommendation

**File follow-up and fix in next spec — adopt Option E (C + handler marker).**
Root cause is identified and unambiguous: H3. Option E strictly dominates Options A, B,
and C because it fixes both failure points — collection AND response-shape — with a
small increment over C alone.

Required changes in the follow-up spec:

1. **Collection fallback.** Keep `git diff HEAD` for uncommitted changes. When it returns
   empty, compute raw diff from `base_commit..HEAD`.
2. **Parallel summary path.** `build_diff_summary` MUST use the same basis as the raw
   diff. This is not optional — the raw diff and structured stats must agree. Mentioned
   here as a required step so it does not get dropped from the implementation plan.
3. **Handler marker (Option E addition).** `handle_get_task_diff` ALWAYS inserts the
   `"diff"` key. When `result.diff` is `None`, the value is `null` and the response
   carries a `"diff_status"` field explaining why (e.g., `"no_changes_detected"`).

**Required test coverage:**
- (a) Single-commit worker: `get_task_diff` returns the commit's diff.
- (b) Multi-commit worker (2+ commits on the branch): returns the union of all commits.
- (c) No-change worker: returns `"diff": null, "diff_status": "no_changes_detected"`.
  Explicitly asserts the handler does NOT silently omit the key.
- (d) `diff_summary` is computed on the same basis as the raw diff in each scenario.

**Explicit non-goals for the follow-up spec** (to prevent scope creep):
- Do NOT add a `commit_sha` field to `DelegationResult` (rejected Option B; schema
  churn for ambiguous gain).
- Do NOT persist `PlanState` to disk (out of scope; addressed in a separate durability
  track if/when needed).
- Do NOT change the `DelegationResult` schema (the fix lives in collection + handler; the
  `diff: Option<String>` field stays as-is).
- Do NOT introduce retry/backoff for `collect_diff` (one-shot is fine; the base-aware
  fallback removes the failure mode).

---

## Follow-up

File a beads task under epic `bd-1mh` (or `bd-33r` UX track):

> **`get_task_diff` returns no diff when worker self-commits, and silently omits the key
> on genuine no-change tasks.** Adopt Option E (Option C + handler marker):
>
> 1. `collect_diff` (`crates/spur-worktree/src/manager.rs:206`) — when `git diff HEAD`
>    is empty, fall back to `git diff <base_commit>..HEAD` using the worktree's recorded
>    `base_commit`.
> 2. `build_diff_summary` (`crates/spur-core/src/orchestrator.rs:3822`) — use the same
>    basis as the raw diff for each case.
> 3. `handle_get_task_diff` (`crates/spur-mcp/src/server.rs:1638`) — always insert the
>    `"diff"` key. When `result.diff` is `None`, emit
>    `{ "diff": null, "diff_status": "no_changes_detected", "diff_basis": "base_commit..HEAD" }`
>    so the brain can distinguish genuine no-change outcomes from collection failures.
>
> Tests: single-commit, multi-commit, no-change, diff_summary-matches-basis (four cases).
>
> Estimated: ~35 LOC across three files + 4 tests. Regression risk low-medium.
> Non-goals: no `commit_sha` field, no `DelegationResult` schema change, no `PlanState`
> durability.
