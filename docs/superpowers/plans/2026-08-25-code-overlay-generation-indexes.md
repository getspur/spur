# Generation-Scoped Incremental Overlay Indexes Implementation Plan

> **Execution rule:** Every task is delegated to the `codex` worker with model
> `gpt-5.6-sol` and reasoning effort `xhigh`. Workers must follow strict
> RED -> GREEN TDD and record a SOLVE PRE result before production edits and a
> SOLVE POST result after verification.

**Design source:**
`docs/superpowers/specs/2026-08-25-code-overlay-generation-indexes-design.ipynb`

**Approved design epic:** `bd-20iv` (closed)

**Formal design contracts:**

- `OVERLAY-GENERATION-ROUTING` — 6/6 cases proved.
- `OVERLAY-GENERATION-UPDATE-SCOPE` — 6/6 cases proved.
- `OVERLAY-GENERATION-PUBLICATION` — 10/10 cases proved.
- Z3 Optimize solution `sol_24852081d34c479b` selected
  `generation_incremental_combined_indexes` as the complete lexicographic
  optimum.

## Goal

Remove repeated overlay merge/filter/sort/deduplicate work from warm `code_*`
requests. Build one immutable overlay generation from the base graph and the
current Git snapshot, update only paths whose Git state changed, publish it
atomically, and pin it for the full request. Preserve the existing exact
request-scoped `OverlayClient` as a fail-closed fallback.

The optimization is successful only when an identical warm generation performs
zero overlay finalization stages per query. Query-specific ranking, sorting, and
top-k selection remain normal search work and must not be misreported as overlay
finalization.

## Architecture

`OverlayGeneration` owns persistent per-file symbol and edge segments. An
unchanged generation reuses all segment `Arc`s. A new generation replaces,
removes, or restores only segments for the complete current changed-path set.
It also owns precomputed selector and caller/callee indexes, so a query sees one
logical graph rather than separate base and delta result vectors.

`OverlayGenerationCache` is keyed by the exact `SnapshotIdentity` and bounded by
the existing overlay cache capacity. It has no TTL. A compatible previous
generation for the same canonical worktree and base graph may seed an
incremental update, but the newly built generation is published only after all
indexes are complete. Concurrent identical builders collapse through
singleflight.

The MCP handler acquires one `Arc<OverlayGeneration>` before invoking a
`code_*` handler. Nested subgraph calls use that same generation. Git fsmonitor
is only an invalidation hint; exact Git state remains authoritative.

## Non-negotiable protocol for every task

1. Read this plan, the approved design notebook, and the listed source files.
2. Record SOLVE PRE by reloading the relevant approved rule family/model and
   proving the task invariant against the pre-change implementation. If the
   invariant is intentionally false before implementation, capture the
   counterexample.
3. Add the smallest behavioral test that fails for the intended reason. Run it
   with `scripts/spur-cargo`; never invoke bare `cargo`.
4. Commit the RED test separately using the repository commit convention.
5. Implement only the task's scoped files. Preserve unrelated worktree changes.
6. Run focused tests, crate tests, formatting, and any task-specific benchmark.
7. Record SOLVE POST against the implemented behavior and show that the PRE
   counterexample is eliminated without weakening routing, update-scope, or
   publication constraints.
8. Commit GREEN only after all required checks pass. Leave a clean worktree and
   report RED/GREEN commit IDs, commands, timings, and SOLVE report IDs.

## Dependency DAG

```text
task-1-baseline-contract
  -> task-2-symbol-index
    -> task-3-edge-index
      -> task-4-generation-cache
        -> task-5-mcp-integration
          -> task-6-post-matrix
```

The chain is deliberate: each task fixes an interface consumed by the next, and
Tasks 1 and 6 must use the same measurement protocol.

---

### Task 1: Establish the overlay-finalization contract and PRE baseline

**Files:**

- Modify: `crates/spur-graph/src/query_client.rs`
- Modify: `docs/superpowers/plans/2026-08-25-code-overlay-generation-indexes.md`

**SOLVE PRE invariant:** A non-identity request-scoped `OverlayClient` currently
performs each of these stages once per search invocation: shadow filtering,
base/delta merging, overlay ordering, and stable-ID deduplication. Identity
requests perform none. Encode the stage cardinalities and capture the expected
non-zero counterexample for a repeated warm query.

