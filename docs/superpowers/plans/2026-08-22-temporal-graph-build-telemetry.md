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

- [x] Build one profiling binary from the reviewed Task 1 base and record HEAD plus binary SHA-256.
- [x] Use fresh local `--no-hardlinks` Turso clones and unique output directories; every run reports mode Full, 5,269 commits, and explicit jobs=8.
- [x] Run one telemetry-disabled and one telemetry-enabled timed cold build with at least a 1,200-second timeout, recording wall/user/system, mean CPU, peak RSS, commits/s, page faults/reclaims, context switches, source HEAD, and exact command.
- [x] Report instrumentation overhead as enabled/off wall, CPU-work, and RSS ratios. If either run is censored or differs in workload output, do not credit a performance comparison.
- [x] Enabled output includes all required cache and worker counters and no per-key identifiers.
- [x] Capture one rootless xctrace profile from the enabled build and analyze its folded file through DuckDB plus the local `quack_flamegraph` extension using schema discovery first.
- [x] Use exact-frame unique-stack coverage for recursive functions and occurrence-summed inclusive values only for non-recursive frames.
- [x] Record artifact paths and SHA-256 hashes under `/tmp`; commit no trace, clone, folded, SVG, or DuckDB binary.
- [x] Apply the decision rules below and recommend exactly one next implementation patch, with a new beads issue/plan rather than editing Rust in this measurement task.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: this plan's Results section and fresh `/tmp` measurement/profile artifacts.
- OUT of scope: all Rust source, cache policy, scheduler policy, CLI defaults, benchmark fixtures, and committed binary artifacts.
- Emit `risk` if telemetry overhead prevents a representative profile; emit `scope_drift` before any source edit.

**Implementation:**

- [x] **Step 1: Verify the reviewed instrumentation and build once.**

```bash
scripts/spur-cargo test -p spur-graph cache_telemetry -- --nocapture
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli --profile profiling
shasum -a 256 target/profiling/spur
```

- [x] **Step 2: Run paired cold builds.** Use the exact sample flags with `SPUR_EMBEDDING_MODEL=jina-code`, explicit `--temporal-jobs 8`, `/usr/bin/time -l`, and unique graph output. The disabled run must not enable the debug target. The enabled run must enable only the established temporal debug telemetry target and prove `telemetry_enabled=true` in its summary.

- [x] **Step 3: Capture and fold the enabled xctrace profile.** Use the installed full Xcode CLI, no root, the same source revision and binary hash, and the established `quack-flamegraph` extension at `/Volumes/Projects/Projects/quack-flamegraph/build/debug/quack_flamegraph.duckdb_extension`.

- [x] **Step 4: Quantify from telemetry and profile.** Report:

  - cold, hit, and reparse initialization counts and nanoseconds;
  - reparse initialization nanoseconds divided by total active-worker nanoseconds;
  - current/peak retained payload estimate and ghost-key count;
  - time-weighted average active workers and worker-capacity utilization;
  - admission-window-full, next-ordinal-blocked, static-send-blocked, and out-of-order metrics;
  - cache initialization, parse, query, parent, token, finalization, and tree-drop exact-frame coverage;
  - recursion-safe `collect_symbol_tokens` coverage and occurrence inflation.

- [x] **Step 5: Apply one decision rule.**

  1. Choose a byte-budgeted reuse-aware cache next if reparse initialization consumes at least 26.35% of total active-worker time. Its follow-up must run solve pre/post and sweep measured budgets without exceeding 4 GiB RSS.
  2. Otherwise choose ready-worker dispatch plus separately bounded reorder capacity if time-weighted activity is below five workers and next-ordinal/admission waiting accounts for the missing capacity.
  3. Otherwise choose neither: the telemetry model is insufficient, so localize the remaining measured phase before changing policy.

- [x] **Step 6: Append results and commit only this document.**

```bash
git add docs/superpowers/plans/2026-08-22-temporal-graph-build-telemetry.md
git commit -m "docs(spur-graph): cold-turso-telemetry record reuse decision"
```

## Results

Executed 2026-08-22 from approved Task 1 base
`aaa366cf7211a883b881ef952e165acd74a651b7`. The selected next patch is
**cache only**. No Rust source or runtime policy was changed in this task.

### Provenance and verification

