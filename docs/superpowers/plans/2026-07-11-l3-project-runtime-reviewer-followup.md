# L3 Project Runtime Reviewer Follow-up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox
> (- [ ]) syntax for tracking.

**Goal:** Finish the WIP L3 project runtime with an authenticated independent reviewer
worker, non-overlapping shutdown drain, and explicit-only migration of legacy
brain-owned L3 generations.

**Architecture:** Stack the change on WIP commit 203c1b835. Keep maker task execution
in the existing system-L3 reconciler, add a durable companion-issue protocol for
reviewer delegations, and translate authenticated reviewer verdicts through the
existing non-advisory review transition. Move the leadership guard into a drain task
when shutdown exceeds its caller deadline, and replace historical auto-adoption with
an explicit force_reclaim_plan target.

**Tech Stack:** Rust 2021, Tokio, async-trait, beads through spur-pm, existing
DelegationRequest and plan projector, scripts/spur-cargo.

---

## Starting Point and Constraints

- Start from branch
  spur/brain/l3-runtime-default-followup-20260711, which contains the rejected WIP
  plus the approved follow-up design.
- Read
  docs/superpowers/specs/2026-07-11-l3-project-runtime-reviewer-followup-design.md
  and the original 2026-07-10 design before editing.
- Use code-explore before navigation, systematic-debugging for any unexpected
  failure, test-driven-development for every behavior phase, worker-signals for scope
  changes, and verification-before-completion before the final report.
- Always use scripts/spur-cargo, never bare cargo.
- Do not touch unrelated user files or merge the branch.
- Preserve the current WIP's system-owner, lock, exact-generation, scheduler-scope,
  and no-brain behavior.
- The known untouched-base full-suite exception is
  tool_schemas::tests::submit_loop_params_schema_is_fully_inlined. Do not modify
  tool_schemas.rs merely to fix that unrelated failure. If it no longer fails, report
  that fact; if any additional test fails, treat it as a regression.

## File Map

- Modify crates/spur-core/src/plan/audit_sentinel.rs: durable review dispatch and
  verdict variants plus round-trip tests.
- Modify crates/spur-core/src/plan/labels.rs: companion review labels and parsers.
- Create crates/spur-core/src/mcp/review_verdict.rs: reviewer-only verdict tool and
  authorization checks.
- Modify crates/spur-core/src/mcp/mod.rs: register the verdict tool only in the worker
  registry.
- Modify crates/spur-core/src/worker_server.rs only if the worker registry needs the
  existing WorkerCallContext routed to the new module.
- Replace crates/spur-core/src/plan/reconciler/reviews.rs: review eligibility,
  companion issue lifecycle, dispatch, recovery, and verdict consumption.
- Modify crates/spur-core/src/plan/reconciler/mod.rs: narrowly expose dispatch or PM
  helpers required by reviews.rs.
- Modify crates/spur-core/src/plan/reconciler/tests.rs: protocol, signal, retry, and
  recovery tests.
- Modify crates/spur-core/src/orchestrator/loop_runtime.rs: shutdown drain ownership
  and leadership handoff tests.
- Modify crates/spur-core/src/plan/reconciler/ownership.rs: delete historical
  auto-adoption.
- Modify crates/spur-core/src/server/handlers/plan.rs and
  crates/spur-core/src/mcp/plan.rs: explicit system_l3 force-reclaim target.
- Modify crates/spur-core/tests/l3_project_runtime.rs: real no-brain maker/reviewer
  flow.
- Modify docs/loops.md: independent review, shutdown drain, and manual migration
  operations.

### Task 1: Durable Reviewer Identity and Authenticated Verdict Tool

- [ ] **Step 1: Add failing audit and authorization tests**

In audit_sentinel.rs, add round-trip and stable-kind tests for these exact concepts:

~~~rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemReviewDecision {
    Approve,
    RequestChanges,
}