**RED:** Add a test-only/publicly observable measurement seam with explicit
stage names. First write tests that expect the four non-identity stages to be
reported and identity stages to remain zero; verify the tests fail because no
measurement contract exists.

Suggested API shape (names may be adjusted locally, semantics may not):

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OverlayFinalizationMeasurements {
    pub shadow_filters: u64,
    pub result_merges: u64,
    pub overlay_sorts: u64,
    pub stable_id_deduplications: u64,
}

impl OverlayFinalizationMeasurements {
    pub fn total(self) -> u64 {
        self.shadow_filters
            + self.result_merges
            + self.overlay_sorts
            + self.stable_id_deduplications
    }
}
```

Do not add production logging on every request. Prefer an optional counter sink
or an internal counted execution method that benchmarks and tests can enable.

**PRE measurement:** Run the existing stable overlay probe for at least three
repetitions per selected project/session. Append the exact command, project
identity, base graph identity, changed-path count, warm/cold classification,
p50/p95, correctness digest, and stage counts under the evidence section below.
Do not mix these samples with older protocols.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph query_client::tests -- --nocapture
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Verify that the measurement labels are mutually exclusive,
identity totals are zero, non-identity totals match the actual structural
operations, and query-ranking sort is not counted as overlay sort.

**Commits:**

```text
test(spur-graph): task-1 expose overlay finalization contract
chore(spur-graph): task-1 record overlay PRE baseline
```

---

### Task 2: Build the persistent symbol/file/search overlay generation

**Files:**

- Create: `crates/spur-graph/src/overlay_generation.rs`
- Modify: `crates/spur-graph/src/lib.rs`
- Modify: `crates/spur-graph/tests/overlay_client.rs`

**SOLVE PRE invariant:** For every path, exactly one visible source is selected:
base, replacement delta, untracked delta, or deletion. For an update from
generation `g0` to `g1`, the set of rebuilt file segments must be a subset of
the complete current changed-path set plus paths whose prior changed state was
restored. Capture that the requested generation type is absent before GREEN.

**RED:** Add oracle tests covering:

- unchanged base symbols;
- modified-file shadowing;
- deletion;
- untracked addition;
- rename as delete plus add;
- restore-to-base;
- stable-ID uniqueness;
- exact search and selector equality with a freshly rebuilt graph oracle;
- pointer reuse for every unaffected per-file segment.

The tests must fail because `OverlayGeneration` does not yet exist.

**GREEN:** Implement an immutable generation seeded once from
`Arc<GraphIndexArtifact>`. Store reusable per-file `Arc` segments and layered or
persistent lookup patches. Updating a generation must not clone or rescan every
base symbol. Apply the full current `OverlayPathState` set so missed monitor
events cannot preserve stale data.

Provide one logical iteration/search view. Search may score and top-k candidates
for the current query, but it must not construct separate base and delta result
vectors and then shadow-filter/merge/deduplicate them.

Suggested public boundary:

```rust
pub struct OverlayGeneration { /* immutable indexes and identity */ }

