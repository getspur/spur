# Temporal Graph Build Performance Correction Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `beads://bd-214j` (approved measured design; no notebook artifact)
**Formal @spec cells:** none
**Design epic:** `bd-214j` (closed)
**Prior implementation plan:** `docs/superpowers/plans/2026-08-21-temporal-graph-build-performance.md`
**Measured gate task:** `bd-3rku`, worker commit `8dc9239043c0bfeb09f93c2d90248e8e43b9cd65`

**Goal:** Restore a safe serial automatic default, stop the shared parse cache from retaining the entire repository history, and re-measure before considering automatic parallel execution again.

**Architecture:** Explicit CLI and environment overrides remain available, but an unspecified worker count resolves to one. The shared `(Language, BlobOid)` cache keeps once-only initialization for concurrently active commits while the ordinal reducer evicts entries whose last user is already reduced. A final cold Turso sweep determines whether scheduling needs a separate follow-up.

**Tech Stack:** Rust 2021, tree-sitter, scoped threads, `scripts/spur-cargo`, Criterion, Xcode Time Profiler/xctrace.

---

## Measured reason for correction

The jobs=8 implementation is semantically deterministic, but it did not meet the performance gate:

| jobs | result | wall | mean CPU | peak RSS | commits/s |
|---:|---|---:|---:|---:|---:|
| 1 | timeout at 420s | 420.10s censored | 96.7151% | 6,467,878,912 B | censored |
| 2 | timeout at 420s | 420.09s censored | 128.0583% | 7,768,014,848 B | censored |
| 4 | completed | 333.50s | 173.8891% | 7,868,792,832 B | 15.7991 |
| 8 | completed | 251.77s | 235.9971% | 7,707,410,432 B | 20.9278 |

Jobs=8 proves only a `>1.6686x` lower-bound speedup over the censored jobs=1 run, not the required `>=2x`. Its 236% mean CPU misses the 500-800% utilization gate, and 7.71 GB peak RSS is unacceptable for an automatic default. The cache currently documents that successful entries live for the full walk, matching the observed multi-gigabyte retention.

## File map

| File | Responsibility |
|---|---|
| `crates/spur-cli/src/commands/graph.rs` | Precedence and safe automatic temporal-worker resolution |
| `crates/spur-cli/src/main.rs` | User-facing help for explicit opt-in parallelism |
| `crates/spur-graph/src/git_walk.rs` | Shared parse-cache lifetime and reducer-driven eviction |
| `docs/superpowers/plans/2026-08-22-temporal-graph-build-performance-correction.md` | Reproducible cold-run and flamegraph result record |

## Dependency DAG

```text
safe-serial-default ----\
                        +--> cold-turso-revalidation
bounded-parse-cache ---/
```

Tasks 1 and 2 have disjoint write scopes and may run in parallel. Task 3 must use both reviewed commits.

### Task 1: Restore the safe automatic serial default

**Task ID:** `safe-serial-default`

**Files:**