- SPUR commit/tree: `aaa366cf7211a883b881ef952e165acd74a651b7` /
  `e09be538a4481cd99687ebca8feb21dd0ba3b460`; committed
  `git_walk.rs` SHA-256:
  `21185990b024be7e5dd4c0d348cba56404c9a4788bdb06f7d7f170970438b0b1`.
- Turso source commit/tree:
  `a45cd87ff7b25a30476491037a028c43ff95d6f5` /
  `4da9932858e594383fb379cca8f87293cf9848af`; every clone reported 5,269
  first-parent commits and had neither `.spur` nor `.git/spur-graph` before the
  run.
- Frozen arm64 Mach-O profiling binary:
  `/tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7`, SHA-256
  `3fd94f97206bb0ce4deb226769094dd7f97cee68971a092270b73b6dd667d70b`.
  It was produced once successfully with optimized code, debuginfo, and forced
  frame pointers. The prescribed native command and a shared-target retry both
  failed before producing a binary because APFS ran out of space while archiving
  bundled DuckDB. Their exact, recoverable generated files were removed. The
  successful repository-supported Darwin cross-build command was:

```bash
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 \
  scripts/spur-cargo zigbuild -p spur-cli --profile profiling -j 8
SPUR_CLOUD=aws-my SPUR_REMOTE_NAMESPACE=spur \
  /Volumes/Projects/Projects/spur-notebook/scripts/cloud-build/fetch.sh \
  --via-s3 --to /tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7 \
  target/aarch64-apple-darwin/profiling/spur
shasum -a 256 /tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7
```

The verification sequence ran in the prescribed order. Focused telemetry tests
passed 5/5, `temporal_parallel` passed 1/1, check passed with the existing
`spur-graph/src/mcp/mod.rs` dead-code warnings, and formatting passed:

```bash
scripts/spur-cargo test -p spur-graph cache_telemetry -- --nocapture
scripts/spur-cargo test -p spur-graph --test temporal_parallel
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli --profile profiling
CARGO_TARGET_DIR=/Volumes/Projects/Projects/spur/target SPUR_REMOTE=0 \
  scripts/spur-cargo build -p spur-cli --profile profiling
```

The last two commands are the two pre-binary `ENOSPC` attempts described above;
neither completed or emitted the measured binary.

Host: Darwin 23.4.0 arm64, 10 logical/physical CPUs, 32 GiB memory. Tooling:
Xcode xctrace 15.3 (15F31d), `flamegraph` 0.6.14, DuckDB 1.5.5
(`8463eb26527b278109cfddffc4606e1019ebefcfe2eac75efc53c76ed8f282d5`),
Z3 4.16.0, and quack-flamegraph commit
`bcc78d53cbf6e1564490af5da106311b1eedec43` with extension SHA-256
`6679979078c54714808bf30dc28992d9cf5eb21ba3d8fe0c9db941902f235e4b`.

### Exact cold-build commands

Each run began with a distinct local clone:

```bash
git clone --quiet --local --no-hardlinks /Volumes/Projects/Projects/turso \
  /tmp/spur-turso-telemetry.KmIJrb/off/repo
git clone --quiet --local --no-hardlinks /Volumes/Projects/Projects/turso \
  /tmp/spur-turso-telemetry.KmIJrb/on/repo
git clone --quiet --local --no-hardlinks /Volumes/Projects/Projects/turso \
  /tmp/spur-turso-telemetry.KmIJrb/profile/repo
```

Telemetry off:

```bash
/usr/bin/time -l -o /tmp/spur-turso-telemetry.KmIJrb/off/time.txt \
  /opt/homebrew/bin/timeout --signal=TERM --kill-after=10s 1200s \
  /usr/bin/env -u SPUR_GRAPH_TEMPORAL_JOBS -u RUST_LOG \
  SPUR_EMBEDDING_MODEL=jina-code \
  /tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7 graph build \
  --root /tmp/spur-turso-telemetry.KmIJrb/off/repo \
  --output /tmp/spur-turso-telemetry.KmIJrb/off/graph \
  --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
  --no-analyst --temporal-jobs 8
```

Telemetry on:

```bash
/usr/bin/time -l -o /tmp/spur-turso-telemetry.KmIJrb/on/time.txt \
  /opt/homebrew/bin/timeout --signal=TERM --kill-after=10s 1200s \
  /usr/bin/env -u SPUR_GRAPH_TEMPORAL_JOBS \
  SPUR_EMBEDDING_MODEL=jina-code RUST_LOG=spur_graph::git_walk=debug \
  /tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7 graph build \
  --root /tmp/spur-turso-telemetry.KmIJrb/on/repo \
  --output /tmp/spur-turso-telemetry.KmIJrb/on/graph \
  --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
  --no-analyst --temporal-jobs 8
```