impl OverlayGeneration {
    pub fn seed(base: Arc<GraphIndexArtifact>) -> Result<Self>;
    pub fn update(
        previous: &Arc<Self>,
        identity: SnapshotIdentity,
        path_state: &BTreeMap<String, OverlayPathState>,
        delta: Arc<GraphIndexArtifact>,
    ) -> Result<Self>;
    pub fn identity(&self) -> &SnapshotIdentity;
}
```

Keep MCP-specific cache mechanics out of this module.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo test -p spur-graph overlay_generation -- --nocapture
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Verify all routing cases, changed-set containment, single visible
source per path, stable-ID uniqueness, and unaffected-segment reuse. The fresh
oracle and generation digests must be equal in every test case.

**Commits:**

```text
test(spur-graph): task-2 specify persistent overlay symbols
feat(spur-graph): task-2 add persistent overlay symbol indexes
```

---

### Task 3: Add edge adjacency and resolution indexes

**Files:**

- Modify: `crates/spur-graph/src/overlay_generation.rs`
- Modify: `crates/spur-graph/tests/overlay_client.rs`

**SOLVE PRE invariant:** Every visible edge endpoint resolves against the same
generation as its source symbol; deleted/replaced symbols cannot leak base
caller/callee edges. Capture the missing adjacency implementation as the PRE
counterexample.

**RED:** Extend the oracle matrix for callers, callees, resolve-symbol,
get-symbol, list-files, and nested subgraph behavior. Include changed definitions,
deleted endpoints, new callers, rename, unresolved-label remap, and stable-ID
collision cases. Expect exact equality with a fresh rebuilt oracle.

**GREEN:** Add persistent caller/callee adjacency, unresolved-label/remap, and
selector indexes to `OverlayGeneration`. Implement `GraphQueryClient` for an
`Arc<OverlayGeneration>`-backed client so all graph operations read one
generation. Updates may rebuild adjacency segments touching changed endpoints,
but must reuse unaffected segments.

Do not route search through the old `OverlayClient`. Do not introduce a second
cache here.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo test -p spur-graph overlay_generation -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Prove endpoint visibility, adjacency symmetry where required,
no deleted-edge leakage, exact oracle equivalence, and request-generation
coherence for a nested traversal.

**Commits:**

```text
test(spur-graph): task-3 specify overlay generation edges
feat(spur-graph): task-3 add overlay adjacency indexes
```

---

### Task 4: Cache and atomically publish exact overlay generations

**Files:**

- Modify: `crates/spur-graph/src/mcp/request_cache.rs`

**SOLVE PRE invariant:** Exact `SnapshotIdentity` equality is necessary and
sufficient for a cache hit. A builder may reuse a previous generation only when
canonical worktree and base graph identity are compatible. Publication occurs
after all indexes are complete. Capture the absent generation cache before
GREEN.

**RED:** Add tests for:

- exact identity hit without rebuild;
- invalidation for each identity component;
- separation between worktrees;
- concurrent identical requests building once;
- no partial generation visibility during publication;
- bounded LRU eviction without wrong-identity reuse;
- no TTL expiry;
- latest compatible generation passed to the incremental builder;
- incompatible base generation rejected as a seed.

**GREEN:** Add `CachedOverlayGeneration` and a singleflight LRU keyed by exact
`SnapshotIdentity`. Reuse the existing overlay cache capacity constant; do not
select a new quota by feel. Maintain a compatible-latest pointer only as a build
seed. It must never satisfy an exact lookup by itself.

The builder closure must receive an optional compatible generation and return a
fully constructed immutable `Arc<OverlayGeneration>`. Publish the `Arc` only
after successful construction. Failed or cancelled builds leave the previous
generation valid and wake waiters into exact fallback/retry behavior.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph request_cache::tests -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Verify cache-hit equivalence, worktree isolation, builder
cardinality of one for identical concurrency, capacity bound, atomic publication,
and the absence of any time/TTL variable in hit validity.

**Commits:**

```text
test(spur-graph): task-4 specify overlay generation cache
feat(spur-graph): task-4 cache immutable overlay generations
```

---

### Task 5: Route `code_*` MCP requests through one pinned generation

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`

**SOLVE PRE invariant:** One handler invocation acquires exactly one generation
and every nested query uses it. Generation construction happens at most once for
an exact identity. Unsupported, failed, or over-budget generation construction
uses the existing exact `OverlayClient` path. Capture that the current handler
constructs a request-scoped overlay client and repeats finalization.

**RED:** Add module tests for:

- two sequential `code_*` queries reusing one generation ID;
- nested subgraph calls retaining that generation ID;
- generation route reports zero overlay finalization stages;
- concurrent requests publish/acquire one complete generation;
- new or renamed symbols trigger the existing not-found retry semantics;
- fsmonitor hint mismatch is corrected by authoritative Git state;
- generation build failure preserves exact fallback output;
- Off mode and identity-overlay behavior are unchanged.

**GREEN:** Extend `CodeSearchBackend` with a helper that supplies the full base
artifact once: clone the in-memory artifact, or load Parquet with
`read_artifact_parquet(client.dir())`. Extend the changed-path snapshot boundary
only as needed to pass the complete authoritative path state into generation
construction.