- Modify: `crates/spur-cli/src/commands/graph.rs`
- Modify: `crates/spur-cli/src/main.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] With no CLI value and no `SPUR_GRAPH_TEMPORAL_JOBS`, temporal builds resolve to exactly one worker on 1-, 2-, 8-, and 16-CPU test inputs.
- [ ] `--temporal-jobs N` still overrides `SPUR_GRAPH_TEMPORAL_JOBS`, and the environment still overrides the automatic default.
- [ ] Zero, invalid text, non-UTF-8 environment data, and overflow retain source-specific errors.
- [ ] Non-temporal builds do not read the temporal environment variable or query host parallelism.
- [ ] Help says the automatic default is one and that higher values are an explicit opt-in.
- [ ] The original command remains valid and reports `workers: 1` when no override is present.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: the two files above and their existing unit/parser tests.
- OUT of scope: worker-pool implementation, cache implementation, benchmark files, and unrelated CLI flags.
- Emit `scope_drift` before touching any out-of-scope file.

**Implementation:**

- [ ] **Step 1: Replace the old automatic-fallback expectation with a failing serial-default test.**

```rust
#[test]
fn temporal_jobs_automatic_fallback_is_serial_on_every_host_size() {
    for logical_cpus in [1, 2, 8, 16] {
        assert_eq!(
            resolve_temporal_jobs(None, None, logical_cpus).unwrap(),
            NonZeroUsize::MIN,
            "logical_cpus={logical_cpus}",
        );
    }
}
```

- [ ] **Step 2: Run RED.**

```bash
scripts/spur-cargo test -p spur-cli temporal_jobs_automatic_fallback_is_serial_on_every_host_size -- --nocapture
```

Expected: fail on multi-core inputs because the current fallback selects up to eight workers.

- [ ] **Step 3: Return `NonZeroUsize::MIN` only after CLI and environment precedence have been exhausted.** Keep explicit positive overrides unchanged. Remove the host-parallelism query from the automatic path if it is no longer needed, and update the help text to describe explicit opt-in parallelism.

- [ ] **Step 4: Run GREEN and the scoped suite.**

```bash
scripts/spur-cargo test -p spur-cli graph
scripts/spur-cargo run -p spur-cli -- graph build --help
scripts/spur-cargo check -p spur-cli
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-cli/src/commands/graph.rs crates/spur-cli/src/main.rs
git commit -m "fix(spur-cli): safe-serial-default keep temporal auto mode serial"
```

### Task 2: Evict parse results after their active ordinal window

**Task ID:** `bounded-parse-cache`

**Files:**

- Modify: `crates/spur-graph/src/git_walk.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] Same-key concurrent callers still initialize once and share one immutable `Arc<[ExtractedSymbol]>`.
- [ ] Failed initialization remains retryable and is never retained.
- [ ] Every full-walk cache access records the commit ordinal; standalone `SymbolDiffCtx` behavior remains persistent unless an ordinal-aware walk opts into eviction.
- [ ] After the reducer advances to ordinal `K`, entries whose greatest recorded ordinal is `< K` are removed; entries used by ordinal `K` or later remain.
- [ ] Eviction cannot remove an entry already recorded as in use by an unreduced worker.
- [ ] Cache peak/current-entry telemetry proves the final full-walk cache is empty and its peak is bounded by admitted active work rather than total history on a unique-blob regression fixture.
- [ ] Jobs 1/2/4/8 retain normalized artifact parity, parse-failure diagnostics, and deterministic ordering.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: `SharedParseCache`, `SymbolDiffCtx`, `cached_extract`, the full-walk compute/reducer wiring, `TemporalWalkStats`, and tests in `git_walk.rs`.
- OUT of scope: CLI defaults, worker scheduling/admission-window size, tree-sitter extraction semantics, Parquet code, and new dependencies.
- Emit `scope_drift` if correctness appears to require changing the scheduler or another file.

**Implementation:**

- [ ] **Step 1: Add RED tests for reducer-safe eviction.** Use entries touched at ordinals 2 and 5, evict before 4, and assert that only ordinal 5 remains. Add a concurrent test in which an ordinal-5 caller records its access before an ordinal-2 eviction and still observes one initialization.

```rust
#[test]
fn shared_parse_cache_evicts_only_fully_reduced_ordinals() {
    let cache = SharedParseCache::default();
    let old_key = (Language::Rust, "old".to_owned());
    let active_key = (Language::Rust, "active".to_owned());
    cache
        .get_or_init_at(2, old_key.clone(), || {
            Ok(Ok(vec![cache_test_symbol("old")]))
        })
        .unwrap();
    cache
        .get_or_init_at(5, active_key.clone(), || {
            Ok(Ok(vec![cache_test_symbol("active")]))
        })
        .unwrap();

    cache.evict_before(4);

    assert!(!cache.contains(&old_key));
    assert!(cache.contains(&active_key));
}
```

- [ ] **Step 2: Run RED.**

```bash
scripts/spur-cargo test -p spur-graph shared_parse_cache_evicts_only_fully_reduced_ordinals -- --nocapture
```

Expected: fail because the current cache has no ordinal tracking or eviction API.

- [ ] **Step 3: Add ordinal-aware access and reducer eviction.** Each entry records the greatest commit ordinal before returning its shared initialization cell. `CommitWorkerState` sets the current ordinal before symbol extraction. After `CommitResultReducer::push` advances `next_ordinal`, call `evict_before(next_ordinal)`. Entries created by non-walk callers remain persistent, preserving the public context behavior. Track current and peak entry counts for debug stats.

