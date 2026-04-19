# RCA — Phase 3a Worker Dispatch: Failure Modes and Plan-Completion Blind Spots

**Date:** 2026-04-19
**Context:** Phase 3a "Low-Risk Hardening" plan (6 tasks) dispatched to the spur orchestrator for execution by external ACP workers (gemini, codex, claude-code-acp). Brain session: this Claude Code session. Plan files: `docs/superpowers/plans/2026-04-19-phase3a-low-risk-hardening.md`.

**Outcome:** 4 of 6 tasks reached clean approved state through the MCP `submit_plan` → `review_task` workflow; 2 tasks had to exit the workflow (DN-5 approved-on-disk but the plan never observed completion, DN-2 worker hung and required manual implementation). 8 dispatch attempts across 4 workers exhibited 5 distinct failure modes.

**Post-merge re-review (current `HEAD`, 2026-04-19):** several hardening changes from this run have since landed, including `RetryLoop` extraction, cancel-path `DelegationCompleted`, `PlanTaskStatus::Cancelled`, and `run_plan` terminal-exit cleanup. Those merges narrow some of the original incident surface, but they also sharpen two conclusions:
- `Unknown delegation` from `check_delegation_status` is **not**, by itself, evidence of plan-state corruption for `submit_plan` tasks. Plan tasks advance through per-task oneshot receivers in `run_plan`, not the MCP polling registry.
- A new semantic split now exists around `Cancelled` dependencies: one scheduler path treats them as dep-satisfied and another does not. See §§4.1, 4.2, and 4.5.

---

## 1. Dispatch ledger

| # | Task | Agent | Attempt | Delegation ID | Outcome | Failure mode |
|---|---|---|---|---|---|---|
| 1 | UP-1 | gemini | 1 | faf1f97f | ✅ approved | — |
| 2 | UP-4 | codex | 1 | f8839e14 | ✅ approved | — |
| 3 | DN-2 | gemini | 1 | 80eae897 | ❌ request_changes | **Hallucinated diff** |
| 4 | DN-2 | gemini | 2 | 1531349e | ❌ rejected | **Scope creep** |
| 5 | DN-4 | gemini | 1 | d23ad050 | ❌ request_changes | **Shallow tests** |
| 6 | DN-4 | gemini | 2 | afc54e76 | ✅ approved | — (after strong feedback) |
| 7 | DN-5 | codex | 1 | — (pre-dispatch fail) | ❌ failed | **Stash contention** |
| 8 | DN-5 | codex | standalone async | 86bbef0a | ❌ no-work | **Silent no-op** |
| 9 | DN-6 | codex | 1 | ce54abbf | ❌ rejected | **Silent no-op** |
| 10 | DN-2 v2 | claude-code-acp | 1 | 103a0fd4 | ⏸ **hung** (21+ min, zero commits) | **Plan-completion blind spot** |
| 11 | DN-5 v2 | claude-code-acp | 1 | e3f7c8c2 | ⚠ code DONE on branch, plan still `dispatched` | **Plan-completion blind spot** |
| 12 | DN-6 v2 | claude-code-acp | 1 | — | ✅ approved | — |

---

## 2. Failure mode taxonomy

### 2.1 Hallucinated diff (gemini DN-2 attempt 1, commit `80eae897`)

**Observation:** Worker's self-summary claimed `"Ported delegate_internal"` and `"Deleted Comment"` but the returned diff showed only a ~50-line change to the test-support function; the production retry site at `orchestrator.rs:~3046` and the comment at `orchestrator.rs:~2984` were untouched.

**Evidence:** `git show` of `spur/worker-gemini-8266ccab-...` contained only the test_support hunk.

**Severity:** High. The summary is the reviewer's first signal; a wrong summary leads to false-approve risk if the reviewer doesn't pull the full diff.

**Worker:** gemini

---

### 2.2 Scope creep (gemini DN-2 attempt 2, branch `aa6ccf84`)

**Observation:** Second attempt at DN-2 produced a 759-insertion / 486-deletion diff across 6 files. Out-of-scope edits included:
- `crates/spur-cli/src/commands/mod.rs` — removal of `pub mod init;`
- `crates/spur-cli/src/main.rs` — inlining of `cmd_init` and addition of a 22-line `INSTALL_HINTS` constant for agent install commands.

These have no bearing on extracting a `RetryLoop` combinator in `spur-core`.

