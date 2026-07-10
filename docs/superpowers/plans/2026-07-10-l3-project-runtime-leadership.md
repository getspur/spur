# L3 Project Runtime Leadership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run L3 loop generations without an active brain session while electing exactly one SPUR process per repository to schedule and execute them.

**Architecture:** Add a repository-scoped advisory-lock supervisor to the interactive orchestrator. The elected process runs a headless, system-owned L3 reconciler/delegation runtime; brain-session reconcilers are explicitly restricted to L1/L2. Beads remains the durable recovery substrate, with exact generation identity preventing duplicate plans during leader handoff.

**Tech Stack:** Rust 2021, Tokio, `fs4`, SPUR reconciler/plan engine, beads PM adapter, `scripts/spur-cargo`.

**Design:** `docs/superpowers/specs/2026-07-10-l3-project-runtime-leader-design.md`

---

## File Structure

- Create `crates/spur-core/src/plan/loops/leadership.rs`: repository lock guard,
  holder metadata, acquisition outcomes, and cross-process lock tests.
- Create `crates/spur-core/src/orchestrator/loop_runtime.rs`: standby/leader
  supervisor and headless project runtime lifecycle.
- Modify `crates/spur-core/src/plan/loops/mod.rs`: expose leadership and runtime
  constants/types.
- Modify `crates/spur-core/src/plan/loops/scheduler.rs`: autonomy scope filtering,
  system ownership, and exact-generation recovery.
- Modify `crates/spur-core/src/plan/labels.rs`: transient durable generation-arming
  label builder/parser.
- Modify `crates/spur-core/src/plan/reconciler/mod.rs`: add explicit loop/plan scopes
  to `ReconcilerConfig` and carry them through construction.
- Modify `crates/spur-core/src/plan/reconciler/ready.rs`: limit project runtime
  discovery to system-owned L3 plans.
- Modify `crates/spur-core/src/plan/reconciler/terminal.rs`: apply the same scope to
  terminal projection and merge processing.
- Modify `crates/spur-core/src/server/mod.rs`: retain scope settings on
  `McpCallbackServer` and pass them into the reconciler.
- Modify `crates/spur-core/src/orchestrator.rs`: register the runtime module and any
  shared dependency bundle.
- Modify `crates/spur-core/src/orchestrator/support.rs`: configure brain-session
  servers as brain-armed-only and construct system-runtime dispatch dependencies.
- Modify `crates/spur-core/src/orchestrator/interactive_loop.rs`: start the supervisor
  before the UI input loop and shut it down before returning.
- Modify `crates/spur-core/src/plan/reconciler/tests.rs`: scheduler, ownership, and
  handoff characterization tests.
- Create `crates/spur-core/tests/l3_project_runtime.rs`: process-lifetime integration
  coverage with no active brain session.
- Modify `docs/loops.md`: document leader election, standby behavior, and the phase-one
  process-lifetime boundary.

## Task 1: Repository-Scoped Leadership Guard

**Files:**

- Create: `crates/spur-core/src/plan/loops/leadership.rs`
- Modify: `crates/spur-core/src/plan/loops/mod.rs`

- [ ] **Step 1: Write failing ownership tests**

Add tests covering first acquisition, standby detection, release/reacquisition,
independent repositories, and fail-closed lock errors. At least one test must hold the
lock in a child process so the assertion proves real process exclusion.

The public crate-local API exercised by the tests must be:

