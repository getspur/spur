# RCA: parked worker branch refs and dirty-snapshot contamination (/explore phase-1 plan run)

- **Date:** 2026-07-08
- **Plan:** `4abe04d1-7284-48b1-afcf-e217dbc8b50e` (docs/superpowers/plans/2026-07-07-explore-phase1.md, 8 tasks, codex workers)
- **Severity:** high (silent loss of upstream work between tasks; near-miss regression of parallel work at merge)
- **Outcome:** plan completed 8/8 and merged, but only via manual brain-side repairs on every task and a hand-built integration branch.

## Summary

Two independent plan-engine defects interacted across one plan run:

1. **Worker branch refs stayed parked at the brain snapshot.** Task commits survived only in the object store and artifact diffs, so downstream overlays came out empty and `merge_plan` could not assemble the plan.
2. **The brain snapshot captured a torn view of a dirty working tree.** Modified tracked files were snapshotted while the untracked files they referenced were not, so no worker could compile the workspace — and 11 files of stale WIP rode along into every worker branch and the final merge.

## Defect 1: worker branch refs not updated with worker commits

### Observed

- After each early task completed and its worktree was cleaned up, the recorded `spur/worker/v2/codex/<session>/<id>` branch tip still pointed at the brain snapshot `c69a8f60a`. The worker's commits existed in the object store (reachable via reflog/artifact OIDs) but were not on the branch.
- `preview_task_base` returned `base_oid == tip_oid` (empty overlay) for downstream tasks.
- Consequence: exp-2 was dispatched without exp-1's files, concluded the plan text described unbuilt work, reimplemented exp-1's scope from scratch, and emitted a `scope_drift` signal → escalation. The same pattern threatened every later task.
- `merge_plan` ultimately conflicted on exp-1 (its branch had no commits above the snapshot to cherry-pick), with `merged_task_ids: []` and an empty `files` list.

### Brain-side workaround (repeatable procedure)

Before approving each task:

1. Fetch the attempt artifact (`fetch_outcome_artifact`, `section='diff_only'`) — the authoritative reviewed content.
2. Locate the worker's implementation commits (close-out SHAs, or the `spur: worker codex output` normalize squash) and verify parentage chains onto the expected base.
3. `git branch -f <worker-branch> <impl-commit-sha>`.

After the first round of repairs (exp-2..exp-5), subsequent dispatches (exp-7, exp-8) received fully populated overlay bases — each new worker branch contained all approved predecessors' commits replayed in order — and the scope-drift pattern stopped. This confirms the overlay extractor reads branch refs, and that the missing piece is only the ref update at completion time.

### Root-cause status

Not yet traced to code. Hypothesis: the completion path records the result commit OID into the plan store but the branch ref update (`git branch -f` / `update-ref`) is skipped or races with worktree cleanup. Follow-up: trace `run_one_worker_attempt` → outcome persistence → worktree teardown for the ref-update call.

### Note on the merge-time consequence

Even with refs repaired, `merge_plan`'s per-task cherry-pick assembly could not handle the degenerate case (exp-1 intentionally left empty because exp-2's superset subsumed it; restoring exp-1 would have created add/add conflicts). Resolution: the final task's branch (`e038afc60`) was already the complete cumulative chain — every approved task's commits on the snapshot, the exact tree the last worker's full-crate gates ran on — so the plan-merge branch was pointed there manually and merged.

## Defect 2: brain snapshot captures dirty working tree (torn state)

### Observed

- Snapshot `c69a8f60a` contained a modified `crates/spur-tui/src/lib.rs` declaring `pub mod git_info;` while `git_info.rs` itself was untracked WIP and therefore absent from the snapshot.
- Every worker inherited a workspace that could not compile, so `cargo fmt` pre-commit hooks failed and workers resorted to `--no-verify` commits and ad-hoc restorations (exp-6 attempt 1 restored `git_info.rs` from main and added lint suppressions; attempt 2 produced a cleaner convergent restore).
- Beyond the compile break, the snapshot carried **11 spur-tui files of uncommitted WIP** (status bar, dashboard/browser views, status-bar tests). A parallel session subsequently landed the finished versions of those files on `main` (UX e2e plan `689837c0`). At merge time, most of the stale WIP **auto-merged silently** into the integration tree — only `git_info.rs` surfaced as a visible conflict.

### Containment at merge

The integration merge excised the contamination wholesale: `git checkout main -- crates/spur-tui/` (safe because the plan's only spur-tui changes — the git_info restore and a dead re-export removal — converged with main). The final staged diff vs main was verified to be exactly the 28 reviewed plan files.

### Root cause

`submit_plan` snapshots the working tree as-is: uncommitted **tracked** modifications are included, **untracked** files are not. Any WIP that spans both (the common case for in-progress features) produces a torn, non-compiling snapshot, and any WIP at all leaks into every worker branch and the eventual merge.

### Fix directions (pick one)

1. **Require a clean tree at `submit_plan`** (error out listing dirty paths) — simplest, matches "beads as source of truth" discipline.
2. Snapshot `HEAD` instead of the working tree (workers see only committed state).
3. Include untracked files in the snapshot (fixes the torn-compile case but still leaks WIP into worker branches and merges — not recommended alone).

Option 1 or 2 also eliminates the silent-auto-merge regression class entirely.

## Lessons / follow-ups

- [ ] Trace and fix the branch-ref update gap (Defect 1) in the completion/teardown path.
- [ ] Add a clean-tree guard or HEAD-based snapshot to `submit_plan` (Defect 2).
- [ ] `merge_plan` should treat an empty overlay (tip == snapshot) as "nothing to pick, continue" rather than a conflict, and populate `files` in conflict payloads.
- [ ] Reviewer habit worth keeping: verify each task's branch tip against the artifact diff before approving; repair refs *before* approval so the reconciler dispatches dependents with correct overlays.
