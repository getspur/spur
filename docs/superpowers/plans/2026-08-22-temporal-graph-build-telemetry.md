# Temporal Graph Build Reuse and Occupancy Telemetry Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `beads://bd-1zip` plus the user-approved 2026-08-22 quack-flamegraph analysis
**Formal @spec cells:** none
**Design epic:** `bd-1zip` (open continuation epic)
**Prior correction plan:** `docs/superpowers/plans/2026-08-22-temporal-graph-build-performance-correction.md`
**Measured baseline:** main `9778a5f771efc1a21349b93329a90d2d6efe46bd`, Turso `a45cd87ff7b25a30476491037a028c43ff95d6f5`

**Goal:** Measure how much temporal graph-build work is cold parsing, re-initialization after ordinal eviction, and worker/reducer waiting before changing cache policy or scheduling.

**Architecture:** Extend the existing internal `TemporalWalkStats`, `WorkerPoolStats`, and `SharedParseCache` debug telemetry. Expensive exact-key reuse tracking is opt-in only when temporal debug telemetry is enabled; the normal build path preserves current eviction, scheduling, output, and memory semantics. A paired cold Turso run compares telemetry off/on and applies the solver-derived decision gates to choose the next one-change performance patch.

**Tech Stack:** Rust 2021, scoped threads, tree-sitter, `tracing`, `scripts/spur-cargo`, Xcode Time Profiler/xctrace, DuckDB 1.5.5, and the local `quack_flamegraph` extension.

---

## Measured basis and hard boundaries

The merged bounded-cache jobs=8 run completed in 778.06 seconds with 1,709.19 CPU-seconds, 219.67% mean CPU, 3,458,859,008 bytes peak RSS, and 6.772 commits/s. The previous unbounded run completed in 251.77 seconds with 594.17 CPU-seconds. The new run therefore performs 2.8766x as much CPU work while using only 6.9% less mean CPU.

The exact cache-initialization closure covers 91.027% of 1,696,270 folded samples. Mutually exclusive query and parent traversal cover 22.989% and 17.910%; cache locking/bookkeeping is about 0.02% self-time. The on-CPU profile cannot distinguish necessary cold parses from reparses after eviction and cannot attribute off-CPU waiting because xctrace used `all-thread-states=NO`.

Solver bounds for restoring the prior 251.77-second wall time are:

- current 2.197 effective cores: eliminate at least 67.64% of total work;
- no work reduction: reach at least 6.789 effective cores;
- balanced at five effective cores: eliminate at least 26.35% of total work.

The resource-rule verification passes at 3,458,859,008 bytes under a 4 GiB limit, leaving 836,108,288 bytes (797.375 MiB) measured headroom. This plan does not consume that headroom as a cache budget; a later cache-policy task must run solve pre/post before selecting any byte cap.

## File map

| File | Responsibility |
|---|---|
| `crates/spur-graph/src/git_walk.rs` | Opt-in cache-reuse, payload-byte, initialization-time, and worker/reducer telemetry plus focused tests |
| `docs/superpowers/plans/2026-08-22-temporal-graph-build-telemetry.md` | Reproducible paired cold-run, quack-flamegraph evidence, and next-patch decision |

## Dependency DAG

```text
temporal-telemetry --> cold-turso-telemetry-decision
```

The tasks are sequential. Task 2 must benchmark the reviewed Task 1 commit and must not tune implementation during measurement.

### Task 1: Add opt-in cache-reuse and time-weighted worker telemetry

**Task ID:** `temporal-telemetry`

**Files:**