Both exited zero and reported the identical workload summary: mode Full, source
commit count 5,269, temporal workers 8, final files 1,620, nodes 59,141, edges
289,851, section rows 1,583, and final code-symbol rows 52,943. The enabled
stderr contained one structured summary with `telemetry_enabled=true` and no
per-key identifier.

### Paired timing and instrumentation cost

| Telemetry | Real s | User s | System s | CPU work s | Mean CPU | Peak RSS bytes | Commits/s | Reclaims | Faults | Voluntary ctx | Involuntary ctx |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| off | 777.82 | 1,664.03 | 37.19 | 1,701.22 | 218.716% | 3,434,250,240 | 6.774061 | 1,388,246 | 8,079 | 1,033,134 | 250,462 |
| on | 771.22 | 1,658.39 | 39.77 | 1,698.16 | 220.191% | 3,442,720,768 | 6.832032 | 1,382,329 | 8,074 | 1,034,887 | 241,256 |

Enabled/off ratios were wall `0.991515x`, CPU work `0.998201x`, RSS
`1.002466x`, and throughput `1.008558x`. This single pair does not establish a
telemetry speedup; it shows no measurable positive instrumentation overhead.
Neither run was censored and their workload summaries match, so the enabled
profile is representative. No `risk` signal was required.

### Enabled telemetry accounting

Cache classification covered 247,899 lookups: 133,958 hits (54.0373%), 22,530
cold initializations (9.0884%), and 91,411 reparses after eviction (36.8743%).
There were 113,941 successful and zero failed initializations; all 113,941
initialized entries were eventually evicted.

| Cache counter | Value |
|---|---:|
| total / cold / reparse initialization time | 1,534.530646 / 427.525594 / 1,107.005053 s |
| cache-hit / initialization-lock-wait time | 627.404833 / 628.739238 s |
| reparse / cold / all initialization share of active-worker time | 49.4837% / 19.1106% / 68.5943% |
| current / peak cache entries | 0 / 1,608 |
| current / peak retained payload estimate | 0 / 43,368,240 bytes (41.3592 MiB) |
| reuse-distance count / sum / max / mean | 91,411 / 3,175,797 / 4,702 / 34.742 ordinals |
| current / peak exact ghost keys | 22,530 / 22,530 |

The initialization-lock value is emitted as `cache_lock_wait_nanos`; it is the
plan's initialization-lock-wait counter. Durations are summed across concurrent
calls and therefore are not additive wall-clock phases.

| Worker/reducer counter | Value |
|---|---:|
| pool elapsed / summed active-worker time | 733.372648 / 2,237.110903 s |
| time-weighted average active workers | 3.050 of 8 (38.125% capacity utilization) |
| completed out of order | 3,755 (71.2659% of commits) |
| admission-window-full receive wait | 671.749076 s (91.5972% of pool elapsed) |
| next-ordinal-blocked wait | 648.013143 s (88.3607% of pool elapsed) |
| coordinator send blocked | 0.011538 s (0.001573% of pool elapsed) |
| max in-flight / queued / active / result occupancy / reducer pending | 8 / 8 / 8 / 8 / 7 |

Admission and next-ordinal waits describe overlapping coordinator states and
must not be summed. Scheduler evidence exists (3.05 average workers and 1.95
workers below the five-worker gate), but the ordered cache rule is evaluated
first.

### Rootless xctrace and quack-flamegraph accounting

The profile used the same binary and enabled target from a third fresh clone:

```bash
/usr/bin/time -l -o /tmp/spur-turso-telemetry.KmIJrb/profile/xctrace-time.txt \
  /usr/bin/env -u SPUR_GRAPH_TEMPORAL_JOBS \
  DEVELOPER_DIR=/Applications/Xcode-15.4.0.app/Contents/Developer \
  xcrun xctrace record --template 'Time Profiler' --time-limit 1200s \
  --output /tmp/spur-turso-telemetry.KmIJrb/profile/enabled.trace \
  --no-prompt \
  --target-stdout /tmp/spur-turso-telemetry.KmIJrb/profile/stdout.txt \
  --env SPUR_EMBEDDING_MODEL=jina-code \
  --env RUST_LOG=spur_graph::git_walk=debug --launch -- \
  /tmp/spur-turso-telemetry.KmIJrb/spur-aaa366cf7 graph build \
  --root /tmp/spur-turso-telemetry.KmIJrb/profile/repo \
  --output /tmp/spur-turso-telemetry.KmIJrb/profile/graph \
  --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
  --no-analyst --temporal-jobs 8
```