AuditSentinelKind::SystemReviewDispatch {
    plan_id,
    task_id,
    attempt,
    maker_delegation_id,
    reviewer_delegation_id,
    review_issue_id,
}

AuditSentinelKind::SystemReviewVerdict {
    maker_delegation_id,
    reviewer_delegation_id,
    review_issue_id,
    decision,
    feedback,
    evidence,
}
~~~

In a test module beside the new MCP module, build a beads-backed target issue with a
SystemReviewDispatch audit and verify:

1. the recorded reviewer delegation can submit approve;
2. the maker delegation receives Unauthorized;
3. a stale reviewer receives Unauthorized after a newer dispatch;
4. repeating an identical verdict succeeds without adding a second conflicting fact;
5. a different second verdict is rejected.

- [ ] **Step 2: Run RED tests**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::audit_sentinel::tests::system_review -- --nocapture
scripts/spur-cargo test -p spur-core --lib mcp::review_verdict::tests -- --nocapture
~~~

Expected: compile or assertion failures because the variants and module do not exist.

- [ ] **Step 3: Commit the RED tests**

~~~bash
git add crates/spur-core/src/plan/audit_sentinel.rs crates/spur-core/src/mcp
git commit -m "test(spur-core): L3.r1 cover reviewer verdict binding"
~~~

- [ ] **Step 4: Implement the durable variants and tool**

Add the audit variants and SystemReviewDecision with the same sentinel version and
encoding path used by existing dispatch/completion/approval audits.

Add review companion label helpers whose values remain br-legal:

~~~rust
pub const SYSTEM_REVIEW: &str = "spur:system-review";

pub fn review_target(issue_id: &str) -> String;
pub fn review_maker_delegation(delegation_id: &str) -> String;
pub fn review_reviewer_delegation(delegation_id: &str) -> String;
~~~

Implement submit_review_verdict with this schema:

~~~json
{
  "type": "object",
  "required": ["target_issue_id", "decision", "feedback", "evidence"],
  "properties": {
    "target_issue_id": {"type": "string"},
    "decision": {
      "type": "string",
      "enum": ["approve", "request_changes"]
    },
    "feedback": {"type": "string", "minLength": 1},
    "evidence": {
      "type": "array",
      "items": {"type": "string", "minLength": 1},
      "minItems": 1
    }
  }
}
~~~

The handler must list target comments, select the latest unconsumed
SystemReviewDispatch, compare its reviewer_delegation_id to
WorkerCallContext.delegation_id, revalidate the maker completion, verify the companion
issue labels, and append the verdict. It must not close the companion while the
reviewer process is still unwinding. Do not add a signal-prefixed label for verdicts.
Register this tool in worker_tool_registry but not the brain/catalog registry.

- [ ] **Step 5: Run GREEN tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::audit_sentinel::tests::system_review -- --nocapture
scripts/spur-cargo test -p spur-core --lib mcp::review_verdict::tests -- --nocapture
git add crates/spur-core/src/plan/audit_sentinel.rs crates/spur-core/src/plan/labels.rs crates/spur-core/src/mcp crates/spur-core/src/worker_server.rs
git commit -m "feat(spur-core): L3.r1 authenticate reviewer verdicts"
~~~

Expected: all selected tests pass.

### Task 2: Replace Metadata Auto-Approval with Reviewer Dispatch

- [ ] **Step 1: Add failing reconciler tests**

Add focused tests that project real audit comments and assert:

- a successful maker completion causes one companion review issue and one reviewer
  DelegationRequest whose ID differs from the maker;
- the reviewer request base is the maker worker branch;
- a MarkNoop signal remains eligible;
- an unresolved non-MarkNoop signal prevents dispatch;
- a processed historical signal does not block solely because the signal label
  remains;
- a durable approve verdict calls the existing approval transition and releases a
  dependent task;
- request_changes records feedback and makes the same maker branch reusable;
- a reviewer result without a verdict does not approve;
- replay after a simulated crash creates no second live review;
- three expired reviewer attempts park the target with a review-failed audit/signal.