**Hypothesis:** Worker branch appears to have been created from a base that included unrelated in-progress work (possibly a brain snapshot that contained pre-existing uncommitted user changes), and the worker attempted to "fix" compile errors it encountered by refactoring unrelated files. The summary also mentioned wrapping closure state in `Arc<tokio::sync::Mutex<DelegationState>>` — an over-engineered pattern that was not requested and not necessary.

**Severity:** High. Unreviewable in the review-by-diff workflow; the invariant of "a task's diff == the task's contract" is broken.

**Worker:** gemini

---

### 2.3 Shallow tests (gemini DN-4 attempt 1, branch `3cc616cd`)

**Observation:** Production code was correct (Cancelled variant added, non-cascade semantic preserved, PlanCompleted.cancelled with serde default). But the 4-test file was composed entirely of type-checks of the form:

```rust
#[tokio::test]
async fn test_non_cascade_on_dep() {
    let status = PlanTaskStatus::Cancelled { reason: "test".to_string() };
    assert!(matches!(status, PlanTaskStatus::Cancelled { .. }));
}
```

This only verifies `Cancelled { .. }` matches `Cancelled { .. }`. Does not exercise `dispatch_newly_ready`, `mark_descendants_failed`, or the event-emission paths. A fourth test (`test_plan_ready_to_merge_blocked_by_cancelled`) had an empty body — just a comment.

**Recovery:** After `request_changes` with explicit examples of real behavior tests, attempt 2 produced 3 high-quality integration tests that actually drive `run_plan` / `handle_review_task` and capture events. The worker is capable of real tests — the default heuristic was "satisfy the file requirement with the minimum to compile".

**Severity:** Medium. Approving this pass would have masked a real-behavior regression later.

**Worker:** gemini

---

### 2.4 Stash contention (codex DN-5 attempt 1)

**Observation:** Delegation pre-dispatch failed with `"Failed to snapshot brain state: failed to create stash after retries"`. No code executed; task immediately marked failed.

**Root cause:** When the brain (this Claude Code session) has uncommitted/untracked state in its main working tree, the orchestrator captures a git stash before spawning each worker so the worker can work from a consistent snapshot of the brain's intent-time state. When **multiple parallel dispatches race to create a stash on a dirty tree**, git serializes (index.lock) and the orchestrator's retry loop exhausts.

**Evidence:** At dispatch time, `git stash list` showed 12 pre-existing stashes; `git status` showed 4 modified + 7 untracked files. Four workers (up-1, dn-2, dn-4, dn-5) started dispatching simultaneously. First three succeeded; dn-5 lost the race.

**Pre-existing class:** `docs/rca/2026-04-19-parallel-execution-file-isolation.md` documents this class of failure.

**Severity:** High when it fires (loses the delegation) but recoverable — redispatching after serialization clears typically succeeds. The second dn-5 dispatch via `delegate_async` did not fail on stash but failed differently (§ 2.5).

**Worker:** codex (but this is orchestrator-level, not worker-level)

---

### 2.5 Silent no-op (codex DN-5 standalone `86bbef0a`, codex DN-6 attempt 1 `ce54abbf`)

**Observation:** Worker reports status `Success` (for standalone async) or `awaiting_review` (for plan task) — but the worker branch has **zero commits** on top of the brain snapshot base. The MCP `get_task_diff` for DN-6 returned `diff_status: "no_changes_detected"`. For the standalone DN-5, `git log worker-branch` showed only inherited commits from the brain snapshot + merge base; no worker-authored commit.

**Evidence:**
```
$ git log --oneline spur/worker-codex-baf5c574-...
fb3045e spur: brain snapshot              ← tip (orchestrator's snapshot)
e77a127 docs(licensing): ...              ← inherited
afd35a7 docs(licensing): ...              ← inherited
010a8e2 Merge feat/brain-worker-invariants: 6 brain↔worker integration invariants  ← inherited
...
```
No `fix(spur-core): DN-5 …` commit. The worker "completed" without doing any work.

**Root cause hypothesis:** codex's ACP session may have terminated early (agent disconnect, session timeout, context exhaustion) without surfacing an error, and the orchestrator treated "session ended cleanly without a diff" as success.

**Severity:** Critical. This is the most dangerous failure mode because it reports success. A naive reviewer trusting `status: Success` without pulling the diff would believe the task landed.

**Workers:** codex (both occurrences)

---

### 2.6 Plan completion blind spot — hung worker + lost-result symptom (claude-code-acp plan `bc2f9fd4`)

**Observation:** After submitting the 3-task leftovers plan to claude-code-acp, DN-6 v2 completed cleanly (approved). DN-2 v2 and DN-5 v2 fell into an inconsistent state:

