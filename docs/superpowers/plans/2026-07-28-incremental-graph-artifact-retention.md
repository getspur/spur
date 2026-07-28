# Incremental Graph Artifact Retention Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-07-28-incremental-graph-artifact-retention-design.md`
**Design epic:** `bd-2wkkk` (closed)

**Goal:** Bound default incremental graph builds to the current canonical
artifact plus two rollback generations while protecting every registered
worktree pointer.

**Architecture:** Extend `spur-graph` Git discovery with NUL-safe registered
worktree enumeration. In the canonical cache writer, keep the existing
manifest lock through atomic pointer publication, compute the protected set,
then best-effort prune validated completed directories beyond the solver-backed
retention count.

**Tech Stack:** Rust 2021, `std::fs`, existing Git subprocess helpers,
`anyhow`, `fs2`, Parquet artifact fixtures, `scripts/spur-cargo`

---

### Task 1: Enumerate registered Git worktrees safely

**Task ID:** `registered-worktree-discovery`

**Files:**
- Modify: `crates/spur-graph/src/git.rs`
- Test: unit tests colocated in `crates/spur-graph/src/git.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] A public helper returns every registered Git worktree root.
- [ ] Parsing is NUL-safe and handles a worktree path containing spaces.
- [ ] Malformed or non-UTF-8 output returns a contextual error.
- [ ] Existing `spur-graph` Git tests remain green.

**Suggested Worker:** Codex using profile `rust-engineer`, model
`gpt-5.6-sol`, effort `max`.

**Scope Boundary:**
- IN scope: Git subprocess invocation and registered-worktree parsing in
  `crates/spur-graph/src/git.rs`.
- OUT of scope: cache retention, pointer parsing, CLI changes, and other files.
- If another file is required, emit a `scope_drift` signal before editing it.

**Implementation:**

- [ ] **Step 1: Write the failing worktree-enumeration test**

Add a NUL-safe helper with this wished-for production contract:

```rust
pub fn registered_worktree_roots(root: &Path) -> anyhow::Result<Vec<PathBuf>>;
```

The test creates a temporary Git repository and linked worktree, invokes the
helper, canonicalizes the returned roots, and asserts that both roots are
present. Include a linked-worktree path containing spaces so line-based parsing
cannot pass accidentally.

- [ ] **Step 2: Verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-graph git::tests::registered_worktree_roots -- --nocapture
```

Expected: FAIL because the helper/behavior does not exist yet.

- [ ] **Step 3: Implement NUL-safe registered worktree discovery**

Use the existing `git_stdout_bytes` helper with:

```rust
["worktree", "list", "--porcelain", "-z"]
```

Parse only `worktree ` records, preserve paths as platform paths, reject
malformed UTF-8 with context, and return roots in Git's deterministic order.
Do not fall back to human-formatted `git worktree list`.

- [ ] **Step 4: Verify GREEN and format**

Run:

```bash
scripts/spur-cargo test -p spur-graph git::tests::registered_worktree_roots -- --nocapture
scripts/spur-cargo fmt --all
```

Both commands must exit `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/git.rs
git commit -m "feat(spur-graph): G1.a enumerate registered worktrees"
```

### Task 2: Prune stale canonical graph generations

**Task ID:** `graph-artifact-retention`

**Files:**
- Modify: `crates/spur-graph/src/store/cache.rs`
- Test: unit tests colocated in `crates/spur-graph/src/store/cache.rs`

**Depends on:** `registered-worktree-discovery`

**Acceptance Criteria:**
- [ ] `RETAINED_CANONICAL_ARTIFACTS` equals the solved value `3`.
- [ ] Four canonical publications retain the current artifact and two rollback
      artifacts.
- [ ] Canonical targets referenced by registered worktrees are retained even
      when older than the newest three.
- [ ] Temporary, foreign, incomplete, and other-manifest entries are untouched.
- [ ] Cleanup happens only after both worktree pointers publish successfully.
- [ ] Cleanup discovery/deletion failures warn but do not fail a published build.
- [ ] Explicit output and lock-timeout fallback behavior remains unchanged.
- [ ] Targeted tests, all `spur-graph` tests, formatting, and Clippy pass.

**Suggested Worker:** Codex using profile `rust-engineer`, model
`gpt-5.6-sol`, effort `max`.

**Authoritative Solve Artifact:**
- Reload `sol_e8ce7c3b90e74292` with `get_solve_result`.
- Treat `retained_generations = 3` as authoritative.
- Safety proofs: `sol_8b4a6cca225a42ac` and
  `sol_c63d53d133b447bc`.