It completed Full in 854.39 seconds including xctrace launch/save, with 1,754.38
CPU-seconds and 3,491,463,168-byte peak RSS. Its reparse count differed from the
timed enabled run by only +0.3380%, and its reparse/active-worker ratio was
49.3039% versus 49.4837%, supporting representativeness.

The trace was ZIP-preserved before conversion, and the disposable copy was
folded with:

```bash
XCTRACE=/Applications/Xcode-15.4.0.app/Contents/Developer/usr/bin/xctrace \
  /Users/vutch/.cargo/bin/flamegraph --deterministic \
  --title 'spur graph build — Turso temporal telemetry enabled' \
  --subtitle 'Xcode Time Profiler, fresh clone, jobs=8, 1200s bound' \
  --post-process \
  '/usr/bin/tee /tmp/spur-turso-telemetry.KmIJrb/profile/enabled.folded' \
  --perfdata /tmp/spur-turso-telemetry.KmIJrb/profile/for-flamegraph.trace \
  -o /tmp/spur-turso-telemetry.KmIJrb/profile/enabled.svg
```

DuckDB analysis used official CLI 1.5.5 and the exact local extension. Schema
discovery preceded every analysis query:

```bash
/tmp/spur-turso-telemetry.KmIJrb/duckdb-v1.5.5/duckdb -unsigned \
  /tmp/spur-turso-telemetry.KmIJrb/quack-analysis.duckdb -c "
  LOAD '/Volumes/Projects/Projects/quack-flamegraph/build/debug/quack_flamegraph.duckdb_extension';
  SELECT function_name, function_type, parameters, parameter_types
  FROM duckdb_functions()
  WHERE function_name IN ('read_folded','flamegraph_coverage',
    'flamegraph_edges','flamegraph_exclusive','flamegraph_hot_stacks',
    'flamegraph_inclusive') ORDER BY function_name;
  DESCRIBE SELECT * FROM read_folded(
    '/tmp/spur-turso-telemetry.KmIJrb/profile/enabled.folded');
  DESCRIBE SELECT * FROM flamegraph_coverage(
    '/tmp/spur-turso-telemetry.KmIJrb/profile/enabled.folded');
  DESCRIBE SELECT * FROM flamegraph_inclusive(
    '/tmp/spur-turso-telemetry.KmIJrb/profile/enabled.folded');"
```

Discovery showed `read_folded(frames VARCHAR[], samples BIGINT, source VARCHAR)`
and the quack table macros before materialization. The folded corpus has 8,911
stacks and 1,689,971 samples. Exact-frame results below overlap and are not
additive. Occurrence-summed inclusive values are used only where the observed
maximum is one occurrence per stack. Repeated frames use exact-frame
unique-stack coverage.

| Phase (exact frame) | Method | Max occurrences/stack | Samples | Coverage |
|---|---|---:|---:|---:|
| cache scope (`spur_graph::git_walk::cached_extract`) | occurrence-summed inclusive | 1 | 1,545,811 | 91.4697% |
| extract (`spur_graph::extract::tree_sitter::BytesExtractor::extract`) | occurrence-summed inclusive | 1 | 1,538,590 | 91.0424% |
| parse (`ts_parser_parse_with_options`) | exact-frame unique-stack | 2 | 421,245 | 24.9262% |
| query (`<tree_sitter::QueryMatches<T,I> as streaming_iterator::StreamingIterator>::advance`) | occurrence-summed inclusive | 1 | 376,590 | 22.2838% |
| parent traversal (`ts_node_child_with_descendant`) | occurrence-summed inclusive | 1 | 291,936 | 17.2746% |
| parent primitive (`ts_node_parent`) | occurrence-summed inclusive | 1 | 290,369 | 17.1819% |
| token collection (`spur_graph::extract::languages::collect_symbol_tokens`) | exact-frame unique-stack | 255 | 236,773 | 14.0105% |
| finalization (`spur_graph::store::build::buckets_from_facts`) | occurrence-summed inclusive | 1 | 8,308 | 0.4916% |
| tree drop (`ts_tree_delete`) | occurrence-summed inclusive | 1 | 40,323 | 2.3860% |

