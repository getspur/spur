# Worker evidence and blocked-plan repair

Tracking issue: `bd-2a4e`. User approved inline infrastructure recovery before retrying E1-09b. Base: `6142413c571d0515495f5162be448efddd4aef71`.

## Approved design

Workers need an authenticated append-only evidence channel, not generic issue mutation. Bind each write to the current task delegation and brain session; reject unrelated, superseded, and terminal targets. Persist typed evidence in Beads, with an idempotency key and conflict refusal. Expose blocked/risk signals through both MCP schemas and the typed dispatcher; retain brain ownership of lifecycle transitions.

Plan membership is independent of issue lifecycle status. Query every labeled member, including closed issues. Project non-runnable open lifecycle statuses as an explicit brain hold using the existing nonterminal `EscalatedToBrain` state, with the original Beads status in its reason. This preserves compatibility and prevents dependency recomputation from scheduling held work. Terminal closed issues retain existing audit validation. Require at least one approved task for merge readiness.

## Execution plan

Inline recovery is tracked on `bd-2a4e`; no worker is dispatched into the broken evidence channel. No otobank source edits, retries, remote pushes, application restarts, or root-branch merges are included.

### B: Preserve held plan membership

Depends on: none. Scope: `crates/spur-core/src/plan/projector.rs`, `crates/spur-core/src/plan/mod.rs` and their existing tests.

1. PRE: catalog workflow safety controls for visible held versus hidden merge-ready traces; pin and copy receipts into Beads.
2. RED: project a labeled blocked/deferred task after restart; assert membership retained, not Ready, not terminal, not merge-ready. Reopen and assert readiness recovers. Assert an epic-only plan is not merge-ready. Preserve closed-task coverage.
3. Commit failing behavioral tests. Query lifecycle-independent membership with closed inclusion; hold non-runnable statuses; add the nonempty merge guard.
4. GREEN: run relevant projector/status tests through `scripts/spur-cargo`; repeat POST controls and record scope limitations. Commit fix separately.

### A: Durable worker evidence and supported blockers

Depends on: B for safe display of brain-blocked issues. Scope: core worker MCP registry/server, evidence handler, signal types/dispatcher/watcher, typed audit sentinel, and relevant tests.

1. PRE: catalog permission controls for authorized own-task append versus cross-task writes. Pin/copy receipts; behavioral tests must validate concrete identity bindings.
2. RED: exercise advertised tools through the registry; verify missing audit endpoint and blocked/risk schema support. Cover own-task persistence/reload, duplicate/conflicting IDs, cross-task/brain/stale/closed/superseded rejection and no issue-status mutation.
3. Commit failing tests. Add a narrow authenticated append-only audit endpoint and align worker signal schemas with supported typed signals. Blocked/risk signals request brain attention, never automatic success/retry.
4. GREEN: targeted worker transport, audit, signal and plan tests; POST controls; separate fix commit.

## Handoff gate

Report exact tests and commits, preserving any build/environment blockers. The running app still requires rebuild/restart and a fresh worker smoke test of evidence and blocker persistence before E1-09b can be retried. Do not treat compilation or a solver model as that live smoke test.
