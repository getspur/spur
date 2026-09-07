# Spur plan-state regression: investigation and repair

## Finding

The source-level regression was introduced by `f0f765176a27969df78e38dcb63bde36dd034e1a`
(`fix(spur-pm): bd-1h6 inherit parent blockers in graph planning`), committed
2026-08-19 22:34:43 +07:00. Its first parent is
`d285b9ea9e5424601c0df16605eeff78510fdf77`.

The adapter correctly changed `Issue.blocked_by` from loaded structural dependency
edges to the effective **active blocker** cache. Plan consumers still interpreted
that field as permanent relationships. An unblocked parent epic and an approved,
closed prerequisite therefore disappeared from those consumers' input.

Local tags `v1.20.0` and `v1.20.1` contain the previous adapter behavior.
`v1.21.0` is the first locally available tag containing the introducing commit;
the observed runtime reported `1.23.0`. This attribution is based on exact Git
patches, ancestry, unchanged consumer code across the introducing commit, and
the pinned Beads implementation—not a compiled whole-system bisect. The exact
running binary Git SHA and installation time were not established.

## Observed incident

Otobank plan `30064a3b-9649-4de5-9faf-005dbe12f6ab`, epic `bd-2851`,
submitted 113 tasks. Beads events 8029–8139 show 111 pending-task membership labels
removed between **2026-09-07 01:39:25.296589 and 01:39:25.543941 UTC**
(08:39:25 +07:00). This is the first observed corruption for that plan,
not a claim about the first occurrence anywhere.

The persisted task/dependency records remained present. The label-based projection
shrunk from 113 tasks to two and then one, allowing a false one-task completion.
A separate terminal-cache behavior retained that truncated result.

Read-only evidence inspected in the Otobank workspace:

- `.spur/events/59236-1788743758729-0.ndjson:746`: submission.
- `.spur/events/59236-1788745634975-1.ndjson:2064`: two-task projection.
- `.spur/events/59236-1788747128014-3.ndjson:795`: one-task projection.
- `.spur/events/59236-1788747128014-3.ndjson:1240`: false completion.
- `.spur/logs/spur.log.2026-09-07-59236:44345`: unknown-task review.
- Beads dependency rows versus active blocker cache, inspected read-only.

Malformed brain-authored audit comments appeared later and caused parse warnings;
they were not the initial trigger. Two runtimes sharing the Otobank workspace are
a recovery risk, but available actor records do not attribute individual removals
to a specific PID.

## Repair scope

| Consumer | Incorrect assumption | Repair |
| --- | --- | --- |
| Reconciler ownership recovery | Active blockers identify parent epics | Read direct typed parent-child edges and durable parent audits; preserve own-audit precedence; reject ambiguity |
| Epic execution derivation | A child's active blockers always contain the epic | Discover direct structural children |
| Persisted dependency projection | Active blockers preserve approved prerequisites | Retain structural execution dependencies and worker lineage |

The intended effective-blocker scheduling semantics are preserved. This repair
does not revert `f0f7651` or change the public meaning of `Issue.blocked_by`.

## TDD and bounded solver evidence

The work is tracked by `bd-3kbas` (membership) followed by dependent `bd-1f5lm`
(children and prerequisite lineage), using isolated SPUR Codex workers with the
requested `gpt-5.6-sol` / `xhigh` configuration.

Membership test commit: `8dee0b3433345bbc70eeaa418d3fe9fc3cab79c0`.
Membership fix commit: `00d7797db35eb9b8c31df30db714db5168a1f4b6`.

The real-Beads membership test failed before the fix with `[]` versus expected
`["PLAN-A"]` (exit 101) and passed afterward (exit 0). Independent remote
`reconciler_tick` results moved from 31 passed / 8 failed (39 tests) at baseline
`210e1636` to 34 passed / 6 failed (40 tests) after membership repair.
All six residual failures were traced to the two remaining consumers above.

Broader independent unit coverage also caught a test-double compatibility gap:
`--lib plan::` at the original baseline yielded 559 passed / 2 failed (561 tests),
whereas the first membership patch yielded 529 passed / 33 failed (562 tests).
The shared `MockPm` does not implement the structural-graph query and flattens
parent ownership into `blocked_by`. Accurate typed graph support in that test
double is included in the follow-up; the 31 additional failures are not mislabeled
as baseline failures. The two original failures concern overlay prediction and
closed external dependency validation.

Membership solver checks used the implemented `data_integrity.mutually_consistent`
rule across 27 finite combinations of own audit, structural parent, and effective
blocker in `{none, A, B}`, split to respect the 64-variable guard.
PRE and POST passed the intended policy; the old witness failed, with immediate
`get_solve_result` readbacks. These are bounded policy checks, not proof of all
Rust executions.

The first follow-up attempt used test commit `08601963f` and fix `08bf20d5`.
Its `reconciler_tick` and `plan_projection` integrations passed 40/40 and 4/4,
but independent review rejected the candidate: an active prerequisite outside
the plan was dropped by the plan-scoped graph reconstruction. The real-Beads
`legacy_projection_keeps_active_external_blockers_pending` check passed at
`00d7797d` and failed at the exact candidate tree (`[]` versus the external ID).
That RED review test is retained in commit `18722b892`.
The full plan-unit slice on that exact rework base was 531 passed / 34 failed
(565 tests), despite the green integration subset. The remaining mock graph gap
also affected three truncate/restart scenarios. These results were retained as
the rework comparison baseline rather than excluded from acceptance.

