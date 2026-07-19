# Runtime Skill Projection Launch Integration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:executing-plans` to implement this plan task-by-task. Follow the
> repository SPUR transaction, plan-task, code-exploration, TDD, and
> verification skills throughout.

**Goal:** Wire the completed generation-backed runtime skill projection into
supported brain and worker launches so every launch materializes the effective
built-in + active-pool + eligible-repository skill set before the agent starts.

**Architecture:** Keep resolution, generation, ownership, migration, and Git
hygiene inside `skills::projection`. Add one small agent-kind runtime entry
point that maps supported `AgentKind` values to existing adapters and returns
`Ok(None)` for unsupported kinds. Brain connection setup calls it after the
brain config is resolved and before connection construction. Worker setup calls
it after worktree/profile materialization and before connection construction,
replacing the pool-only direct writer. Fatal projection errors abort the
affected launch; structured ownership skips remain warnings and permit launch.

**Locked design:**
`docs/superpowers/specs/2026-07-17-runtime-skill-projection-design.md`, especially
“Selection policy,” “Launch Integration,” “Error Handling,” and “Launch
integration tests.” This plan completes that already-approved design; it does
not reopen its policy decisions.

**Base:** `8a07340060a4566c0761c4363268b646fb960058`

**Rust commands:** Always use `scripts/spur-cargo`; never invoke bare `cargo`.

## Contracts That Must Not Drift

- Runtime policy is always `SelectionPolicy::AllActive` for v1. The legacy
  delegation `skills` field must not narrow runtime projection.
- Every bundled skill is selected for supported external adapters, including
  bundled skills whose frontmatter says `role: brain`.
- Active, gate-approved pool skills and eligible repository overrides retain
  the existing resolver precedence and validation rules.
- Brain projection uses `source_repo_root == launch_root == repo_root` with
  `RuntimeRole::Brain`.
- Worker projection uses the source repository for resolution and the
  provisioned worker worktree as `launch_root`, with `RuntimeRole::Worker`.
- Unsupported `AgentKind` values remain launchable and perform no projection;
  supported kinds use `Adapter::for_agent_kind` as the authoritative mapping.
- Projection must finish before either connection construction or ACP
  initialization/session creation.
- A fatal `ProjectionError` stops the affected agent. A `ProjectionSummary`
  containing `skipped` ownership collisions is logged and launch continues.
- On worker projection failure, remove the newly provisioned worker worktree
  through `WorktreeManager` before returning the setup error.
- Keep the legacy materializer and JSONL ownership metadata available for
  migration/adoption. Remove only its worker-launch call site.
- Do not change adapter rendering, resolver precedence, manifest schemas,
  generation layout, ownership rules, installer behavior, or dependencies.
- Do not touch unrelated dirty files in the brain checkout.

## File Map

- Modify: `crates/spur-core/src/skills/projection/mod.rs`
  - Add the shared supported-agent runtime entry point and focused mapping/no-op
    tests if needed.
- Modify: `crates/spur-core/src/orchestrator/connection.rs`
  - Reconcile the selected brain adapter before connection construction and
    test fatal ordering plus successful materialization.
- Modify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
  - Replace pool-only materialization with projection, preserve profile-before-
    projection ordering, add a projection setup error, clean up on failure, and
    update ordering/content tests.
- Preserve: `crates/spur-core/src/explore/materialize.rs`
  - No production deletion; its legacy records remain migration input.

## Task 1: Add RED Launch-Integration Regressions

**Files:**