- Modify: `crates/spur-graph/src/git_walk.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] Telemetry distinguishes cache hits, first-seen cold initialization, and initialization after a previously evicted key.
- [ ] Reparse telemetry records initialization count, elapsed initialization nanoseconds, and ordinal reuse-distance count/sum/max without changing the cache key or eviction decision.
- [ ] Cache telemetry records successful/failed initialization counts, lock-wait nanoseconds, evicted entries, current/peak retained payload-byte estimates, and current/peak exact ghost-key metadata.
- [ ] Worker telemetry records pool elapsed nanoseconds, summed active-worker nanoseconds, time-weighted average active workers in milli-workers, completed-out-of-order count, admission-window-full receive-wait nanoseconds, next-ordinal-blocked wait nanoseconds, and coordinator-to-static-worker send-blocked nanoseconds.
- [ ] Expensive exact-key ghost tracking is disabled by default and enabled only through the existing debug-telemetry path; disabled mode does not retain evicted keys.
- [ ] Existing scheduling, ordinal admission capacity, static worker assignment, reducer ordering, cache eviction, CLI behavior, and artifacts remain unchanged.
- [ ] Same-key concurrent initialization, failure retry, standalone persistent-cache behavior, jobs 1/2/4/8 parity, cancellation, and bounded occupancy remain green.
- [ ] Every new telemetry semantic has RED then GREEN evidence; duration tests assert monotonic/nonzero relationships rather than brittle exact wall-clock values.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: internal stats/counter structs, `run_bounded_worker_pool`, `SharedParseCache`, reducer/full-walk debug emission, and tests in `git_walk.rs`.
- OUT of scope: eviction policy, cache byte caps, admission-window sizing, ready-worker dispatch, extraction/query logic, CLI flags, dependencies, and other files.
- Emit `scope_drift` before changing another file or altering runtime policy.

**Implementation:**

- [ ] **Step 1: Add a RED cache-classification test.** Construct telemetry-enabled cache state explicitly in the test: initialize one key at ordinal 1, hit it at ordinal 2, evict before 3, and access it at ordinal 7. Assert one cold initialization, one hit, one reparse-after-eviction, two successful initializations, at least one eviction, and a reuse distance of four ordinals.

```rust
#[test]
fn cache_telemetry_distinguishes_cold_hit_and_post_eviction_reparse() {
    let cache = SharedParseCache::with_telemetry();
    let key = (Language::Rust, "reused".to_owned());

    cache.get_or_init_at(1, key.clone(), || {
        Ok(Ok(vec![cache_test_symbol("first")]))
    }).unwrap().unwrap();
    cache.get_or_init_at(2, key.clone(), || unreachable!("cache hit"))
        .unwrap().unwrap();
    cache.evict_before(3);
    cache.get_or_init_at(7, key, || {
        Ok(Ok(vec![cache_test_symbol("second")]))
    }).unwrap().unwrap();

    let stats = cache.telemetry_snapshot();
    assert_eq!(stats.cold_initializations, 1);
    assert_eq!(stats.cache_hits, 1);
    assert_eq!(stats.reparse_initializations, 1);
    assert_eq!(stats.successful_initializations, 2);
    assert_eq!(stats.reuse_distance_max, 4);
}
```

- [ ] **Step 2: Run RED.**

```bash
scripts/spur-cargo test -p spur-graph cache_telemetry_distinguishes_cold_hit_and_post_eviction_reparse -- --nocapture
```

Expected: compile/test failure because the telemetry constructor, snapshot, and classification counters do not exist.

- [ ] **Step 3: Add RED payload and failure tests.** Compute a documented retained-payload estimate from `size_of_val(symbols)` plus owned string/token lengths. Assert current bytes rise after successful initialization, peak is at least current, and current returns to zero after eviction. Assert a failed initialization increments failure telemetry, retains no payload, and the next call remains retryable.

- [ ] **Step 4: Add RED worker timing/event tests.** Extend the existing ordinal-zero stall fixture. Assert `active_worker_nanos > 0`, `pool_elapsed_nanos > 0`, `average_active_workers_milli <= jobs * 1000`, out-of-order completions occur for jobs > 1, and next-ordinal/admission wait accounting is nonzero for the forced stall. Preserve every existing occupancy maximum assertion.

- [ ] **Step 5: Implement the minimal opt-in telemetry.** Keep exact-key ghost metadata only in enabled mode. On a vacant lookup, remove any ghost record before classifying cold versus reparse. Measure initialization around the existing `initialize()` call and worker activity around the existing compute call. Attribute coordinator receive waits from the state immediately before `recv`; do not infer thread state from sampled stacks.

- [ ] **Step 6: Emit one structured debug summary.** Extend the existing `spur-graph: temporal worker-pool occupancy` event with all cache and worker fields, including whether exact reuse telemetry was enabled. Do not log individual blob OIDs or emit per-lookup events.

- [ ] **Step 7: Run GREEN and regression verification.**

```bash
scripts/spur-cargo test -p spur-graph cache_telemetry -- --nocapture
scripts/spur-cargo test -p spur-graph sliding_window_bounds_every_occupancy_class_when_ordinal_zero_stalls -- --nocapture
scripts/spur-cargo test -p spur-graph parse_cache
scripts/spur-cargo test -p spur-graph git_walk
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo test -p spur-graph --test incremental_ingest
scripts/spur-cargo check -p spur-graph --tests --benches
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-graph/src/git_walk.rs
git commit -m "feat(spur-graph): temporal-telemetry measure reuse and stalls"
```

### Task 2: Run paired cold Turso telemetry and choose exactly one next patch

**Task ID:** `cold-turso-telemetry-decision`

**Files:**

- Modify: `docs/superpowers/plans/2026-08-22-temporal-graph-build-telemetry.md`

**Depends on:** `temporal-telemetry`

**Acceptance Criteria:**

- [ ] Build one profiling binary from the reviewed Task 1 base and record HEAD plus binary SHA-256.
- [ ] Use fresh local `--no-hardlinks` Turso clones and unique output directories; every run reports mode Full, 5,269 commits, and explicit jobs=8.
- [ ] Run one telemetry-disabled and one telemetry-enabled timed cold build with at least a 1,200-second timeout, recording wall/user/system, mean CPU, peak RSS, commits/s, page faults/reclaims, context switches, source HEAD, and exact command.
- [ ] Report instrumentation overhead as enabled/off wall, CPU-work, and RSS ratios. If either run is censored or differs in workload output, do not credit a performance comparison.
- [ ] Enabled output includes all required cache and worker counters and no per-key identifiers.
- [ ] Capture one rootless xctrace profile from the enabled build and analyze its folded file through DuckDB plus the local `quack_flamegraph` extension using schema discovery first.
- [ ] Use exact-frame unique-stack coverage for recursive functions and occurrence-summed inclusive values only for non-recursive frames.
- [ ] Record artifact paths and SHA-256 hashes under `/tmp`; commit no trace, clone, folded, SVG, or DuckDB binary.
- [ ] Apply the decision rules below and recommend exactly one next implementation patch, with a new beads issue/plan rather than editing Rust in this measurement task.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: this plan's Results section and fresh `/tmp` measurement/profile artifacts.
- OUT of scope: all Rust source, cache policy, scheduler policy, CLI defaults, benchmark fixtures, and committed binary artifacts.
- Emit `risk` if telemetry overhead prevents a representative profile; emit `scope_drift` before any source edit.

**Implementation:**

- [ ] **Step 1: Verify the reviewed instrumentation and build once.**

```bash
scripts/spur-cargo test -p spur-graph cache_telemetry -- --nocapture
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli --profile profiling
shasum -a 256 target/profiling/spur
```

- [ ] **Step 2: Run paired cold builds.** Use the exact sample flags with `SPUR_EMBEDDING_MODEL=jina-code`, explicit `--temporal-jobs 8`, `/usr/bin/time -l`, and unique graph output. The disabled run must not enable the debug target. The enabled run must enable only the established temporal debug telemetry target and prove `telemetry_enabled=true` in its summary.

- [ ] **Step 3: Capture and fold the enabled xctrace profile.** Use the installed full Xcode CLI, no root, the same source revision and binary hash, and the established `quack-flamegraph` extension at `/Volumes/Projects/Projects/quack-flamegraph/build/debug/quack_flamegraph.duckdb_extension`.

- [ ] **Step 4: Quantify from telemetry and profile.** Report:

  - cold, hit, and reparse initialization counts and nanoseconds;
  - reparse initialization nanoseconds divided by total active-worker nanoseconds;
  - current/peak retained payload estimate and ghost-key count;
  - time-weighted average active workers and worker-capacity utilization;
  - admission-window-full, next-ordinal-blocked, static-send-blocked, and out-of-order metrics;
  - cache initialization, parse, query, parent, token, finalization, and tree-drop exact-frame coverage;
  - recursion-safe `collect_symbol_tokens` coverage and occurrence inflation.

- [ ] **Step 5: Apply one decision rule.**

  1. Choose a byte-budgeted reuse-aware cache next if reparse initialization consumes at least 26.35% of total active-worker time. Its follow-up must run solve pre/post and sweep measured budgets without exceeding 4 GiB RSS.
  2. Otherwise choose ready-worker dispatch plus separately bounded reorder capacity if time-weighted activity is below five workers and next-ordinal/admission waiting accounts for the missing capacity.
  3. Otherwise choose neither: the telemetry model is insufficient, so localize the remaining measured phase before changing policy.

- [ ] **Step 6: Append results and commit only this document.**

```bash
git add docs/superpowers/plans/2026-08-22-temporal-graph-build-telemetry.md
git commit -m "docs(spur-graph): cold-turso-telemetry record reuse decision"
```

## Results

Pending execution. No cache-budget or scheduler-policy patch is authorized by this plan until the paired run selects it through the decision rule.