For recursive `collect_symbol_tokens`, occurrence summing produces 2,332,979
samples (138.0485% of the profile), a 9.853231x inflation; that value is retained
only as the recursion diagnostic and is not used as inclusive phase coverage.
The repeated parse wrapper similarly uses unique-stack coverage, although its
occurrence inflation is only 1.000012x. The final method audit reports zero
non-recursive targets with more than one occurrence and zero repeated targets
using occurrence-summed reporting.

### Solver decision: cache only

The decision encoding maps `0=cache`, `1=scheduler`, and `2=neither`. It fixes
the measured reparse share at 4,948 basis points, average activity at 3,050
milli-workers, and the observed missing-capacity wait predicate to true, then
applies the plan's ordered rules:

```bash
/opt/homebrew/bin/z3 -T:30 \
  /tmp/spur-turso-telemetry.KmIJrb/solver-decision.smt2
```

Z3 4.16.0 returned `sat` with `decision = 0`. Therefore recommend exactly one
next implementation patch: **a byte-budgeted reuse-aware cache**. Its separate
follow-up issue/plan must run solve before and after the change and sweep measured
byte budgets without exceeding 4 GiB RSS. Do not combine ready-worker dispatch,
reorder-capacity changes, or any other scheduler tuning with that patch. The
worker environment exposed no issue-creation mutation (`bd` was absent and the
worker MCP is read-only for issue state), so the orchestrator must materialize
this sole selected follow-up as the new beads issue/plan; no second recommendation
was emitted and no Rust was edited here.

### Artifacts and completion audit

All artifacts live under `/tmp/spur-turso-telemetry.KmIJrb` (3.9 GiB). The full
checksum manifest is
`/tmp/spur-turso-telemetry.KmIJrb/artifact-sha256.txt`, SHA-256
`44817a78a6d44e1888aa9ca6139501fcc7eeb7822829e3cd0a64c88b99813856`;
every ordinary file entry passed `shasum -a 256 -c`.

| Artifact | SHA-256 |
|---|---|
| `off/time.txt` | `0f79f00e8940277604a27cf97aa851341720c7dddb34dca430bc9704552d5beb` |
| `on/time.txt` | `d691f11ca50c3ebf8f02865426544939f4e0d7da71b5798a79f53624aa45c520` |
| `on/stderr.txt` (telemetry) | `81f7e5e49fe6a9d5f4ca71a90f617b76962ac1c5ebd0fbb70ea18eae0f3ffd2f` |
| `profile/enabled.trace.zip` | `3849da7b2bcf5bcb72a5020a2f86e751608df1edf7d5d2212e0f36899e91804e` |
| `profile/time-profile.xml` | `4a69d53bacc0269849e9cba2554859e5a5a5b32420fa290a84c41e078ea5858b` |
| `profile/enabled.folded` | `e815bf95e72b517efed3e5a423bff533c3b00aca9e27b64f6207b2cc85fe097e` |
| `profile/enabled.svg` | `fa6aec69d58eeb26245554ffeb8e8d95228756df2bcc15331ca5ae21b42edf26` |
| `profile/reported-exact-frame-metrics.csv` | `5e2530557a75abe5fd405e4c6a4cb3c9cb11674de9b840ee73eef339c88af5fe` |
| `quack-analysis.duckdb` | `8062583cfa47f846685e8fd3477b25411141267f1448ecf9608fb0ee90e99283` |
| `solver-decision.smt2` / result | `0a104a9e667893c1107742c44f06d6ced28e10773a9d75e3ba68e359c30eee00` / `2ee9463e5ba88ab82c5998f2276690edc52b6cd4783e8babfc394286aff0f905` |

Completion audit: base `aaa366cf7` and Turso `a45cd87f` were reverified;
three independent cold clones and graph outputs remain under `/tmp`; timed runs
and profile are uncensored Full jobs=8 builds; telemetry overhead preserves
profile validity; every required counter is present; schema discovery precedes
quack analysis; recursion handling passes its method audit; the solver selects
one cache recommendation; only this plan document is modified; no
`scope_drift` or `risk` condition occurred.