- Modify tests in `crates/spur-core/src/orchestrator/connection.rs`
- Modify tests in
  `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
- Modify tests in `crates/spur-core/src/skills/projection/mod.rs` only when a
  shared runtime helper needs direct coverage

### Step 1: Cover worker union and ordering

Refine the existing
`attempt_materializes_pool_skills_before_session` harness into a projection
test. Configure a deterministic bundled-skill directory containing at least a
brain-role bundled skill and add one accepted pool skill. Keep
`ctx.skills = Some(...)` to prove the legacy request field does not narrow
`AllActive`.

The fake connection must inspect the worker launch root during
`initialize(...)` (or the earliest observable connection boundary) and assert:

- the bundled skill exists at the Codex-native projection target;
- the accepted pool skill exists;
- the projected target resolves through the generation-backed layout under
  `.spur/runtime/skill-projections/codex` (or is the verified copy fallback);
- the worker worktree remains clean and required excludes exist.

This test must fail against the base because the current worker path writes
only the selected pool subset directly.

### Step 2: Cover worker fatal ordering and cleanup

Add a deterministic projection resolution failure, preferably a pool digest
mismatch or an explicitly missing configured bundled directory. Use a
connection factory that records whether it was called.

Assert:

- `run_one_worker_attempt` returns the dedicated skill-projection setup error;
- the connection factory was never called;
- the failed attempt worktree is removed from the manager/filesystem;
- the error retains adapter, launch-root, phase, and underlying cause text from
  `ProjectionError`.

### Step 3: Cover brain projection before connection

Build a temporary repository/orchestrator with a supported brain config and a
deterministic bundled skill. Exercise the narrowest real brain connection seam
that proves projection precedes connection creation/initialization.

Required assertions:

- successful setup projects the bundled skill into the repository’s native
  adapter directory before the connection’s initialization observation;
- a deterministic fatal projection error is returned before an intentionally
  unusable/failing brain command can become the primary error;
- the error contains `skill projection` context and no brain session is
  created.

Prefer existing orchestrator test builders. If connection injection would
require a broad production abstraction, factor a small private preparation
helper and test it plus the production call ordering instead of adding a new
long-lived dependency or public trait.

### Step 4: Cover unsupported agent kinds

Add a focused test that the shared runtime entry point returns `Ok(None)` for
an unsupported kind and does not create projection state. Do not invent a new
adapter mapping.

### Step 5: Run the RED tests

Run the narrowest new test filters through `scripts/spur-cargo`. Capture the
expected failures in the worker summary; failures must be due to missing launch
integration, not fixture or compilation mistakes.

### Step 6: Commit RED

```bash
git add crates/spur-core/src/skills/projection/mod.rs \
  crates/spur-core/src/orchestrator/connection.rs \
  crates/spur-core/src/orchestrator/delegation/worker_attempt.rs
git commit -m "test(skills): rsp-5 cover runtime launch projection"
```

## Task 2: Implement the Shared Runtime Projection Entry Point

**Files:**

- Modify: `crates/spur-core/src/skills/projection/mod.rs`

### Step 1: Add the supported-agent helper

Add a small async entry point with this responsibility (exact naming may follow
local style):

```rust
pub async fn reconcile_for_agent_kind(
    worktrees: &spur_worktree::manager::WorktreeManager,
    source_repo_root: &Path,
    launch_root: &Path,
    kind: spur_acp::AgentKind,
    role: RuntimeRole,
) -> Result<Option<ProjectionSummary>, ProjectionError>
```

It must:

1. call `Adapter::for_agent_kind(kind)`;
2. return `Ok(None)` without filesystem mutation when no adapter exists;
3. call `reconcile_with_worktrees` with `SelectionPolicy::AllActive` for a
   supported adapter;
4. return the complete summary without converting fatal errors into warnings.

Do not add another adapter table or accept a selection list.

### Step 2: Keep observability at the launch boundary

The helper returns data; launch callers log one structured completion record
and explicit warnings for `summary.skipped`. Avoid dumping every selected skill
at info level. Fatal errors propagate with existing `ProjectionError` context.

## Task 3: Integrate Worker Launch

**Files:**

- Modify:
  `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`

### Step 1: Add a setup error variant

Add a dedicated `AttemptSetupError::SkillProjectionFailed(String)` display
variant so projection failures are distinguishable from ACP initialization and
session failures.

### Step 2: Replace the legacy call site

Immediately after optional profile materialization and before spawn args,
connection factory/build, ACP initialization, or session creation:

- call the shared runtime helper with the existing `WorktreeManager`, source
  repository root, worker worktree path, selected `AgentKind`, and
  `RuntimeRole::Worker`;
- log a successful supported-adapter summary and ownership-skip warnings;
- on fatal error, save its full text, remove the worker worktree, and return
  `SkillProjectionFailed`;
- on `Ok(None)`, continue unchanged for the unsupported agent kind.

Delete the `materialize_pool_skills(...)` invocation only. Do not delete its
implementation or records, and do not feed `ctx.skills` to projection.

### Step 3: Make worker tests GREEN

Run the worker launch filters through `scripts/spur-cargo`. Confirm the fake
connection observes projection before initialization and fatal projection does
not reach connection construction.

## Task 4: Integrate Brain Launch

**Files:**

- Modify: `crates/spur-core/src/orchestrator/connection.rs`

### Step 1: Reconcile after brain config resolution

Inside `Orchestrator::connect_brain`, after cloning the selected `brain_config`
and before `create_connection(...)`:

- construct/reuse a `WorktreeManager` for `self.repo_root`;
- call the shared runtime helper with source and launch root both equal to
  `self.repo_root`, `brain_config.kind`, and `RuntimeRole::Brain`;
- log the same structured success/warning information as worker launch;
- propagate fatal projection errors with context identifying the selected
  brain.

Putting the hook in `connect_brain` must cover `spawn_brain_session`, reconnect,
and direct interactive connection callers shown by exact graph impact.

### Step 2: Make brain tests GREEN

Run the new brain launch filters through `scripts/spur-cargo`. Confirm the
projection error wins before connection/initialization and successful launch
materializes both bundled and accepted pool skills.

## Task 5: Verify and Commit GREEN

### Step 1: Run focused suites

```bash
scripts/spur-cargo test -p spur-core \
  orchestrator::delegation::worker_attempt -- --nocapture