**Scope Boundary:**
- IN scope: canonical cache retention and cache-writer tests in
  `crates/spur-graph/src/store/cache.rs`.
- OUT of scope: Git helper changes, CLI flags, explicit output cleanup,
  age/byte quotas, other crates, standalone cleanup commands, and unrelated
  cache refactors.
- If another file is required, emit a `scope_drift` signal before editing it.

**Scope Drift Checkpoint:**
- If estimated remaining work exceeds the task by more than 50%, emit
  `scope_drift`.
- If the Task 1 API cannot safely discover protected pointers, emit `risk`
  before changing the interface.

**Implementation:**

- [ ] **Step 1: Reload and assert the solved retention value**

Call:

```json
{"solve_id":"sol_e8ce7c3b90e74292"}
```

Add the solver-backed constant and invariant test:

```rust
const RETAINED_CANONICAL_ARTIFACTS: usize = 3;

#[test]
fn retention_count_covers_current_and_two_rollbacks() {
    const CURRENT: usize = 1;
    const ROLLBACKS: usize = 2;
    assert_eq!(RETAINED_CANONICAL_ARTIFACTS, CURRENT + ROLLBACKS);
}
```

- [ ] **Step 2: Write failing retention tests**

In `store/cache.rs`, add:

```rust
#[test]
fn prune_keeps_current_and_two_rollback_generations() {
    // Arrange four CanonicalArtifactCandidate values with explicit
    // `UNIX_EPOCH + Duration::from_secs(1..=4)` modification times and
    // protect the fourth as current.
    // Assert generations 2, 3, and 4 remain and generation 1 is stale.
}

#[test]
fn prune_keeps_an_older_generation_pinned_by_a_worktree() {
    // Arrange five completed generations and protect the oldest pointer target.
    // Assert the newest three plus the pinned oldest generation remain.
}

#[test]
fn prune_ignores_noncanonical_and_incomplete_entries() {
    // Arrange `.parquet.tmp.*`, a foreign file, and a `.parquet` directory
    // without `manifest.json`; assert all survive.
}
```

Keep ordering tests independent from filesystem timestamp resolution by
constructing candidate records with explicit `SystemTime` values.

- [ ] **Step 3: Verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-graph store::cache::tests::prune_ -- --nocapture
```

Expected: FAIL because no pruning helper/integration exists.

- [ ] **Step 4: Implement protected-set discovery and pruning**

Add focused private helpers:

```rust
struct CanonicalArtifactCandidate {
    path: PathBuf,
    modified: SystemTime,
}

fn protected_canonical_artifacts(
    worktree_root: &Path,
    canonical_dir: &Path,
    written_dir: &Path,
) -> Result<BTreeSet<PathBuf>>;

fn stale_canonical_artifacts(
    candidates: Vec<CanonicalArtifactCandidate>,
    protected: &BTreeSet<PathBuf>,
) -> Vec<PathBuf>;

fn prune_canonical_artifacts_best_effort(
    canonical_dir: &Path,
    written_dir: &Path,
    protected: &BTreeSet<PathBuf>,
);
```

Required behavior:

- start protection with the canonicalized `written_dir`;
- enumerate worktrees through `git::registered_worktree_roots`;
- inspect existing `CURRENT` and pointer JSON targets;
- protect only canonicalized direct children of `canonical_dir`;
- scan only completed direct-child `.parquet` directories containing
  `manifest.json`;
- sort newest first by modification time and full-path tie-breaker;
- retain the newest three plus all protected targets;
- delete only validated stale candidates;
- warn and continue on individual deletion failure;
- skip the whole pass on discovery/inspection uncertainty.

- [ ] **Step 5: Integrate cleanup after publication under the cache lock**

Restructure `write_with_dedup_with_section_sidecar_options` so the successful
canonical branch performs:

```rust
write_current_pointer(worktree_root, &written_dir)?;
write_pointer(artifact, worktree_root, ctx, &written_dir)?;
prune_after_success_best_effort(worktree_root, &canonical_dir, &written_dir);
```

before explicitly unlocking the manifest lock. Ensure every error path still
unlocks or drops the lock and cleanup is not called when either pointer write
returns an error. Do not add cleanup to the lock-timeout fallback branch.

- [ ] **Step 6: Verify GREEN and run the crate checks**

Run:

```bash
scripts/spur-cargo test -p spur-graph store::cache::tests -- --nocapture
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo fmt --all
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
```

All commands must exit `0`. Inspect the final diff to confirm only
`crates/spur-graph/src/store/cache.rs` changed in this task.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-graph/src/store/cache.rs
git commit -m "feat(spur-graph): G1.b bound artifact retention"
```