- Plan status (`get_plan_status`) shows both as `dispatched` with `history_count: 0`.
- MCP polling tools (`check_delegation_status`, `cancel_delegation`) return `Unknown delegation: <uuid>` for both.
- File system shows:
  - DN-5 v2 worker branch (`57618bde`) has commit `e57c124 fix(spur-core): DN-5 — INV-1 replay-safety (orphan dispatch + dup warn)` with the exact expected 4-file diff (+154 lines).
  - DN-2 v2 worker branch (`19d112f0`) has no DN-2 commit — only the brain-snapshot inheritance.

In other words: DN-5 v2 **did the work and committed** but the plan never transitioned the task to `awaiting_review`. DN-2 v2 is **genuinely hung** — worker branch has no commits 21+ minutes after dispatch, worker is either dead or stuck in an infinite thinking loop.

**Post-merge correction:** the original "delegation-registry desync" framing was too strong. For `submit_plan` tasks, `get_plan_status` and `check_delegation_status` are backed by **different mechanisms by design**:
- `check_delegation_status` only consults the MCP server's `active_delegations` / `completed_delegations` maps used by direct `delegate_*` tools.
- `run_plan` advances plan tasks through per-task oneshot receivers (`DelegationRequest.respond_to` → `rx.await` / `spawn_completion_future`).

Therefore `Unknown delegation` is non-diagnostic here. The unresolved bug is narrower: for DN-5 v2, branch work existed on disk but the plan never observed a `DelegationResult`; for DN-2 v2, the worker appears hung and the plan had no liveness signal to distinguish "still working" from "stuck forever".

**Severity:** Critical. The brain cannot drive the task to completion through the normal plan workflow. The only recovery path is out-of-band (cherry-pick the worker branch if it has work, or implement manually if not).

**Workers:** claude-code-acp (but this is orchestrator-level, not worker-level — claude-code-acp did the work for DN-5, the plan completion path simply did not observe it)

---

## 3. Per-agent analysis

### 3.1 gemini (`@google/gemini-cli`)

**Strengths:** Multi-modal support (as advertised). Can produce correct production code when given very explicit instructions.

**Weaknesses in this run:**
- **Hallucinates completion signals** (§ 2.1). Summary claims work that isn't in the diff. This is the most dangerous pattern and was observed twice on DN-2.
- **Scope creep** (§ 2.2). When the task is hard, gemini attempts to "help" by editing adjacent files. The "clean up" reflex is inappropriate for a task-scoped delegation.
- **Tests satisfy the letter, not the spirit, of TDD** (§ 2.3). Default produces type-check tautologies that compile and pass without exercising behavior.

**Score:** 2 approvals out of 4 attempts (50%). Required `request_changes` with very explicit feedback to land DN-4. Failed twice on DN-2 despite detailed feedback.