scripts/spur-cargo test -p spur-core \
  orchestrator::connection -- --nocapture
scripts/spur-cargo test -p spur-core \
  skills::projection -- --nocapture
scripts/spur-cargo test -p spur-core \
  explore::materialize::tests -- --nocapture
```

If module-level filters do not select tests in this workspace, use the exact
new test names and record both the command and selected test count.

### Step 2: Run crate and formatting gates

```bash
scripts/spur-cargo test -p spur-core
scripts/spur-cargo fmt --all -- --check
git diff --check
```

Do not substitute plain `cargo`. A genuine remote failure is a real failure.

### Step 3: Audit scope

```bash
git status --short
git diff --name-only HEAD~1
git log --oneline --decorate -3
```

Only the three declared Rust files may change. If a test genuinely requires a
new integration-test file, signal scope drift before adding it.

### Step 4: Commit GREEN

```bash
git add crates/spur-core/src/skills/projection/mod.rs \
  crates/spur-core/src/orchestrator/connection.rs \
  crates/spur-core/src/orchestrator/delegation/worker_attempt.rs
git commit -m "feat(skills): rsp-5 project skills at agent launch"
```

The worker must preserve a RED test commit followed by a GREEN implementation
commit in its own branch, even if orchestration later normalizes the delivered
diff.

## Independent Review Gate

Before brain approval, delegate an independent, read-only architecture review
of the completed implementation branch to:

- worker: `claude-code`
- profile: `design-system-architect`
- model: `claude-fable-5[1m]`
- effort: `xhigh`

The reviewer must inspect the locked spec, this plan, the exact diff, launch
callers, and tests. It must report findings ordered by severity with file/line
evidence, explicitly checking:

1. brain projection happens before all connection construction paths;
2. worker projection happens after profile materialization and before all
   process/ACP startup paths;
3. fatal failures stop launch and worker cleanup is complete;
4. collisions remain non-fatal and user ownership is preserved;
5. `AllActive` includes bundled + active pool and ignores delegation narrowing;
6. unsupported agents remain a no-op;
7. legacy migration metadata remains intact;
8. tests prove ordering rather than only testing the projection helper.

The reviewer is read-only. If it finds a Critical or Important issue, the brain
must request changes from the implementation worker with the concrete finding,
then repeat the relevant verification and independent review. Approval requires
no unresolved Critical or Important findings.

## Completion Evidence

The implementation worker must return:

- RED command(s) and expected failure reason;
- focused and full verification command(s), selected test counts, and exit
  statuses;
- changed-file list;
- RED and GREEN commit hashes;
- proof the worker worktree was clean at handoff;
- any unsupported-agent behavior intentionally left unchanged.

The brain must independently verify the diff, focused tests, formatting, and
review findings before merging the plan.