A compact additional solver witness includes the previously omitted scope
dimension: outside-plan, active prerequisite, absent from the in-plan graph.
Dropping its projected dependency fails (`sol_567594d3e54d47a2`); retaining the
gate passes (`sol_f07939f8c3bd421a`). Both were immediately reloaded. This is one
finite witness, not a universal implementation proof.

The same Beads issue was explicitly returned for focused rework on top of
`18722b892`, including the shared mock gap and restoration of the historical
closed-external validation case. The rework's test-only commit is
`00e25910a3f69fdc65bb1d46b17553fd1d36aeb9`: its final RED slice had 17 passed
and four expected failures, covering the extended TaskSpec external gate,
closed-external validation, and mock edge creation/removal behavior. The
separately committed real-Beads external-gate regression also failed again.

The expanded rework PRE solver checks failed the old witness
(`sol_d868085ae9934854`) and passed the intended external-gate and closed-status
contract (`sol_afab410ea57b4b85`); immediate readbacks confirmed both outcomes.
Rework fix `2d327593df8e512cb1f96bff0394b65c7159bbeb` passed all 45 integration
tests (external gate 1, projection 4, reconciler 40). POST intended contract
`sol_0cd4f938a4f44c10` passed; the omitted-gate witness
`sol_b2aa5dea3f574676` failed as expected. Both 55-variable results were
immediately reloaded. The public active-blocker and closed-external status
contracts remained unchanged.

During rework, the full plan-unit slice first improved to 560 passed / 9 failed.
The remaining failures identified two forwarding test wrappers, outdated
parent-as-blocker assertions, and a loop fixture using the same obsolete child
lookup. Updating those fixtures to the structural contract produced 569 passed /
0 failed before the final integration review. A further independent real-Beads
test caught canonical-ID duplication: durable task ID `T1` plus the active Beads
ID for that task projected to `["T1", "T1"]`, not `["T1"]`. The combined review
target returned 1 passed / 1 failed (exit 101) on `2d327593`. This RED test is
retained in `2c2398892ff1017e30fa69f1e100f25ec09a27c5`. The returned worker was
explicitly reviewed and the same issue dispatched for a final narrow
post-mapping normalization correction. The final code fix is
`957a446c5d88e04040e59f8fb5db1535bda98e88`: normalize and deduplicate after
mapping dependency aliases to canonical task IDs.

The identity rework's worker-local PRE uniqueness check
`sol_466bf5124e2e4135` rejected two active aliases sharing one canonical key;
its persisted result was immediately reloaded. The worker then independently
reproduced the exact Rust RED (0 passed / 1 failed / 1 filtered, exit 101).
Remote sync attempts initially exited 30 before Cargo, and were recorded as
infrastructure failures, not test failures. Read-only checks showed healthy VM
and SSM status but contention on the shared source-reclamation lock. A longer,
per-invocation sync timeout allowed the unchanged test to run after the lock
queue cleared. No remote service was restarted or configuration persisted.

The focused Rust test then passed (1 passed / 0 failed / 1 filtered, exit 0).
POST uniqueness receipt `sol_91574f0024f34d53` passed for the retained canonical
row and was immediately reloaded. This is a finite one-key example, not proof
for arbitrary Rust inputs.

Worker final acceptance passed: 569 plan unit tests, plus 46 integration tests
(external/identity review 2, projection 4, reconciler 40), all exit 0. Formatting
and diff checks also exited 0. The brain confirmed that its independent review
worktree exactly matched the final code commit and separately reproduced all
569 passing plan unit tests and all 46 integration tests (615 total), with
formatting and diff checks also passing. The review branch was fast-forwarded
to that exact tested commit; no implementation change was made afterward.

Final Beads approval is recorded for `bd-1f5lm` as JSON-only audit 4916; the
membership task's earlier approval is audit 4882. The membership epic
`bd-3gci2` was closed only after its actual structural child was confirmed
closed. An earlier handoff approval note (4915) accidentally included prose
after its JSON; it is informational/malformed, and was explicitly corrected by
the valid JSON-only approval and separate explanatory comment. It was not
treated as the approval evidence.

Final acceptance commands (configured remote builder, no local fallback):

```sh
scripts/spur-cargo test -p spur-core --lib plan::
scripts/spur-cargo test -p spur-core --test plan_external_blocker_review --test reconciler_tick --test plan_projection
scripts/spur-cargo fmt -- --check
git diff --check
```

Nested-worktree verification supplied `SPUR_NOTEBOOK_REPO` pointing at the
existing sibling Spur notebook checkout and `SPUR_NO_LOCAL_FALLBACK=1`; the
final independent run also used `SPUR_RSYNC_IO_TIMEOUT=300` for source sync.
These were per-command settings, not persisted configuration changes.

Handoff branch: `fix/bd-3kbas-structural-plan-state`.

## Operational boundary

No original Otobank task, plan, approval, retry branch, configuration, or runtime
was repaired or restarted. No deployment or push was performed. The source repair
does not itself invalidate an already-terminal cache, recover all pre-existing
labels, or add a terminal-cardinality guard. Those need a separately authorized
recovery/hardening operation.

The old Spur service also removed the newly submitted membership repair task's
plan label. The brain verified its actual completion audit, diff, tests and
solver receipts, then recorded explicit Beads approval using the supported
`approval` audit after `review_task` returned `unknown task`. The dependent task
was dispatched directly through SPUR against the exact reviewed commit,
not by creating duplicate plans or trusting the empty projection.

The candidate branch preserves the original test-then-fix commits even though the
worker runtime normalized its output branch to an identical-tree squash.
Unrelated pre-existing main-worktree edits are preserved.
