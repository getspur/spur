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

Pending Task 1 execution under the committed plan protocol.

### POST

Pending Task 6 execution under the committed plan protocol.