Delete or rewrite checker_accepts_only_non_superseded_success_completion: completion
metadata alone must now be insufficient.

- [ ] **Step 2: Run RED tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::reconciler::reviews::tests -- --nocapture
scripts/spur-cargo test -p spur-core --lib plan::reconciler::tests::system_l3_review -- --nocapture
git add crates/spur-core/src/plan/reconciler
git commit -m "test(spur-core): L3.r2 require independent review"
~~~

Expected: failures show the current direct Approval write and prefix-only signal check.

- [ ] **Step 3: Implement review eligibility**

Replace the label-prefix branch with a helper that derives unresolved signals from
audit and signal comments:

~~~rust
fn system_review_eligibility(
    task: &PlanTaskEntry,
    audits: &[AuditSentinelKind],
    signals: &[WorkerSignal],
    processed_signal_ids: &HashSet<String>,
) -> Result<ReviewIdentity, ReviewBlockReason>;
~~~

ReviewIdentity must include plan ID, task ID, attempt, maker delegation ID, maker
branch, and target issue ID. MarkNoop is non-blocking. Every other unresolved or
unknown signal is blocking. The helper must reject stale, superseded, failed, or
branchless completions.

- [ ] **Step 4: Implement companion issue and reviewer dispatch**

Before send:

1. derive the deterministic maker review identity;
2. find or create one unparented companion task with SYSTEM_REVIEW and link labels;
3. persist SystemReviewDispatch on the target;
4. persist normal dispatch intent/lease on the companion issue;
5. send a DelegationRequest based on BaseSpec::Branch using the maker branch.

Use the maker task's agent/profile/model/effort/config overrides. Build a read-only
prompt containing the acceptance criteria, maker delegation and branch, relevant
signals, exact get_task_diff instructions, and the mandatory submit_review_verdict
call. Set issue_id to the companion review issue and keep worker MCP enabled.

Track the result receiver using ReconcilerDispatch::track_task. A result without a
matching durable verdict reopens the companion only after the review lease is stale;
it must not write Approval. A result with a verdict is allowed to contain no code diff;
the reviewer branch is never integrated. After consuming the verdict, close the
companion issue idempotently so a late generic result update cannot leave it open.

- [ ] **Step 5: Consume verdicts through existing review transitions**

For a valid latest verdict, wrap the fresh projected PlanState in the synchronization
type required by handle_review_task_with_write_mode and call it with
ReviewWriteMode::NonAdvisory:

~~~rust
let (decision, reuse_prior_worktree) = match verdict.decision {
    SystemReviewDecision::Approve => ("approve", false),
    SystemReviewDecision::RequestChanges => ("request_changes", true),
};
~~~

Pass the reviewer feedback. Revalidate the maker delegation immediately before the
call. Preserve existing MAX_ATTEMPTS behavior and emit PlanTaskReviewed with feedback
that names the reviewer delegation.

- [ ] **Step 6: Run GREEN suites and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::reconciler::reviews::tests -- --nocapture
scripts/spur-cargo test -p spur-core --lib plan::reconciler:: -- --nocapture
git add crates/spur-core/src/plan/reconciler crates/spur-core/src/plan/mod.rs
git commit -m "feat(spur-core): L3.r2 dispatch independent reviewers"
~~~

Expected: direct metadata approval is gone and all reconciler tests pass.

### Task 3: Hold Leadership Through Abort Acknowledgement

- [ ] **Step 1: Add failing handoff tests**

Add a fake ProjectLoopRuntimeInstance whose graceful phase finishes but whose drain
future waits on a Notify. Start two supervisors against one lock and assert:

1. supervisor A reaches LeaderDraining;
2. B cannot start its runtime after A's public shutdown deadline;
3. releasing the drain lets exactly one B runtime start.

Strengthen runtime_shutdown_aborts_a_delegation_child_after_the_grace_bound so the
test observes the child JoinHandle completion, not only shutdown return.

