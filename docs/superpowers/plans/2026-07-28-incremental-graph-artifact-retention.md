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

### Task 1: Implement safe canonical graph artifact retention

**Task ID:** `graph-artifact-retention`

**Files:**
- Modify: `crates/spur-graph/src/git.rs`
- Modify: `crates/spur-graph/src/store/cache.rs`
- Test: unit tests colocated in both files

**Depends on:** none

**Acceptance Criteria:**
- [ ] `RETAINED_CANONICAL_ARTIFACTS` equals the solved value `3`.
- [ ] Four sequential canonical publications retain the current artifact and
      two rollback artifacts.
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
- IN scope: registered-worktree discovery and canonical cache retention inside
  `spur-graph`.
- OUT of scope: CLI flags, explicit output cleanup, age/byte quotas, other
  crates, standalone cleanup commands, and unrelated cache refactors.
- If another file is required, emit a `scope_drift` signal before editing it.

**Scope Drift Checkpoint:**
- If estimated remaining work exceeds the task by more than 50%, emit
  `scope_drift`.
- If safe pointer discovery cannot be implemented in the two listed files,
  emit `risk` with the unsafe case before proceeding.

**Implementation:**

- [ ] **Step 1: Reload and assert the solved retention value**

Call:

```json
{"solve_id":"sol_e8ce7c3b90e74292"}
```

Add a constant and invariant test in `store/cache.rs`:

```rust
const RETAINED_CANONICAL_ARTIFACTS: usize = 3;

#[test]
fn retention_count_covers_current_and_two_rollbacks() {
    const CURRENT: usize = 1;
    const ROLLBACKS: usize = 2;
    assert_eq!(RETAINED_CANONICAL_ARTIFACTS, CURRENT + ROLLBACKS);
}
```

- [ ] **Step 2: Write failing Git worktree-enumeration tests**

Add a NUL-safe helper with this production contract:

```rust
pub fn registered_worktree_roots(root: &Path) -> anyhow::Result<Vec<PathBuf>>;
```

The test creates a temporary Git repository and linked worktree, invokes the
helper, canonicalizes the returned roots, and asserts that both roots are
present. Include a path containing spaces so line-based parsing cannot pass
accidentally.

- [ ] **Step 3: Verify the Git test fails for the missing behavior**

Run:

```bash
scripts/spur-cargo test -p spur-graph git::tests::registered_worktree_roots -- --nocapture
```

Expected: FAIL because the helper/behavior does not exist yet.

- [ ] **Step 4: Implement NUL-safe registered worktree discovery**

Use the existing `git_stdout_bytes` helper with:

```rust
["worktree", "list", "--porcelain", "-z"]
```

Parse only `worktree ` records, preserve paths as platform paths, reject
malformed UTF-8 with context, and return roots in Git's deterministic order.
Do not fall back to human-formatted `git worktree list`.

- [ ] **Step 5: Verify the Git discovery test passes**

Run:

```bash
scripts/spur-cargo test -p spur-graph git::tests::registered_worktree_roots -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Write failing retention tests**

In `store/cache.rs`, test the behavior through focused filesystem fixtures:

```rust
#[test]
fn prune_keeps_current_and_two_rollback_generations() {
    // Arrange four CanonicalArtifactCandidate values with explicit
    // `UNIX_EPOCH + Duration::from_secs(1..=4)` modification times and
    // protect the fourth as current.
    // Act with the production stale-candidate selector.
    // Assert generations 2, 3, and 4 remain and generation 1 is removed.
}

#[test]
fn prune_keeps_an_older_generation_pinned_by_a_worktree() {
    // Arrange five completed generations.
    // Protect the newest current generation and the oldest worktree target.
    // Assert the newest three plus the pinned oldest generation remain.
}

#[test]
fn prune_ignores_noncanonical_and_incomplete_entries() {
    // Arrange `.parquet.tmp.*`, a foreign file, and a `.parquet` directory
    // without `manifest.json`; assert all survive.
}
```

Keep ordering tests independent from filesystem timestamp resolution by
constructing candidate records with explicit `SystemTime` values. Exercise
filesystem classification/deletion separately in the ignored-entry test.

- [ ] **Step 7: Verify retention tests fail for missing cleanup**

Run:

```bash
scripts/spur-cargo test -p spur-graph store::cache::tests::prune_ -- --nocapture
```

Expected: FAIL because no pruning helper/integration exists.

- [ ] **Step 8: Implement protected-set discovery and pruning**

Add focused private helpers in `store/cache.rs`:

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
- enumerate registered worktrees through
  `git::registered_worktree_roots`;
- inspect existing `CURRENT` and pointer JSON targets;
- protect only canonicalized direct children of `canonical_dir`;
- scan only completed direct-child `.parquet` directories containing
  `manifest.json`;
- sort newest first by modification time and full-path tie-breaker;
- retain the newest three plus all protected targets;
- delete only validated stale candidates;
- warn and continue on individual deletion failure;
- skip the whole pass on discovery/inspection uncertainty.

- [ ] **Step 9: Integrate cleanup after publication under the cache lock**

Restructure `write_with_dedup_with_section_sidecar_options` so the successful
canonical branch performs:

```rust
write_current_pointer(worktree_root, &written_dir)?;
write_pointer(artifact, worktree_root, ctx, &written_dir)?;
prune_after_success_best_effort(worktree_root, &canonical_dir, &written_dir);
```

before explicitly unlocking the manifest lock. Ensure every error path still
unlocks or drops the lock and that cleanup is not called when either pointer
write returns an error. Do not add cleanup to the lock-timeout fallback branch.

- [ ] **Step 10: Verify green and run the crate checks**

Run:

```bash
scripts/spur-cargo test -p spur-graph store::cache::tests -- --nocapture
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo fmt --all
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
```

All commands must exit `0`. Inspect the final diff to confirm only the two
scoped Rust files changed.

- [ ] **Step 11: Commit**

Commit the complete implementation:

```bash
git add crates/spur-graph/src/git.rs crates/spur-graph/src/store/cache.rs
git commit -m "feat(spur-graph): G1 bound canonical artifact retention"
```