Acquire a cached generation in `overlay_response_for_backend`, pin the returned
`Arc` for the whole handler call, and invoke the handler with the generation
client. Preserve the current request-scoped `OverlayClient` as an exact fallback.
Do not change `/configure` Off/Auto semantics and do not make fsmonitor a
correctness authority.

Expose bounded diagnostic metadata/counters usable by Task 6: route,
generation ID, build/reuse, changed segment count, and overlay finalization
stage counts. Avoid unbounded labels and per-symbol logs.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph mcp::tests -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Verify routing completeness/exclusivity, one pinned generation
per request, at-most-one exact build, exact fallback reachability, authoritative
validation after monitor hints, and zero overlay finalization stages on the
generation route.

**Commits:**

```text
test(spur-graph): task-5 specify generation routed MCP queries
feat(spur-graph): task-5 route code tools through overlay generations
```

---

### Task 6: Run comparable POST matrix and enforce the release gate

**Files:**

- Modify: `crates/spur-graph/benches/overlay.rs`
- Modify: `crates/spur-graph/tests/perf_gates.rs`
- Modify: `docs/superpowers/plans/2026-08-25-code-overlay-generation-indexes.md`

**SOLVE PRE invariant:** The evidence is admissible only when PRE and POST use
the same projects, graph identities, dirty states, query sequences, warmup,
repetition count, timing boundaries, and digest algorithm. The release gate
must reject missing generation IDs, mismatched digests, non-zero warm overlay
finalization, or fewer than three measured repetitions.

**RED:** Add a performance-evidence gate that fails on the PRE/current route
because warm repeated queries report overlay finalization stages or lack
generation reuse evidence. The gate must validate evidence structure and
correctness, not assert an unrealistically fixed wall-clock threshold.

**GREEN:** Extend the overlay benchmark to measure, separately:

- direct Parquet baseline;
- current exact request-scoped `OverlayClient` oracle;
- cold generation construction;
- warm generation reuse across the same representative sequential `code_*`
  query sequence;
- incremental generation update after a bounded Git change;
- exact fallback.

Run at least three measured repetitions for every PRE project/state cell; prefer
the existing 30-sample stable protocol when fixtures remain available. Report
p50/p95, cold/warm classification, generation build count and ID, changed
segment count, stage counts, response digest, and mismatch count. The timing
boundary must include handler-visible work and exclude fixture setup equally in
PRE and POST.

Append raw commands and summarized evidence below. Preserve raw machine-readable
artifacts under the existing `.spur/bench-evidence/` convention without
committing machine-specific bulk output.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph --test perf_gates -- --nocapture
scripts/spur-cargo bench -p spur-graph --bench overlay -- --noplot
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo clippy -p spur-graph --all-targets -- -D warnings
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Re-run the approved architecture objectives and the evidence
admissibility model. Release is approved only if:

1. all correctness digests match and mismatch count is zero;
2. warm repeated generation queries report zero overlay finalization stages;
3. identical warm requests reuse one exact generation;
4. incremental work is contained to the changed-path dependency closure;
5. fallback remains exact and tested;
6. PRE and POST matrices are comparable.

If any condition fails, record the counterexample and leave the feature behind
the existing default-Off configuration. Do not reinterpret an inconclusive
matrix as a performance win.

**Commits:**

```text
test(spur-graph): task-6 gate overlay generation evidence
chore(spur-graph): task-6 validate incremental overlay generations
```

---

## PRE/POST Evidence

Task 1 records the new-protocol PRE rows here. Task 6 appends the exactly
comparable POST rows and the final SOLVE release verdict. Historical numbers
from the earlier fsmonitor-cache plan are context only and are not admissible as
this plan's PRE unless their complete protocol identity is proven equal.

### PRE

Task 1 measured the existing request-scoped overlay search on the Spur worker
worktree. The selected session used exact query `handle_code_search`, limit 20,
default filters, and one explicit changed path
(`crates/spur-graph/src/query_client.rs`). Each repetition warmed the prebuilt
overlay for one second, then collected 30 Criterion samples / 90 measured query
iterations over a one-second measurement window. Percentiles below are
nearest-rank percentiles of the raw per-iteration sample times
(`times[i] / iters[i]`), not Criterion confidence-interval endpoints.