```rust
pub(crate) const LOOP_RUNTIME_LOCK_PATH: &str = ".spur/loop-runtime.lock";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct LoopRuntimeHolder {
    pub pid: u32,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub label: Option<String>,
    pub workdir: Option<std::path::PathBuf>,
}

pub(crate) enum LoopRuntimeLeadershipOutcome {
    Acquired(LoopRuntimeLeadership),
    Standby { holder: Option<LoopRuntimeHolder> },
    Unsafe { reason: String },
    Io(std::io::Error),
}

impl LoopRuntimeLeadership {
    pub(crate) fn try_acquire(repo_root: &std::path::Path)
        -> LoopRuntimeLeadershipOutcome;
}
```

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
scripts/spur-cargo test -p spur-core plan::loops::leadership::tests -- --nocapture
```

Expected: compilation or assertion failure because leadership types/behavior are not
implemented.

- [ ] **Step 3: Commit the failing tests**

```bash
git add crates/spur-core/src/plan/loops/leadership.rs crates/spur-core/src/plan/loops/mod.rs
git commit -m "test(spur-core): L3.1 cover loop runtime leadership"
```

- [ ] **Step 4: Implement the guard minimally**

Use `fs4::fs_std::FileExt::try_lock_exclusive` on an open, non-truncating lock file.
Store the `std::fs::File` in `LoopRuntimeLeadership` for the full guard lifetime. Write
holder JSON only after acquisition. Classify `ENOTSUP`, `EOPNOTSUPP`, and `ENOLCK` as
`Unsafe`; do not expose a degraded no-lock path. File existence alone must never count
as ownership.

- [ ] **Step 5: Run focused tests and confirm GREEN**

Run the command from Step 2. Expected: every leadership test passes, including the
child-process exclusion case.

- [ ] **Step 6: Commit the implementation**

```bash
git add crates/spur-core/src/plan/loops/leadership.rs crates/spur-core/src/plan/loops/mod.rs
git commit -m "feat(spur-core): L3.1 elect one project loop runtime"
```

## Task 2: Explicit Reconciler Scopes and System Ownership

**Files:**

- Modify: `crates/spur-core/src/plan/loops/mod.rs`
- Modify: `crates/spur-core/src/plan/loops/scheduler.rs`
- Modify: `crates/spur-core/src/plan/reconciler/mod.rs`
- Modify: `crates/spur-core/src/plan/reconciler/ready.rs`
- Modify: `crates/spur-core/src/plan/reconciler/terminal.rs`
- Modify: `crates/spur-core/src/server/mod.rs`
- Modify: `crates/spur-core/src/orchestrator/support.rs`
- Modify: `crates/spur-core/src/plan/reconciler/tests.rs`

- [ ] **Step 1: Add failing scope and ownership tests**

Replace the misleading `l3_loop_arms_generation_without_brain` setup with explicit
system-runtime coverage and retain its byte-exact template assertions. Add tests proving:

```rust
assert!(LoopSweepScope::L3Only.allows(AutonomyLevel::L3));
assert!(!LoopSweepScope::L3Only.allows(AutonomyLevel::L1));
assert!(!LoopSweepScope::BrainArmedOnly.allows(AutonomyLevel::L3));
assert!(LoopSweepScope::BrainArmedOnly.allows(AutonomyLevel::L2));
```

Also assert that an L3 generation epic contains
`labels::plan_owner(LOOP_RUNTIME_OWNER_ID)` and that a system-L3 reconciler does not
discover or terminally mutate a brain-owned non-L3 plan.

- [ ] **Step 2: Verify RED and commit tests**

Run:

```bash
scripts/spur-cargo test -p spur-core plan::reconciler::tests -- --nocapture
```

Expected: failure because `LoopSweepScope`, `PlanScope`, and the system owner do not
exist. Commit:

```bash
git add crates/spur-core/src/plan/reconciler/tests.rs crates/spur-core/src/plan/loops/mod.rs
git commit -m "test(spur-core): L3.2 cover scoped system reconciliation"
```

- [ ] **Step 3: Add the explicit scope types**

Define and thread these exact concepts:

```rust
pub(crate) const LOOP_RUNTIME_OWNER_ID: &str = "spur-loop-runtime";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopSweepScope {
    All,
    BrainArmedOnly,
    L3Only,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanScope {
    BrainOwned,
    SystemL3Only,
}
```

`ReconcilerConfig::default()` remains compatible with existing direct tests by using
`LoopSweepScope::All` and `PlanScope::BrainOwned`. Production brain MCP servers must
override the loop scope to `BrainArmedOnly`. The project runtime will use `L3Only` and
`SystemL3Only`.

- [ ] **Step 4: Enforce scopes at query boundaries**

Filter loops before any governor or persistence mutation. For system plan discovery,
require all three durable labels at the initial epic query:

```rust
vec![
    labels::PLAN_COMPLETE.to_string(),
    labels::plan_owner(LOOP_RUNTIME_OWNER_ID),
    format!("{}l3", labels::AUTONOMY_PREFIX),
]
```

Apply the same system-owner/L3 restriction to ready-task hydration, dispatch-lease
recovery, terminal epic projection, and auto-merge. Do not scan all epics and filter
only after a write-capable path has started.

- [ ] **Step 5: Persist L3 under the stable system owner**

For `LoopSweepScope::L3Only`, set `PersistPlanAsEpicInput.brain_session_id` to
`BrainSessionId::new(SessionId(LOOP_RUNTIME_OWNER_ID.into()))`. Brain-armed scopes must
continue using the real dispatch brain ID for L1/L2 continuations. No ACP brain
connection is created for the system ID.

- [ ] **Step 6: Verify GREEN and commit**

Run the focused reconciler tests and:

```bash
scripts/spur-cargo test -p spur-core --test reconciler_tick -- --nocapture
```

Expected: all focused and existing reconciler tests pass. Commit:

```bash
git add crates/spur-core/src/plan crates/spur-core/src/server/mod.rs crates/spur-core/src/orchestrator/support.rs
git commit -m "feat(spur-core): L3.2 scope system loop reconciliation"
```

## Task 3: Idempotent Generation Handoff

**Files:**

- Modify: `crates/spur-core/src/plan/loops/scheduler.rs`
- Modify: `crates/spur-core/src/plan/labels.rs`
- Modify: `crates/spur-core/src/plan/reconciler/tests.rs`

- [ ] **Step 1: Characterize the crash window with failing tests**

Create a due L3 loop carrying `spur:loop-arming:1` plus an already-persisted
generation epic carrying the exact `loop_id` and `generation` labels. After one
scheduler tick, assert:

```rust
assert_eq!(generation_epics_for(&pm, loop_id, 1).await.len(), 1);
assert!(loop_has_future_next_run(&pm, loop_id).await);
assert!(!loop_has_arming_label(&pm, loop_id).await);
assert!(!loop_run_outcomes(&pm, loop_id).await.contains(&"skipped_overlap".into()));
```

Add a second recovery case with `spur:loop-arming:1` but no generation plan; takeover
must create generation 1 exactly once and clear the claim. Add the contrasting case
where no arming claim exists and a genuinely older generation remains live at a later
cadence; that case must retain `skipped_overlap`.

- [ ] **Step 2: Verify RED and commit tests**

Run the reconciler tests from Task 2. Expected: the recovery fixture writes a false
overlap or otherwise fails to repair the schedule. Commit:

```bash
git add crates/spur-core/src/plan/reconciler/tests.rs
git commit -m "test(spur-core): L3.3 reproduce generation handoff crash window"
```

- [ ] **Step 3: Add the durable arming label vocabulary**

In `plan/labels.rs`, add the br-legal label builder/parser and round-trip tests:

```rust
pub const LOOP_ARMING_PREFIX: &str = "spur:loop-arming:";

pub fn loop_arming_label(generation: u32) -> String;
pub fn parse_loop_arming(label: &str) -> Option<u32>;
```

The label is a transient durable claim, not a new `LoopSpec` field.

- [ ] **Step 4: Implement claim-first exact-generation recovery**

Refactor generation selection around this exact state machine:

```rust
enum GenerationDisposition {
    ResumeClaim { generation: u32 },
    CreateClaim { generation: u32 },
    SkipOlderLive { generation: u32 },
}
```

For a new L3 generation, atomically update the loop issue to add
`spur:loop-arming:<generation>` and move `spur:loop-next-run:*` forward before
persisting the plan. Then persist the exact generation and remove the arming label.

On every sweep, process an existing arming claim before the ordinary next-run gate:

- Exact plan exists: remove the claim, retain the future next-run label, and wake
  reconciliation without another epic or skipped record.
- Exact plan is absent: persist that claimed generation, then remove the claim.
- No claim and an older generation is live: use the existing `skipped_overlap` path.

If persistence fails after claiming, retain the claim so the same leader or its
successor repairs it. Never infer crash recovery from elapsed wall time.

- [ ] **Step 5: Verify GREEN and commit**

Run the Task 2 commands. Expected: recovery, real-overlap, and legacy loop tests all
pass. Commit:

```bash
git add crates/spur-core/src/plan/labels.rs crates/spur-core/src/plan/loops/scheduler.rs crates/spur-core/src/plan/reconciler/tests.rs
git commit -m "fix(spur-core): L3.3 repair persisted loop generations idempotently"
```

## Task 4: Project Runtime Supervisor and Interactive Wiring

**Files:**

- Create: `crates/spur-core/src/orchestrator/loop_runtime.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-core/src/orchestrator/support.rs`
- Modify: `crates/spur-core/src/orchestrator/interactive_loop.rs`
- Modify: `crates/spur-core/src/server/mod.rs`
- Create: `crates/spur-core/tests/l3_project_runtime.rs`

- [ ] **Step 1: Add failing supervisor lifecycle tests**

Test a supervisor dependency seam rather than spawning real workers for every case.
The seam must expose these observable states:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectLoopRuntimeState {
    Standby,
    LeaderRunning,
    UnsafeDisabled,
    Stopped,
}
```

Cover one leader for two supervisors, standby promotion after leader shutdown, and
guard retention until the leader runtime has stopped. Add an integration test that
starts the project runtime with a mock beads PM, no `BrainSession`, and a due L3 loop;
it must persist exactly one system-owned generation.

- [ ] **Step 2: Verify RED and commit tests**

Run:

```bash
scripts/spur-cargo test -p spur-core orchestrator::loop_runtime -- --nocapture
scripts/spur-cargo test -p spur-core --test l3_project_runtime -- --nocapture
```

Expected: compilation failure because the supervisor does not exist. Commit:

```bash
git add crates/spur-core/src/orchestrator/loop_runtime.rs crates/spur-core/tests/l3_project_runtime.rs crates/spur-core/src/orchestrator.rs
git commit -m "test(spur-core): L3.4 cover project loop runtime lifecycle"
```

- [ ] **Step 3: Implement the supervisor**

The supervisor must own the leadership guard and all child task abort handles. Its
leader branch creates a headless `McpCallbackServer` using the stable system ID, a
no-op/durable-only `DetachedContinuationCtx`, the existing event sink, PM service,
outcome store, worker registry, cancellation control, and worker-MCP fetcher. It must:

1. Configure `LoopSweepScope::L3Only` and `PlanScope::SystemL3Only`.
2. Start the reconciler without creating an ACP brain connection or sending a prompt.
3. Spawn `orchestrator::delegation::handle_delegations` for worker execution.
4. Hold leadership while both reconciler and delegation handler are live.
5. On shutdown, stop the server and child tasks before dropping leadership.
6. On standby, retry lock acquisition with bounded backoff and no error spam.
7. On `Unsafe`, emit a prominent warning and never start an L3 runtime.

- [ ] **Step 4: Wire only the interactive process lifetime**

Start the supervisor near the beginning of `Orchestrator::run_interactive`, after PM
startup context is available and before entering the input loop. Shut it down in the
existing cleanup tail before returning. Do not start a background L3 runtime merely
from `Orchestrator::with_pm_service`, because many tests construct an orchestrator
without running an application loop.

Brain-session `McpCallbackServer` construction must explicitly select
`BrainArmedOnly`; the project supervisor is the sole L3 scheduler candidate.

- [ ] **Step 5: Verify GREEN and commit**

Run both commands from Step 2 plus:

```bash
scripts/spur-cargo test -p spur-core --test reconciler_late_enable -- --nocapture
scripts/spur-cargo test -p spur-core --test shutdown_mcp_server_bounded --features test-support -- --nocapture
```

Expected: all supervisor, integration, late-enable, and bounded-shutdown tests pass.
Commit:

```bash
git add crates/spur-core/src/orchestrator crates/spur-core/src/orchestrator.rs crates/spur-core/src/server/mod.rs crates/spur-core/tests/l3_project_runtime.rs
git commit -m "feat(spur-core): L3.4 run loops from the elected project runtime"
```

## Task 5: Operator Documentation and Full Verification

**Files:**

- Modify: `docs/loops.md`
- Modify only if verification exposes a defect: files already listed in Tasks 1-4

- [ ] **Step 1: Document exact operator behavior**

Update `docs/loops.md` to state:

- L3 requires a running SPUR process but no active brain session.
- Exactly one TUI process holds `.spur/loop-runtime.lock` and executes L3 work.
- Other TUIs are standbys and observe durable state through PM refresh.
- Leadership transfers after clean exit or crash.
- Unsupported advisory locking parks L3 loops rather than risking duplicates.
- L1/L2 still require an active brain session.
- Always-on execution after all TUIs exit belongs to the later daemon phase.

- [ ] **Step 2: Run formatting**

```bash
scripts/spur-cargo fmt --all
```

Expected: exit 0.

- [ ] **Step 3: Run focused and crate-wide verification**

```bash
scripts/spur-cargo test -p spur-core plan::loops -- --nocapture
scripts/spur-cargo test -p spur-core plan::reconciler -- --nocapture
scripts/spur-cargo test -p spur-core --test l3_project_runtime -- --nocapture
scripts/spur-cargo test -p spur-core
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core --all-targets -- -D warnings
```

Expected: every command exits 0 with no failing tests or warnings.

- [ ] **Step 4: Check the final diff against the spec**

Verify every concurrency invariant in the design has either a focused test or a query
boundary assertion. Run:

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; only intentional task files are modified.

- [ ] **Step 5: Commit docs and any verification-only fixes**

```bash
git add docs/loops.md crates/spur-core
git commit -m "docs(loops): L3.5 explain project runtime leadership"
```

The worker must return the commit list, exact verification commands with exit status,
and any remaining limitations. It must not merge its branch or close the beads issue;
the brain performs review and integration.