- [ ] **Step 2: Run RED tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib orchestrator::loop_runtime::tests::standby_waits_for_leader_drain -- --nocapture
scripts/spur-cargo test -p spur-core --lib orchestrator::loop_runtime::tests::runtime_shutdown_awaits_aborted_child -- --nocapture
git add crates/spur-core/src/orchestrator/loop_runtime.rs
git commit -m "test(spur-core): L3.r3 fence leadership during drain"
~~~

Expected: the standby currently acquires after shutdown returns while a child may
remain unfinished.

- [ ] **Step 3: Implement a guard-owning drain**

Change the runtime shutdown contract to return a drain object that owns every
unfinished child/server completion future after the grace deadline. A representative
shape is:

~~~rust
struct ProjectLoopRuntimeDrain {
    completion: futures::future::BoxFuture<'static, ()>,
}

#[async_trait]
trait ProjectLoopRuntimeInstance {
    async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain;
    async fn wait_for_exit(&mut self);
}
~~~

The real runtime requests cancellation and waits through RUNTIME_SHUTDOWN_TIMEOUT.
After timeout it aborts but moves each JoinHandle into the drain future and awaits it
there. Do not use yield_now plus is_finished as acknowledgement.

The supervisor moves the ProjectLoopRuntimeLeadershipGuard into the same detached
drain task. It may signal public shutdown completion at the deadline, but the guard is
dropped only after drain completion. Keep the supervisor from starting another local
runtime while a drain is alive.

- [ ] **Step 4: Run GREEN tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib orchestrator::loop_runtime:: -- --nocapture
git add crates/spur-core/src/orchestrator/loop_runtime.rs
git commit -m "fix(spur-core): L3.r3 retain leadership through drain"
~~~

Expected: all runtime tests pass, including delayed drain handoff.

### Task 4: Remove Auto-Adoption and Add Explicit System Migration

- [ ] **Step 1: Add failing ownership and tool tests**

Replace the stale historical adoption test with:

- repeated SystemL3Only ticks never mutate a brain-owned L3 epic, regardless of lease
  presence or age;
- force_reclaim_plan with target_owner=system_l3 and confirm=true migrates an open L3
  epic, removes old owner/lease/token labels, installs system owner plus fresh fencing,
  and writes PlanForceReclaimed;
- system_l3 target rejects an L1/L2 or non-generation plan;
- missing confirm is rejected;
- default/current_brain behavior remains unchanged.

- [ ] **Step 2: Run RED tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::reconciler::tests::system_l3_runtime_never_adopts_legacy_owner -- --nocapture
scripts/spur-cargo test -p spur-core --lib server::handlers::plan_tests::force_reclaim_plan -- --nocapture
git add crates/spur-core/src/plan/reconciler crates/spur-core/src/server crates/spur-core/src/mcp/plan.rs
git commit -m "test(spur-core): L3.r4 cover explicit legacy migration"
~~~

- [ ] **Step 3: Remove automatic historical adoption**

In SystemL3Only ownership reconciliation, retain only listing and lease refresh for
epics already owned by LOOP_RUNTIME_OWNER_ID. Delete the scan that installs grace on
foreign owners and the time-based adopt_expired_l3_epic path. Keep helpers used by
explicit migration in an appropriate ownership module rather than dead code.

- [ ] **Step 4: Extend force_reclaim_plan**

Add target_owner to the tool schema with current_brain as the compatibility default
and system_l3 as the only new value. For system_l3, validate open status, L3 autonomy,
generation labels, and explicit confirmation. Use one PM update to replace owner,
owner-token, and lease labels, then append PlanForceReclaimed with the supplied reason.
Return new_owner=spur-loop-runtime.

- [ ] **Step 5: Run GREEN tests and commit**

~~~bash
scripts/spur-cargo test -p spur-core --lib plan::reconciler:: -- --nocapture
scripts/spur-cargo test -p spur-core --lib server::handlers::plan_tests -- --nocapture
git add crates/spur-core/src/plan/reconciler/ownership.rs crates/spur-core/src/server/handlers/plan.rs crates/spur-core/src/mcp/plan.rs
git commit -m "fix(spur-core): L3.r4 require manual legacy migration"
~~~