Project/protocol identity:

- repository:
  `/Volumes/Projects/Projects/spur/.spur/worktrees/516c98c3-8a9b-44cc-a20f-c2a1e1867c9c`
- source revision: `b7fff63f41832de154c1fb83951fd0a5f4fcb7ba`
  plus the uncommitted GREEN measurement seam in `query_client.rs`
- tracked files: 3,222; probe dirty records: 1; selected changed-path count: 1
- base artifact:
  `/Volumes/Projects/Projects/spur/.spur/graph/acd60905a3a40accf38792d2e7ce41e37e58bd4ad0d4e62624b069d57a02b832.parquet`
- base graph content hash:
  `acd60905a3a40accf38792d2e7ce41e37e58bd4ad0d4e62624b069d57a02b832`
- artifact indexed commit OID: `null` (not present in this artifact manifest);
  indexed source files: 3,434
- classification: warm overlay query; one-second warm-up before every
  repetition

The probe command was run three times with only the final saved-baseline name
changed from `task1-pre-r1` through `task1-pre-r3`. `GIT_INDEX_FILE` pointed to
a disposable copy of the worktree index with the host-only Cargo lockfile
refresh marked assume-unchanged, keeping the measured Git snapshot at the one
intentional changed path. The disposable index and lockfile refresh were
removed after the probe.

```bash
GIT_INDEX_FILE=/tmp/spur-task1-index.OUzSBo/index \
SPUR_REMOTE=0 \
SPUR_GRAPH_PERF_REPO=/Volumes/Projects/Projects/spur/.spur/worktrees/516c98c3-8a9b-44cc-a20f-c2a1e1867c9c \
SPUR_GRAPH_PERF_FIXTURE=/Volumes/Projects/Projects/spur/.spur/graph/CURRENT \
SPUR_GRAPH_PERF_CHANGED_FILE=crates/spur-graph/src/query_client.rs \
SPUR_GRAPH_PERF_LABEL=task1_pre_spur \
SPUR_GRAPH_PERF_SAMPLE_SIZE=30 \
SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=1 \
scripts/spur-cargo bench --locked -p spur-graph --bench overlay -- \
  overlay_stage_probe_task1_pre_spur/stage_overlay_query --noplot \
  --save-baseline task1-pre-r1
```

| Repetition | Samples / iterations | p50 (ms) | p95 (ms) | Correctness digest | Per-query finalization stages | Measured-window stage counts |
|---|---:|---:|---:|---|---|---|
| `task1-pre-r1` | 30 / 90 | 13.684097 | 13.938806 | `37d1ae135cd80b90589253cd74df41bd88389b2cfc13de96907ff62700985936` | shadow=1, merge=1, overlay-sort=1, stable-ID-dedup=1; total=4 | 90 / 90 / 90 / 90; total=360 |
| `task1-pre-r2` | 30 / 90 | 13.585958 | 14.999069 | `37d1ae135cd80b90589253cd74df41bd88389b2cfc13de96907ff62700985936` | shadow=1, merge=1, overlay-sort=1, stable-ID-dedup=1; total=4 | 90 / 90 / 90 / 90; total=360 |
| `task1-pre-r3` | 30 / 90 | 13.618861 | 15.516486 | `37d1ae135cd80b90589253cd74df41bd88389b2cfc13de96907ff62700985936` | shadow=1, merge=1, overlay-sort=1, stable-ID-dedup=1; total=4 | 90 / 90 / 90 / 90; total=360 |

The stage-count contract is the counted `OverlayClient` execution seam tested
against the same non-identity search path. Identity overlay searches report
zero for all four labels. `overlay_sorts` counts only the sort that orders the
separately queried base and delta vectors as one overlay result; ranking sorts
inside either query client are intentionally excluded.

SOLVE evidence:

- PRE `sol_165c862dc07941fd` (persisted and reloaded):
  `data_integrity.cardinality` passed all eight bindings. Three repeated warm
  non-identity searches contain three occurrences of each stage (12 total),
  while all four identity relations have cardinality zero. This is the expected
  non-zero counterexample to the approved zero-finalization target.
