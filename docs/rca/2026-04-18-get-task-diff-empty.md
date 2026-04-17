# RCA: `get_task_diff` empty result for bd-1mh.2

**Date:** 2026-04-18
**Severity:** medium — brain reviews blind (no diff text); no user-visible crash, no data loss.
**Status:** investigation complete; fix deferred to follow-up spec.

---

## Observed

On 2026-04-17, during live dogfooding of epic `bd-1mh`, the brain called
`get_task_diff(plan_id="8251a6df-ac24-4391-8a7e-3aff378734a7", task_id="bd-1mh.2")`
and received a response containing `task_id`, `agent`, `status`, `summary`, `task_description`,
and `worker_branch` — but **no `diff` field at all**. The task had been completed by
`claude-code-acp` with commit `95e8b73` on branch
`spur/worker-claude-code-acp-42193276-4e62-4673-a136-664d074818c5`. The brain was forced
to call `git show 95e8b73` directly to read the diff.

**Expected:** `get_task_diff` returns a JSON object with a `"diff"` key containing the
full unified diff of the worker's changes.

**Actual:** The JSON response had no `"diff"` key; `entry.result.diff` was `None`.

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
3. `git show 95e8b73` confirms the commit exists locally on `main` (merged after the fact).
4. `collect_diff` at `manager.rs:210` uses `git diff HEAD`, which is empty once the worker
   commits. There is no fallback to `git show <HEAD>` or `git diff HEAD~1..HEAD`.
5. The other hypotheses are ruled out: H1 (base branch wrong) is inapplicable since
   `collect_diff` uses HEAD-relative diff, not main-relative. H2 (branch not fetched) is
   inapplicable — the worker branch existed in the local worktree. H4 (missing commit_sha)
   is not the code's model — the field is `diff: Option<String>`, not `commit_sha`. H5
   (timing/race) is inapplicable — the diff was `None` at worker-completion time, not stale.

---

## Options considered

### Option A: Use `git diff HEAD~1..HEAD` as fallback when `git diff HEAD` is empty

When `collect_diff` returns `None` (no uncommitted changes), the orchestrator could fall
back to `git show HEAD` (or `git diff HEAD~1..HEAD`) to capture any commit the worker made
as its last action. This is safe because the worktree is a throwaway branch — `HEAD~1` is
always the base commit (the main-branch snapshot taken at worktree creation). Implementation
would add ~10 lines to `collect_diff` or to the `execute_delegation` call site in
`orchestrator.rs` around line 3687. Regression risk: **low** — purely additive; the
existing `git diff HEAD` path is unchanged, and the fallback only fires when it returns
empty. One edge case: if a worker legitimately produces no changes, `HEAD~1..HEAD` would
also be empty (or show unrelated base commits), but in that situation `DelegationStatus`
would be `Failed`, not `Success`, so the fallback would never activate on a legitimate
no-change task.

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

### Option C: Change `collect_diff` to use `git diff <base_branch>..HEAD` instead of `git diff HEAD`

Compute the diff against the branch point (the main snapshot used when creating the
worktree). This captures all changes regardless of whether the worker committed.
Implementation would require passing the base branch or base SHA into `collect_diff` (~5
lines in `manager.rs`, ~5 lines at call site). Regression risk: **medium** — changes
behavior for the non-commit case (workers that leave changes uncommitted), which currently
produces a working-tree diff vs HEAD. Merging both cases into one `base..HEAD` diff is
semantically correct for plan review, but it changes what `DiffSummary` reports for ad-hoc
delegations where the worker does NOT commit.

---

## Recommendation

**File follow-up and fix in next spec.** Root cause is identified and unambiguous: H3.
Option A is the lowest-risk fix (~10 LOC in `orchestrator.rs`, no schema changes, additive
fallback). It should be filed as a small follow-up spec item and implemented in the next
close-the-loop iteration alongside any other `get_task_diff` polish.

---

## Follow-up

File a beads task under epic `bd-1mh` (or `bd-33r` UX track):

> **`get_task_diff` returns no diff when worker self-commits**: `collect_diff` uses
> `git diff HEAD` which is empty after a worker commit. Fallback to `git show HEAD` (or
> `git diff HEAD~1..HEAD`) in `orchestrator.rs` around line 3688 when `collect_diff`
> returns `None` and `DelegationStatus` is `Success`/`Modified`.
> File: `crates/spur-core/src/orchestrator.rs` and/or
> `crates/spur-worktree/src/manager.rs`. Estimated: ~10 LOC, regression risk low.
