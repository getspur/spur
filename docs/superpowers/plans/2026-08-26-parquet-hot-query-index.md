# Parquet Hot Query Index Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** User-approved performance investigation of `crates/spur-graph/benches/parquet.rs`
**Formal @spec cells:** none
**Design epic:** none; approval and baseline evidence are in the originating conversation
**Pre-solve:** `sol_753837b911104bd4`
**Post-solve:** `sol_af0d50bd9922450e` (`sat` for every steady-state upper confidence bound under 10 ms)

**Goal:** Put repeated current-graph `code_*` search, lookup, resolve, and adjacency queries below a strict 10 ms steady-state SLO on the current 74,124-symbol / 322,715-edge fixture.

**Architecture:** Keep Parquet as the durable, column-pruned hydration layer and add one immutable, lazily initialized hot index owned by `ParquetClient`. The index stores compact row references and lookup/range maps for current symbols and resolved/unresolved adjacency; it must not hydrate commits, snapshots, temporal edges, source text, or diagnostics. The first build is measured separately from steady-state queries and shared by the existing cached `Arc<ParquetClient>`.

**Tech Stack:** Rust 2021, Arrow/Parquet projections, standard-library immutable maps/ranges, Criterion, Z3 pre/post evaluation.

---

### Task 1: Add the hot current-graph query index

**Task ID:** `hot-current-query-index`

**Files:**
- Create: `crates/spur-graph/src/query_hot_index.rs`
- Modify: `crates/spur-graph/src/lib.rs`
- Modify: `crates/spur-graph/src/query_client.rs`
- Modify: `crates/spur-graph/src/store/parquet.rs`
- Test: `crates/spur-graph/tests/query_client_parity.rs`
- Benchmark: `crates/spur-graph/benches/parquet.rs`

**Depends on:** none

**Acceptance Criteria:**
- [x] Preserve exact result parity, ordering, truncation, ambiguity, resolved/unresolved edge behavior, and error behavior for all existing `GraphQueryClient` operations.
- [x] A `ParquetClient` builds the immutable current-query index at most once and reuses it across repeated calls.
- [x] The hot index excludes temporal history and source payloads; temporal cold-build timing remains a separately reported metric.
- [x] On `SPUR_GRAPH_PERF_FIXTURE=.spur/graph/acd60905a3a40accf38792d2e7ce41e37e58bd4ad0d4e62624b069d57a02b832.parquet`, steady-state medians for prefix search, substring search, max-degree callers, and max-degree callees are each strictly below 10,000,000 ns, or the worker emits a `scope_drift` signal with the exact failing measurement before broadening the design.
- [x] `scripts/spur-cargo test -p spur-graph --test query_client_parity` passes.
- [x] `scripts/spur-cargo test -p spur-graph query_client` passes.
- [x] `scripts/spur-cargo fmt -- --check` passes.

**Suggested Worker:** `claude-code-acp` for the tightly coupled data-layout and query-path change.

**Scope Boundary:**
- IN scope: current-symbol search/lookup/resolve/file ranges, resolved forward/reverse adjacency, unresolved source/label adjacency, one-time Parquet projection loading, parity tests, and the operation-matrix benchmark.
- OUT of scope: MCP response schemas, graph extraction, writer schema version changes, overlay semantics, source-file hydration, semantic/vector search, and full temporal-history redesign.
- If the implementation needs a new dependency, changes a public response type, or exceeds six listed files, emit `scope_drift` before proceeding.

**Implementation:**

- [x] **Step 1: Write RED tests before production code.** Add a parity test that exercises exact/prefix/substring search, stable-ID lookup, file lookup, bare/file-qualified resolve, callers, and callees twice through the same `ParquetClient`. Add a test-only observation API with the wished-for contract:

```rust
#[test]
fn parquet_client_reuses_one_hot_current_query_index() {
    let fixture = parity_fixture();
    let (_tempdir, parquet) = parquet_client(&fixture);

    assert_eq!(parquet.hot_query_index_build_count(), 0);
    let _ = parquet.search_symbols(&prefix_options()).unwrap();
    let _ = parquet.find_caller_edges("target");
    let _ = parquet.search_symbols(&substring_options()).unwrap();
    let _ = parquet.find_callee_edges("source");

    assert_eq!(parquet.hot_query_index_build_count(), 1);
}
```

Run `scripts/spur-cargo test -p spur-graph parquet_client_reuses_one_hot_current_query_index -- --nocapture` and record the expected RED failure caused by the missing hot-index contract.

- [x] **Step 2: Implement the minimal immutable index.** Introduce a crate-private shape equivalent to:

```rust
pub(crate) struct HotQueryIndex {
    symbols: Vec<GraphSymbolArtifact>,
    search_symbols: Vec<SearchSymbol>,
    symbol_by_id: HashMap<String, usize>,
    symbols_by_file: HashMap<String, Vec<usize>>,
    symbols_by_entity: HashMap<String, Vec<usize>>,
    symbols_by_qualified: HashMap<String, Vec<usize>>,
    resolved_by_source: HashMap<String, Vec<usize>>,
    resolved_by_target: HashMap<String, Vec<usize>>,
    unresolved_by_source: HashMap<String, Vec<usize>>,
    unresolved_by_label: HashMap<String, Vec<usize>>,
}
```

The exact representation may use sorted vectors/ranges instead of `HashMap` when measurement shows a lower-cost equivalent. Load only current-query Parquet columns. Store the index behind `OnceLock<Result<Arc<HotQueryIndex>, SharedHotQueryIndexError>>`; errors must retain their full chain as `file_oids` already does. Route current-query operations through row references and clone only returned values. Preserve direct Parquet fallback only when it preserves the same error contract.

- [x] **Step 3: Verify GREEN and parity.** Run the focused RED test, the full parity integration test, and query-client tests using `scripts/spur-cargo`; fix production code rather than weakening result assertions.

- [x] **Step 4: Extend the existing operation matrix benchmark.** In `crates/spur-graph/benches/parquet.rs`, benchmark index build separately, prewarm once, then measure exact/prefix/substring, symbol-by-ID, resolve, symbols-by-file, max-degree callers, and max-degree callees on the same client and fixture. Do not include `ParquetClient::open`, index construction, fixture creation, or result formatting in steady-state cells. Retain the temporal cold and cached cells as separate categories.

- [x] **Step 5: Measure and commit.** Run the operation matrix with `SPUR_REMOTE=0` and the exact fixture path above, record medians in the task result, run formatting/tests, and commit with `feat(spur-graph): <issue-id> add hot parquet query index`.

**Execution evidence (upper confidence bounds):** exact 9.5942 ms; prefix `s` / 5,956 matches 0.86868 ms; substring `e` / 60,347 matches 6.5809 ms; symbol ID 3.7843 ms; resolve 4.4740 ms; file / 818 symbols 3.9772 ms; callers / 9,774 records 4.8077 ms; callees / 287 records 0.047757 ms. Cold symbol, adjacency, and temporal builds remain separately reported at 65.200 ms, 227.34 ms, and 2.437 s; their combined 10 ms model is `unsat` (`sol_764e92058d094698`).

**Scope Drift Checkpoint:**
- If a result cardinality alone makes a 10 ms internal-query SLO impossible, report the cardinality and separate fixed lookup cost from per-result cost.
- If estimated remaining work grows by more than 50%, emit `scope_drift`.
- If any listed parity behavior changes, emit `risk` and stop broadening the optimization.