- POST `sol_b6bf8aa93db74d7c` (persisted and reloaded): all nine cardinality
  bindings plus `data_integrity.unique` and
  `data_integrity.mutually_consistent` passed. The four labels are unique and
  drawn only from shadow filter, result merge, overlay sort, and stable-ID
  deduplication; `query_ranking_sort` is outside the allowed overlay-label
  relation. Task 1 intentionally measures rather than removes the PRE
  counterexample; Task 6 must reduce these four warm-path counts to zero.

### POST

Task 6 ran the release matrix on 2026-08-26 at the reviewed Task 5 tip
`6b5db470e2a88dd5669520a79473c8ab2b912d11`, plus the Task 6 benchmark and
gate commits. The protocol ID is `overlay-generation-task6-v1`: exact query
`matrix_target`, limit 20, compact response format, the same base artifact and
changed-path snapshot within each cell, 30 repetitions per latency case, and
nearest-rank p50/p95. Fixture construction is excluded from every timing.

The raw report is preserved in the repository runtime evidence store at
`/Volumes/Projects/Projects/spur/.spur/bench-evidence/task6-overlay-generation-matrix-29fa6d9973eb.json`
(2,322 lines, 67,064 bytes, SHA-256
`29fa6d9973eb7ab464aae81ccc3710ec1d9a5a2d50e9de1ca7168191d22b7a5f`).
It contains all 30 raw samples for each case, generation identities, operation
and finalization counters, changed paths, dependency closures, result digests,
fallback reasons, and fixture manifests. The full run was:

```bash
SPUR_GRAPH_TASK6_MATRIX=1 \
SPUR_GRAPH_RELEASE_REPEATS=30 \
scripts/spur-cargo bench -p spur-graph --bench overlay -- \
  task6_matrix_only --noplot
# release_eligible=true; fsmonitor_auto_safe=true;
# configuration_default=Off; configure_semantics_changed=false
```

Representative deterministic projects deliberately differ in size and change
shape:

| Project | Tracked / source files | Languages | Initial change shape | Initial changed / rebuilt segments |
|---|---:|---|---|---:|
| `small_untracked_heavy` | 5 / 4 | Rust | 12 untracked Rust files | 12 / 12 |
| `medium_dirty_rust` | 49 / 48 | Rust | 5 modified tracked Rust files | 5 / 5 |
| `large_mostly_clean_polyglot` | 193 / 192 | Rust, JavaScript, Python | 1 modified Python file | 1 / 1 |

All latency values below are milliseconds, shown as p50 / p95. “Cold” and
“incremental” are isolated generation operations over identical already
extracted inputs; production cold/incremental request samples are reported
separately below.

| Project | Direct Parquet | Exact request oracle | Cold generation build | Warm generation query | Bounded incremental | Exact fallback | Full warm `code_*` MCP |
|---|---:|---:|---:|---:|---:|---:|---:|
| small | 0.088 / 0.163 | 42.234 / 44.147 | 0.193 / 0.219 | 0.003 / 0.004 | 0.015 / 0.021 | 50.585 / 53.780 | 128.632 / 132.357 |
| medium | 0.101 / 0.169 | 42.334 / 48.000 | 0.366 / 0.400 | 0.004 / 0.005 | 0.019 / 0.023 | 50.723 / 52.546 | 127.820 / 134.930 |
| large | 0.123 / 0.152 | 37.844 / 40.834 | 1.219 / 1.449 | 0.010 / 0.013 | 0.011 / 0.015 | 53.396 / 55.957 | 131.568 / 139.354 |

The raw production cold requests were 179.530, 180.069, and 181.026 ms for
small, medium, and large respectively. The production request after the bounded
change was 180.463, 174.857, and 200.473 ms. Those are single state
transitions, not percentile claims; the repeated isolated cold and incremental
cases above provide the 30-sample p50/p95 distributions.

The phase probes use the same fixture, snapshot, and query as each cell. They
measure the named work independently, so percentile columns are not summed as
if they were a critical-path trace.