### Task 5: No-Brain End-to-End Review Flow and Documentation

- [ ] **Step 1: Rewrite the integration test as RED**

Extend due_l3_generation_runs_without_an_active_brain_session to capture real
DelegationRequests. For each of T1 and T2:

1. complete the maker with a committed worker branch;
2. observe a distinct reviewer request based on that branch;
3. submit an authenticated approve verdict using the reviewer's WorkerCallContext;
4. wait for the target approval before expecting its dependent maker.

Assert one maker and one reviewer per attempt, no BrainSession construction, terminal
plan state, and one LoopRun. Add a MarkNoop variant and a two-supervisor duplicate
review assertion if those are not already fully covered by unit tests.

- [ ] **Step 2: Run RED integration and commit**

~~~bash
scripts/spur-cargo test -p spur-core --test l3_project_runtime -- --nocapture
git add crates/spur-core/tests/l3_project_runtime.rs
git commit -m "test(spur-core): L3.r5 exercise no-brain review"
~~~

- [ ] **Step 3: Complete integration wiring**

Make only the production changes needed for the integration test. Do not reintroduce
an auto-approval shortcut in test helpers. Ensure the stable system identity owns both
maker and reviewer dispatch plumbing without constructing an ACP brain connection.

- [ ] **Step 4: Update documentation**

Rewrite the L3 Project Runtime Leadership section in docs/loops.md:

- separate reviewer worker and authenticated verdict;
- MarkNoop versus unresolved blocking signals;
- guard-owning LeaderDraining handoff;
- no automatic legacy adoption;
- exact force_reclaim_plan example:

~~~json
{
  "plan_id": "<legacy-plan-id>",
  "target_owner": "system_l3",
  "confirm": true,
  "reason": "former owner stopped; migrate to project runtime"
}
~~~

State clearly that new L3 execution needs no active owner BrainSession, while manual
migration itself is an operator action.

- [ ] **Step 5: Run focused GREEN verification and commit**

~~~bash
scripts/spur-cargo test -p spur-core --test l3_project_runtime -- --nocapture
scripts/spur-cargo test -p spur-core --lib plan::reconciler:: -- --nocapture
scripts/spur-cargo test -p spur-core --lib orchestrator::loop_runtime:: -- --nocapture
git add crates/spur-core docs/loops.md
git commit -m "feat(spur-core): L3.r5 complete reviewed autonomy"
~~~

## Final Verification

- [ ] **Step 1: Format**

~~~bash
scripts/spur-cargo fmt --all
git diff --check
~~~

Expected: both exit 0.

- [ ] **Step 2: Run the complete crate suite**

~~~bash
scripts/spur-cargo test -p spur-core
~~~

Expected: exit 0, or the sole failure is the independently known untouched-base
tool_schemas::tests::submit_loop_params_schema_is_fully_inlined failure. No new failure
is acceptable.

- [ ] **Step 3: Run remote clippy**

~~~bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core --all-targets -- -D warnings
~~~

Expected: exit 0 with no warnings.

- [ ] **Step 4: Inspect scope and history**

~~~bash
git status --short
git diff 203c1b835174500f424af6976d8a09c98780e050 --stat
git log --oneline 203c1b835174500f424af6976d8a09c98780e050..HEAD
~~~

Expected: only files named in this plan or directly required registration modules;
RED/GREEN commits remain visible; no user-owned files appear.

- [ ] **Step 5: Final worker report**

Report:

- each commit and intent;
- exact test/clippy commands with exit codes and counts;
- evidence that the reviewer delegation differs from the maker;
- evidence that a delayed drain retains leadership;
- evidence that legacy ownership changes only after explicit migration;
- any remaining limitation.

Do not merge, close the beads issue, or claim completion without the verification
outputs.
