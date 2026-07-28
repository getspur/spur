# Incremental Graph Artifact Retention — Design Spec

**Design epic:** `bd-2wkkk`
**Plan ID:** `2f3a717c-5651-4ad9-9370-69cf03658834`
**Status:** Approved

## 1. Problem

Default `spur graph build` writes content-addressed Parquet directories under:

```text
<git-common-dir>/spur-graph/artifacts/<manifest-version>/<graph-hash>.parquet/
```

Each successful incremental build advances `.spur/graph/CURRENT` and
`.spur/graph-index.pointer.json`, but the canonical cache never removes older
generations. In the SPUR checkout, 44 generations grew to roughly 12 GiB.

The cache needs bounded retention without deleting a published artifact that is
still selected by the main checkout or a registered linked worktree.

## 2. Goals

- Retain the newly published graph plus two rollback generations.
- Protect every canonical artifact referenced by a registered Git worktree,
  even when this temporarily exceeds the three-generation target.
- Run cleanup only after the new canonical artifact and both worktree pointers
  have been published successfully.
- Serialize publication and cleanup with the existing manifest-scoped cache
  lock.
- Keep cleanup failure from invalidating an otherwise successful graph build.
- Remove abandoned completed generations automatically on later default builds.

## 3. Non-goals

- Pruning user-owned explicit output paths.
- Pruning worktree-local fallback artifacts written after cache-lock timeout.
- Applying a byte-size or age-based quota.
- Cleaning artifacts from other manifest-version directories.
- Adding a user-facing cleanup command in this change.

## 4. Solved Retention Constant

The retention target is a constraint-derived constant rather than an arbitrary
magic number.

Variables:

- `current_generations = 1`
- `rollback_generations >= 2`
- `retained_generations = current_generations + rollback_generations`
- `retained_generations <= 3`

Z3 returned the only feasible model:

```text
current_generations = 1
rollback_generations = 2
retained_generations = 3
```

Persisted proof artifacts:

- `sol_e8ce7c3b90e74292`: feasibility is `sat` at `1 + 2 = 3`.
- `sol_8b4a6cca225a42ac`: deletion predicate `rank >= 3` is `unsat`
  for protected recency ranks `0..=2`.
- `sol_c63d53d133b447bc`: the predicate is `sat` for recency rank `3`.

The implementation constant is therefore:

```rust
const RETAINED_CANONICAL_ARTIFACTS: usize = 3;
```

A test must assert the relationship between this constant, one current
generation, and two rollback generations.

## 5. Integration Point

Canonical writes converge in
`spur_graph::store::cache::write_with_dedup_with_section_sidecar_options`.
The CLI default build uses this path for full and incremental artifacts.
Explicit output overrides bypass it and remain user-owned.

The canonical path performs these operations while holding the existing
manifest-scoped exclusive lock:

1. Resolve the previous artifact for incremental and sidecar reuse.
2. Write or repair the canonical artifact.
3. Atomically publish `.spur/graph/CURRENT`.
4. Atomically publish `.spur/graph-index.pointer.json`.
5. Discover protected canonical targets from registered worktrees.
6. Prune stale completed generations.
7. Release the cache lock.

If pointer publication fails, step 6 is not attempted and the build returns the
publication error. Cleanup never runs before the new pointer is durable.

## 6. Retained Set

Cleanup considers only direct child directories of the current
`<manifest-version>` canonical directory that:

- end in `.parquet`;
- contain `manifest.json`; and
- are not staging directories containing `.parquet.tmp.`.

Candidates are ordered by directory modification time, newest first, with the
full path as a deterministic tie-breaker. The retained set is the union of:

1. the newly written canonical path;
2. the three newest completed canonical generations; and
3. completed canonical paths referenced by any registered worktree.

The union may contain more than three artifacts when multiple worktrees pin
older generations. Safety takes precedence over the target count. Once those
pointers advance or the worktrees are removed, a later build makes the old
generations eligible.

## 7. Registered Worktree Protection

Use `git worktree list --porcelain -z` from the current worktree to enumerate
registered worktree roots without parsing human-formatted output. For each
worktree:

- inspect `.spur/graph/CURRENT` when present;
- inspect `.spur/graph-index.pointer.json` when present;
- canonicalize valid targets; and
- protect targets that are direct children of the canonical directory being
  pruned.

Missing pointer files are normal. If worktree enumeration fails or an existing
pointer cannot be inspected safely, skip the entire cleanup pass and emit a
warning. A discovery problem must fail open toward retention, never deletion.

## 8. Concurrency and Failure Semantics

- Keep the manifest-scoped exclusive lock held through pointer publication and
  cleanup so two default builds cannot publish and prune concurrently.
- Lock-timeout fallback writes only to the current worktree and does not prune
  the canonical cache.
- Deletion is restricted to validated direct children of the canonical
  manifest directory.
- A failed deletion emits a warning with the path and continues with remaining
  candidates.
- Discovery failure skips pruning and returns success after logging.
- Cleanup failure does not roll back `CURRENT`, the pointer JSON, or the newly
  written artifact.

## 9. Testing

Tests live with the cache implementation in
`crates/spur-graph/src/store/cache.rs`.

Required TDD cases:

1. Four completed generations prune to the newest three and retain the current
   target.
2. A generation older than the newest three remains when included in the
   protected-pointer set.
3. Temporary directories, foreign files, and incomplete Parquet directories
   are ignored.
4. Cleanup is not invoked when publication fails.
5. The constant invariant is explicit: one current plus two rollback
   generations equals the retention count.

Run compile-heavy verification only through:

```bash
scripts/spur-cargo test -p spur-graph store::cache::tests -- --nocapture
scripts/spur-cargo test -p spur-graph
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

## 10. Acceptance Criteria

- A fourth unprotected completed canonical generation is removed after a
  successful default graph publication.
- The current artifact, two rollback artifacts, and all registered-worktree
  targets survive cleanup.
- Explicit outputs and worktree-local fallback artifacts are unchanged.
- Publication errors never trigger cleanup.
- Cleanup errors are observable but do not fail a successful build.
- Targeted tests, crate tests, formatting, and Clippy pass.

## 11. Alternatives Rejected

### Retain only the current generation

This maximizes reclamation but removes rollback safety during graph-format or
sidecar regressions.

### Seven-day retention

Artifact sizes vary enough that age does not provide a useful disk bound.

### Delete only the immediately previous pointer

This does not collect generations leaked by earlier builds and therefore does
not bound cache growth.

### Prune explicit output paths

Explicit outputs are caller-owned and may be archives or integration fixtures;
automatic deletion would violate ownership expectations.