| Project | Backend open | Full base read | Exact Git freshness | Cold lookup/build | Warm query | Response-file OID analysis | Construct/serialize | Exact overlay finalization |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| small | 0.075 / 0.088 | 0.483 / 0.614 | 34.010 / 41.699 | 0.193 / 0.219 | 0.003 / 0.004 | 8.173 / 8.591 | 0.008 / 0.010 | 0.071 / 0.132 |
| medium | 0.076 / 0.082 | 0.480 / 0.554 | 33.955 / 43.462 | 0.366 / 0.400 | 0.004 / 0.005 | 8.383 / 9.204 | 0.008 / 0.009 | 0.116 / 0.160 |
| large | 0.076 / 0.083 | 0.679 / 0.776 | 34.401 / 43.934 | 1.219 / 1.449 | 0.010 / 0.013 | 8.449 / 9.054 | 0.008 / 0.010 | 0.127 / 0.184 |

This explains the historical roughly 40 ms direct/request-scoped work versus
roughly 200 ms complete request without attributing the gap to the query. The
generation query itself is 0.003--0.010 ms p50 and response serialization is
about 0.008 ms. One exact Git freshness observation costs about 34 ms p50 and
response-file OID analysis another 8.2--8.4 ms. After subtracting one measured
freshness observation, response-file analysis, backend open, warm query, and
serialization from the full-request p50, the accounting remainder is 86.363,
85.394, and 88.624 ms. The request source identifies that remainder: search
preflight runs `GraphResponseMetadata::analyze_source_inner`, stable overlay
preparation performs an initial snapshot plus an exact validation,
`authoritative_overlay_identity` validates again after the handler, and final
response metadata performs Git/OID analysis again. These repeated
freshness/metadata passes are outside the already-pinned generation query.
Cold production adds one exact extraction/generation transition and lands at
179--181 ms; the bounded production transition reached 200.473 ms. The old
40/200 observation is therefore the expected composition of base/query work,
repeated authoritative validation and metadata work, and (when cold) one
generation transition, rather than repeated result merging or sorting.

Structural evidence:

| Project | Cold generation | Warm identity / builds / base loads / query ops | Warm shadow / merge / sort / stable-ID dedup | Incremental generation; changed paths / closure paths / closure symbols / base loads / query ops |
|---|---|---|---|---|
| small | `gen_91066763235ea3db`; 1 build, 1 base load | same ID for all 30; 0 / 0 / 60 | 0 / 0 / 0 / 0 | `gen_6d9caed162e51e5d`; 1 / 1 / 1 / 0 / 2 |
| medium | `gen_ed559c6382a11ae5`; 1 build, 1 base load | same ID for all 30; 0 / 0 / 60 | 0 / 0 / 0 / 0 | `gen_da270027d508080a`; 1 / 1 / 2 / 0 / 2 |
| large | `gen_d261f43c9cb306f8`; 1 build, 1 base load | same ID for all 30; 0 / 0 / 60 | 0 / 0 / 0 / 0 | `gen_0a9303f0eb1427eb`; 1 / 1 / 1 / 0 / 2 |

Every direct, oracle, cold, warm, incremental, fallback, and full-MCP result
has digest
`36559330dd94c8dfb87202d6e9957d92d9ed455a00e5cd66fb23dbcbcd215476`.
Mismatch count is zero in every cell. The exact oracle performed 30 shadow
filters, 30 merges, 30 overlay sorts, and 30 stable-ID deduplications per cell;
the 30 warm generation requests performed zero of every stage. The exact
fallback route is `request_scoped_exact_overlay`, reason
`configuration_off_exact_oracle`, and also matches the oracle. Cold and warm
are explicitly classified and never pooled. There is no fixed millisecond
release threshold; the gate is parity plus structural work elimination.

The emitted JSON passed the same production release contract used by the
deterministic tests:

```bash
SPUR_REMOTE=0 \
SPUR_GRAPH_TASK6_EVIDENCE=/Volumes/Projects/Projects/spur/.spur/bench-evidence/task6-overlay-generation-matrix-29fa6d9973eb.json \
scripts/spur-cargo test -p spur-graph --test perf_gates gate_task6_ -- --nocapture
# 4 passed; 0 failed

scripts/spur-cargo test -p spur-graph --test perf_gates -- --nocapture
# 4 passed; 0 failed; 9 ignored

scripts/spur-cargo test -p spur-graph --lib
# remote Linux: 490 passed; 0 failed; 3 ignored
```

