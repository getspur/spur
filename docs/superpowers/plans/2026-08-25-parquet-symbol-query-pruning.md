# Parquet Symbol Query Pruning Implementation Plan

> **For SPUR orchestrator:** This plan is designed for beads-backed execution.
> The focused task is tracked by issue `bd-22cs`; no worker dispatch is needed because the primary agent owns the single overlapping file scope.

**Source spec:** Approved evaluation of <https://datafusion.apache.org/blog/2025/07/14/user-defined-parquet-indexes/> against `crates/spur-graph/src/store/parquet.rs`
**Formal @spec cells:** none
**Design epic:** none; the user approved the evaluated first tranche directly

**Goal:** Reuse immutable Parquet metadata and prune irrelevant row groups/pages for exact symbol queries while preserving residual row filtering.

**Architecture:** `store/parquet.rs` will own the reusable metadata cache and exact-string pruning planner because it already owns Parquet encoding details. `query_client.rs` will supply exact equality/IN predicates to that shared reader seam; the existing `RowFilter` remains the final correctness filter. This tranche retains `edges_by_dst.parquet` and adds neither DataFusion nor a secondary postings index.

**Tech Stack:** Rust 2021, arrow-rs/parquet 58.1, Z3 configuration rules, `scripts/spur-cargo`

---

### Task 1: Cache metadata and prune exact-string reads

**Task ID:** `bd-22cs`

**Files:**
- Modify: `crates/spur-graph/src/store/parquet.rs`
- Modify: `crates/spur-graph/src/query_client.rs`
- Test: inline unit tests in the same two modules

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ParquetClient::open` loads optional page indexes for `nodes.parquet`.
- [ ] Repeated access to the same immutable Parquet path reuses loaded metadata.
- [ ] Exact string equality/IN predicates exclude row groups by statistics and Bloom filters, then exclude pages by page min/max when available.
- [ ] Existing `RowFilter` predicates remain active and query results remain unchanged.
- [ ] `edges_by_dst.parquet` remains enabled and no DataFusion/FST dependency is added.
- [ ] Focused tests, the `spur-graph` suite, formatting, and lint verification pass.
- [ ] The same Criterion fixture is measured after the change and compared with the recorded baseline.

**Suggested Worker:** primary Codex agent

**Scope Boundary:**
- IN scope: shared Parquet reader metadata, exact string equality/IN pruning, query-client wiring, focused tests.
- OUT of scope: FST/postings, substring indexes, temporal shard manifests, cache eviction policy, removal of `edges_by_dst.parquet`, new DataFusion dependencies.
- If the change requires an out-of-scope file, record scope drift in `bd-22cs` before editing it.

**Implementation:**
- [ ] **Step 1: Run SOLVE PRE.** Verify the catalog-owned configuration snapshot selects exactly built-in pruning plus immutable metadata caching, retains residual filtering and destination-sorted edges, and excludes DataFusion/FST from this tranche. Record the `solve_id` in `bd-22cs`.

- [ ] **Step 2: Write failing behavior tests.**

```rust
#[test]
fn parquet_client_loads_page_indexes_for_pruning() {
    // Open a written artifact and assert column + offset indexes are loaded.
}

#[test]
fn parquet_client_reuses_non_node_metadata_after_first_query() {
    // Query a file once, invalidate only its footer, and prove the cached metadata
    // allows the same immutable-file query to succeed again.
}

#[test]
fn exact_symbol_lookup_skips_an_irrelevant_corrupt_row_group() {
    // Corrupt a non-matching row group's data pages. Exact lookup must still
    // return the target because stats/Bloom pruning excludes that row group.
}
```

- [ ] **Step 3: Run the focused tests through `scripts/spur-cargo` and confirm assertion/decoding failures caused by the absent behavior.**

- [ ] **Step 4: Implement the minimal shared seam.**

```rust
pub(crate) struct ParquetMetadataCache { /* immutable path -> ArrowReaderMetadata */ }

pub(crate) enum StringPruningPredicate {
    Eq { column: String, value: String },
    In { column: String, values: Vec<String> },
}

// Load PageIndexPolicy::Optional metadata once, select candidate row groups,
// derive page RowSelection when possible, and still install the RowFilter.
```

- [ ] **Step 5: Run focused tests to GREEN, then run the full crate tests.**

- [ ] **Step 6: Run SOLVE POST with the implemented selections and dependencies. Treat any result other than `sat` + `pass` as a new RED failure.**

- [ ] **Step 7: Run formatting, lint/build verification, and the same Criterion fixture; record commands and results in `bd-22cs`.**

- [ ] **Step 8: Commit only the plan and the two scoped source files with an intent-focused message.**

## Dependency DAG

```text
bd-22cs
```

The single-node graph is acyclic. Splitting tests and implementation across workers would overlap both source modules and break strict RED-before-GREEN ordering, so no dependency can be removed safely.