- [ ] **Step 4: Preserve cache and artifact invariants.** Keep the existing per-key mutex/once-only initialization and failure-removal path. Do not sort or normalize output to hide nondeterminism.

- [ ] **Step 5: Run GREEN and the scoped suite.**

```bash
scripts/spur-cargo test -p spur-graph parse_cache
scripts/spur-cargo test -p spur-graph git_walk
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo test -p spur-graph --test incremental_ingest
scripts/spur-cargo check -p spur-graph --tests --benches
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-graph/src/git_walk.rs
git commit -m "fix(spur-graph): bounded-parse-cache evict reduced ordinal entries"
```

### Task 3: Re-run cold Turso and decide the next bottleneck

**Task ID:** `cold-turso-revalidation`

**Files:**

- Modify: `docs/superpowers/plans/2026-08-22-temporal-graph-build-performance-correction.md`

**Depends on:** `safe-serial-default`, `bounded-parse-cache`

**Acceptance Criteria:**

- [ ] Fresh local `--local --no-hardlinks` Turso clones are used for jobs 1/2/4/8; no graph artifact or commit index is reused.
- [ ] The timeout is at least 900 seconds so jobs=1 produces completed throughput rather than another censored comparison.
- [ ] Record exact wall/user/system time, mean CPU, peak RSS, commit count, commits/s, mode, cutoff status, hardware logical CPUs, source HEAD, binary hash, and command.
- [ ] Run the automatic-default command without the flag and prove it reports `workers: 1`.
- [ ] Capture a no-root xctrace profile for explicit jobs=8, convert it to folded stacks and SVG, and report self, non-recursive inclusive, and recursive unique-stack coverage correctly.
- [ ] Re-run the serial/parallel byte-for-byte parity test and three jobs=8 deterministic constructions.
- [ ] Append artifact paths and SHA-256 hashes to this plan; do not commit trace, folded, SVG, or cloned-repository artifacts.
- [ ] Decide from completed data: automatic mode remains jobs=1. Recommend a later scheduler task only if jobs=8 still shows head-of-line under-utilization after cache retention is fixed.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: this result section and `/tmp` benchmark/profile artifacts.
- OUT of scope: Rust implementation, CLI behavior, benchmark fixture semantics, generated trace/SVG binaries, and automatic-default changes.
- If any acceptance gate fails, record it and emit `risk` or `scope_drift`; do not tune implementation inside this task.

**Implementation:**

- [ ] **Step 1: Verify the stacked implementation before measurement.**

```bash
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo test -p spur-cli graph
scripts/spur-cargo check --workspace
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 2: Build once locally and hash the binary.**

```bash
SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli --profile profiling
shasum -a 256 target/profiling/spur
```

- [ ] **Step 3: Run jobs 1/2/4/8 from fresh clones with a 900-second timeout.** Use the sample flags, explicit `--temporal-jobs N`, `/usr/bin/time -l`, and one unique output directory per run.

- [ ] **Step 4: Run the same command without `--temporal-jobs` and verify `workers: 1`. Then profile explicit jobs=8 with the established no-root xctrace workflow.**

- [ ] **Step 5: Recompute profile coverage.** Use occurrence-summed inclusive coverage only for non-recursive frames. For `collect_symbol_tokens`, use unique-stack coverage:

```sql
SELECT sum(samples)
FROM read_folded('default.folded')
WHERE list_contains(frames, 'spur`spur_graph::extract::languages::collect_symbol_tokens');
```

- [ ] **Step 6: Apply the gate and append the result.** Promotion remains forbidden unless completed jobs=8 is at least `2.0x` jobs=1, reaches 500-800% aggregate CPU, preserves deterministic parity, and stays at or below 4 GiB peak RSS. If it fails, report whether the next measured lever is scheduler head-of-line blocking, extraction work, or another frame.

- [ ] **Step 7: Commit only the result record.**

```bash
git add docs/superpowers/plans/2026-08-22-temporal-graph-build-performance-correction.md
git commit -m "docs(spur-graph): cold-turso-revalidation record bounded-cache profile"
```

## Results

Pending execution. Automatic temporal jobs must remain at one until this section contains completed comparative evidence.