The local macOS classification reproduced the two caller-identified old
fixtures,
`overlay_snapshot_supported_paths_reject_lossy_normalized_collision` and
`overlay_snapshot_supported_paths_reject_non_utf8_relative_path`: APFS rejects
their non-UTF-8 path creation with `Illegal byte sequence`. Neither test nor
its production source is in the Task 6 diff, and both pass as part of the
remote Linux result above. Strict clippy is presently blocked before Task 6
code by pre-existing warnings: the normal invocation stops on six `spur-acp`
findings, while `--no-deps` reaches unchanged `spur-graph` and stops on its
existing dead-code/complexity/test findings.

SOLVE evidence:

- PRE `sol_49fb133b237749de` (persisted and reloaded): `unsat` / `fail`.
  The one-project, zero-admissible-cell pre-edit model violated both the exact
  three-project cardinality and the mutually-consistent release-cell rule.
- POST `sol_c479540b80db46da` (persisted and reloaded): `sat` / `pass`.
  Exactly three unique project rows satisfy identical-input parity, exact
  digest equality, stable generation reuse, zero warm finalization, bounded
  incremental closure, and exact fallback. All three hard rules
  (`cardinality`, `unique`, and `mutually_consistent`) pass; there is no
  objective and no unsat core.

Release decision: the measured generation route and fsmonitor `Auto` path are
safe to release when explicitly selected: parity and every structural gate
pass across all three project shapes. The production/configuration default
nevertheless remains `Off`, automatic enablement remains disabled, and
`/configure` semantics are unchanged because Task 6 does not alter production
configuration. A later configuration task may choose to change that default;
this evidence removes correctness and structural-performance objections but
does not silently broaden the present release surface.

### Task 3 SOLVE evidence correction (2026-08-25)

The originally reported PRE `sol_687d78351d7e4d8d` and POST
`sol_7a7f30732b9b4d8f` are invalid audit references: reloading either with
`get_solve_result` returns `-32004` (`solve_id ... was not found`). They are
replaced, without changing Task 3 Rust production or test code, by these
persisted and independently reloaded artifacts:

- PRE `sol_23670cb13d6b4d4c`: `sat`, five named hard constraints, no objective.
  Its counterexample model has visible source/target and an oracle edge, but
  `generation_adjacency_present=0` and
  `same_generation_adjacency_available=0`, representing the pre-Task-3
  absent-adjacency base state and exact-oracle mismatch.
- POST `sol_9ef784d457e2477f`: `sat`, nine named hard constraints covering
  endpoint visibility, caller/callee symmetry, deleted-edge exclusion, exact
  edge/selector/oracle equivalence, nested request-generation coherence,
  digest equality, and unaffected-segment reuse. The reloaded model has every
  positive invariant at `1` and `deleted_edge_leaks=0`; its single lexicographic
  maximize objective has value and finite bound `11`, with
  `optimization.termination=complete`.
- Existing catalog POST `sol_475ba936ad3042e9` remains reloadable: `sat`, one
  hard `data_integrity.mutually_consistent` constraint, no objective, and a
  complete coherent model whose generation/oracle digest fields both equal
  `digest_7d4d470d5a4d145d`.

Catalog-first routing used `solve_rule_spec` before the generic typed fallback;
both replacement requests passed `solve_constraint_check`, used
`persist=true`, and were then verified from their reloaded requests, models,
constraints/objectives, and optimization envelope. GREEN was reconfirmed with:

```bash
scripts/spur-cargo test -p spur-graph --test overlay_client \
  overlay_generation_adjacency_matches_fresh_oracle_and_reuses_unaffected_segments \
  -- --exact --nocapture
# ok: 1 passed, 11 filtered; generation/oracle digest
# 7d4d470d5a4d145d9ac19ecfac2dd49e1267b5c87f5081163359ccdedb37cc0f;
# stable caller and isolated adjacency segments reused

scripts/spur-cargo fmt -p spur-graph -- --check
# exit 0
```