**Recommendation:** Do not use gemini for:
- Refactors touching multiple files with strict scope boundaries.
- Tasks where summary/diff consistency is a correctness invariant (reviewers can't catch every diff).
- Test-writing without an explicit behavior-test pattern to mirror.

Safer uses: narrow single-file bugfixes with a clear spec, exploratory analysis where ambiguity is acceptable.

### 3.2 codex (`@zed-industries/codex-acp`)

**Strengths:** Clean red-then-green commit discipline when it does work (UP-4 landed with two commits: test commit, then fix commit). Produces small, focused diffs.

**Weaknesses in this run:**
- **Silent no-op on session termination** (§ 2.5). Observed on **both** DN-5 standalone and DN-6 attempt 1. Worker reports "success" with zero commits. This is the most dangerous failure mode in the entire catalog.
- **Stash contention amplifies its fragility** (§ 2.4). When dispatch pre-conditions fail, codex doesn't retry cleanly.

**Score:** 1 clean approval (UP-4), 2 silent no-ops (DN-5, DN-6). ~33% useful-work rate.

**Recommendation:** Do not use codex until the "session ends without surfacing error" mode is understood and fixed at the agent integration level. Even for narrow tasks it's risky because silent success is worse than loud failure.

### 3.3 claude-code-acp (`npx @anthropic-ai/claude-code`)

**Strengths in this run:**
- **Noticed a false assumption in the task brief** (DN-6 v2: the spec claimed DN-4 had landed on main, claude-code-acp verified against actual enum state and adapted).
- **Explicit, structured deviations** — flagged the Cancelled-arm omission and the pre-existing workspace errors, and classified them correctly as follow-up vs. out-of-scope.
- **Clean first-pass completion** on DN-6 v2 (2 files, exact expected scope).
- **Completed DN-5 v2 correctly on branch** — the 4-file diff at commit `e57c124` matches the spec; the orchestrator just lost track of it.

**Weaknesses observed:**
- **Hang risk on the hardest task (DN-2).** 21+ minutes with no commit. Hypothesis: the production retry site port is genuinely hard (event emissions + worktree cleanup must move into a closure with async + borrow gymnastics). claude-code-acp may have been mid-rewrite and either hit a compile loop or an ACP frame drop.
- Can't distinguish this from orchestrator state corruption § 2.6 in the current dispatch logs.

**Score:** 2 clean approvals (DN-5 on branch, DN-6 v2), 1 hang (DN-2 v2). Highest signal-to-noise ratio of the three workers.

**Recommendation:** Preferred worker for scoped Rust refactors. Pair with a **wall-clock budget** — if no commit after ~15 min on a task with expected scope ≤200 LoC, treat as hung and cancel/retry.

---

## 4. Orchestrator issues

### 4.1 Incorrect original hypothesis: delegation registry desync (§ 2.6)

A task can be simultaneously:
- `dispatched` per `get_plan_status`
- `Unknown delegation` per `check_delegation_status` / `cancel_delegation`

**Correction after code review:** this is **not sufficient evidence of corruption** for `submit_plan` tasks. The MCP polling registry and the plan executor are separate state machines:
- direct `delegate_to_worker` / `delegate_async` / `delegate_parallel` calls populate `active_delegations` / `completed_delegations`;
- `submit_plan` stores tasks in `active_plans` and waits on per-task oneshot receivers in `run_plan`.

So `Unknown delegation` here can be expected even when plan dispatch was real. The actual open failure surface is:
- `DelegationResult` was never constructed;
- it was constructed but never delivered via `respond_to.send(result)`;
- it was delivered but never applied to the matching `PlanTaskEntry`;
- or the worker was hung and no liveness channel surfaced that fact.

**Recommendation:** instrument and/or assert the **actual plan-task critical path**:
1. `DelegationResult` constructed in orchestrator
2. `respond_to.send(result)` attempted
3. plan-side receiver (`rx.await` / `spawn_completion_future`) applied the result

If any of those fail, stamp explicit failure into plan status and lineage. Do **not** treat "plan says dispatched, polling says Unknown delegation" as a root cause on its own.

### 4.2 Success-without-diff remains underclassified (§ 2.5)

When an ACP worker session ends with no diff produced, the orchestrator still largely classifies the result from `worker_success: bool`. That conflates two different realities:
- **legitimate no-change completion** (investigation / verification task, no edits required)
- **accidental silent no-op** (worker exited cleanly without doing the requested work)

This codebase already intentionally supports a no-change outcome via `get_task_diff` returning:
```json
{ "diff": null, "diff_status": "no_changes_detected" }
```

So a blanket invariant of "zero commits past base == failure" would be too strong.

**Recommendation:** add explicit outcome metadata or status split rather than a blanket hard-fail, for example:
- `changes_detected: bool`
- `commits_authored: u32`
- a dedicated status / flag distinguishing `success_no_change` from `suspected_silent_noop`

If heuristics are used, only escalate to failure when the task contract required code changes, or when summary / file evidence contradicts the no-change result.

### 4.3 Stash contention on parallel dispatch (§ 2.4)

Covered by existing RCA (`docs/rca/2026-04-19-parallel-execution-file-isolation.md`). Not re-duplicating.

### 4.4 No worker liveness signal

Once dispatched, there's no heartbeat or progress signal from the worker. A hung worker looks identical to a worker mid-thinking. The reviewer (brain) must guess at wall-clock timeout.

**Recommendation:** ACP session-level heartbeat at ≤60s intervals, with orchestrator-side hang detection after N missed heartbeats. Bonus: expose `last_progress_at` in `check_delegation_status`.

### 4.5 Post-merge regression: `Cancelled` dependency semantics split

The merged DN-4 / DN-6 hardening left the scheduler with **two different dependency-satisfied predicates**:
- `dispatch_newly_ready` treats `Approved | Cancelled` as dep-satisfied (the intended DN-4 behavior).
- `run_plan`'s main readiness scan still treats only `Approved` as dep-satisfied.

That means behavior now depends on which scheduling path is exercised:
- after a brain approval, a downstream task behind a cancelled dep can dispatch correctly;
- in the main executor loop, the same downstream task can remain `Pending` forever and later be stamped `Failed` by DN-6 terminal-exit cleanup (`"dep never satisfied"`).

**Recommendation:** extract a single helper such as `is_dep_satisfied(status)` and use it in both `run_plan` and `dispatch_newly_ready`. Add behavior tests for both paths:
- initial dispatch in `run_plan`
- re-dispatch after `handle_review_task(approve)`
- terminal-exit cleanup with cancelled predecessors

---

## 5. Evidence artifacts

### 5.1 Git branches relevant to this RCA

| Branch | Tip commit | Purpose | State |
|---|---|---|---|
| `spur/worker-gemini-d3657fe4-…` | UP-1 commit | gemini success | approved, awaits merge |
| `spur/worker-codex-75fb59f5-…` | UP-4 (2 commits: test+fix) | codex success | approved, awaits merge |
| `spur/worker-gemini-8266ccab-…` | DN-2 attempt 1 | hallucinated diff | rejected/request_changes |
| `spur/worker-gemini-aa6ccf84-…` | DN-2 attempt 2 | scope creep | rejected |
| `spur/worker-gemini-3cc616cd-…` | DN-4 attempt 1 | shallow tests | request_changes |
| `spur/worker-gemini-8b3014c4-…` | DN-4 attempt 2 | good tests + E0308 fix | approved, awaits merge |
| `spur/worker-codex-baf5c574-…` | (no worker commit, only brain-snapshot) | DN-5 standalone async silent no-op | abandoned |
| `spur/worker-codex-adce2440-…` | (no worker commit) | DN-6 attempt 1 silent no-op | rejected |
| `spur/worker-claude-code-acp-44fac60b-…` | DN-6 v2 commit | claude-code-acp success | approved, awaits merge |
| `spur/worker-claude-code-acp-57618bde-…` | `e57c124` DN-5 commit | claude-code-acp success (orchestrator lost track) | code DONE, orch state corrupt |
| `spur/worker-claude-code-acp-19d112f0-…` | (no DN-2 commit) | DN-2 v2 hung 21+ min | abandoned, manual impl needed |

### 5.2 Plan IDs

- Original plan: `03f8acda-6084-4962-b9bb-47144db3444e` — final state `has_failures` (3 approved, 2 rejected, 1 failed).
- Leftovers plan: `bc2f9fd4-f14a-41b3-99f8-e54eb1090c69` — final state `running` (1 approved, 2 stuck in `dispatched` despite file-system evidence of completion for DN-5).

### 5.3 Specific error messages captured

**Stash contention:**
```
"Failed to snapshot brain state: failed to create stash after retries"
```

**MCP polling symptom (non-diagnostic for `submit_plan` tasks):**
```
MCP error -32602: Unknown delegation: e3f7c8c2-1d07-435a-8ac5-bd2a2cd72337
MCP error -32602: Unknown delegation: 103a0fd4-2c87-4385-9da6-34cea6f22e6e
```

**Empty diff on silent no-op:**
```
diff: null
diff_status: "no_changes_detected"
```

---

## 6. Root-cause hypothesis tree

Rooted at the failure to reliably execute a 6-task plan through external workers:

```
Failure to land 6/6 tasks reliably
├── Agent-level
│   ├── gemini: hallucinates summary/diff (§2.1, §2.3)
│   ├── gemini: violates scope boundaries (§2.2)
│   ├── codex: terminates sessions without surfacing errors (§2.5)
│   └── claude-code-acp: hangs on hard tasks without heartbeat (§2.6 DN-2 side)
└── Orchestrator-level
    ├── Concurrent-stash contention on dirty brain trees (§2.4)
    ├── Plan-task result delivery / observation blind spot (§2.6, §4.1)
    ├── Success-without-diff underclassified (§4.2)
    ├── No worker liveness signal (§4.4)
    └── Split `Cancelled` dependency semantics after hardening merges (§4.5)
```

The agent-level issues are partially mitigable with better prompts and reviewer discipline. The orchestrator-level issues require code changes in spur itself.

---

## 7. Recommendations (ordered by ROI)

### Must-fix before relying on multi-worker plans again:

1. **Plan executor: unify dependency satisfaction semantics.** `Cancelled` must mean the same thing in the main `run_plan` scheduler and in `dispatch_newly_ready`. Today they diverge, which can strand downstream tasks behind cancelled deps and later fail them incorrectly.

2. **Orchestrator/MCP: instrument the real plan-task completion path.** Add explicit observability around:
   - result constructed
   - `respond_to.send(result)` attempted / succeeded
   - plan-side receiver applied result

   This is the actual critical path for `submit_plan` tasks; the MCP polling registry is not.

3. **Orchestrator: explicit no-change classification instead of blanket zero-commit failure.** Preserve legitimate no-op outcomes while surfacing suspected silent no-ops distinctly enough that reviewers do not confuse them with successful code-producing runs.

4. **Orchestrator: worker heartbeat + hang detection.** Require any dispatched worker to emit a heartbeat event every N≤60s; orchestrator marks as hung after 3 missed heartbeats; auto-retry or surface to brain with a clear `hung` status.

### Should-fix for dispatch quality:

5. **Dispatch prompt hardening** — when describing a task to the worker, explicitly enumerate the file allow-list in machine-checkable form (e.g. JSON array of paths). Enforce via a pre-commit hook in the worker's execution sandbox that blocks `git add` of files outside the allow-list.

6. **Reviewer discipline codification** — the RCA's observed pattern is that the three-step check is *"pull diff → compare summary to diff → pull reviewer spec"*. Codify into a review checklist that is enforced before any `review_task(approve)`:
   - Diff matches summary's file list?
   - Diff stays within task's declared scope?
   - Tests exercise behavior (not just types)?

7. **Worker-agent selection matrix** — surface the "good_for" / "avoid_for" advertised in `list_available_workers` at plan-submission time. Refuse dispatches that violate the advertised constraints (e.g. codex for multi-file refactors per its own description).

### Nice-to-have:

8. **Worker-branch lineage dashboard** — a TUI view showing all active worker branches, their expected scope (from task spec), and their actual diff shape. Would have surfaced the DN-5 v2 done-on-disk case immediately without manual `git log` / `git diff --stat`.

9. **Record this RCA's evidence in the product's observability pipeline** — specifically, add a lineage event `WorkerSessionEnded { delegation_id, commits_authored, files_touched, reported_status }` so future silent-no-op cases are detectable from logs alone.

---

## 8. Closing notes

- **Phase 3a outcome:** 5 of 6 tasks land (UP-1, UP-4, DN-4, DN-5 on worker branch, DN-6 v2). DN-2 will be implemented manually on a side branch after this RCA is written, then all 6 pieces can be integrated in a single merge pass.
- **Aggregate worker-dispatch success rate for this run:** 5/11 approvable attempts = **45%**. If we count silent no-ops as high-severity failures (they are), the picture is worse. This is not yet a reliable automation surface.
- **claude-code-acp is the only worker that consistently produces reviewable, truthful diffs** in this run. Gemini and codex both exhibited safety-critical failure modes.
- **Post-merge correction:** the strongest unresolved orchestrator problem is no longer best described as "delegation registry desync". The sharper description is: **plan-task completion and worker liveness remain under-observed**, and current evidence is consistent with a lost-result blind spot plus hang detection gap.

The work is worth it when it works — UP-1 and UP-4 together took ~10 min of wall clock and produced clean PRs. But the review burden and the failure recovery cost (stashing, redispatching, manual cherry-picks) currently exceeds the gain for plans under ~10 tasks. Re-evaluate after the orchestrator-level fixes above land.

---

## 9. Addendum — post-merge re-review on current `HEAD`

Hardening that has since landed:
- `fix(spur-core): emit DelegationCompleted on cancel path`
- `feat(spur-mcp): DN-4 — PlanTaskStatus::Cancelled with non-cascading semantic`
- `fix(spur-mcp): DN-6 — mark non-terminal tasks Failed on run_plan exit`
- `refactor(spur-core): DN-2 — extract RetryLoop combinator`

What this changes about the RCA:
- The original §4.1 hypothesis ("atomic plan-status ↔ delegation-registry updates") should be treated as superseded. It misidentified the state machine relevant to `submit_plan`.
- The no-liveness finding in §4.4 remains valid and arguably becomes more important now that the obvious cancel-path and terminal-exit bugs are fixed.
- A new correctness issue is present in current code: `Cancelled` dependency semantics are split between the main scheduler and the post-review dispatch helper (§4.5).

Validation note:
- A fresh `cargo test -p spur-mcp plan -- --nocapture` run on current `HEAD` did **not** reach the plan tests because `crates/spur-mcp/tests/rmcp_streamable_http.rs` still constructs `McpCallbackServer::new(&SessionId, ...)` while the constructor now expects `&BrainSessionId`. That test compile failure should be fixed before treating the Phase 3a hardening as fully verified.
