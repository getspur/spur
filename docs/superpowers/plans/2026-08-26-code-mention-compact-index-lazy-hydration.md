# Compact Code-Mention Index and Lazy Parquet Hydration Implementation Plan

> **For SPUR:** approved implementation tracked by `bd-2tuc`; execute RED → GREEN → measure → re-solve → review.

**Source:** the Z3 Optimize evaluation recorded on parent issue `bd-3324` and the supporting notebook in `crates/spur-graph/docs/`. The complete Pareto/MaxSMT run selected a compact local fuzzy-search index plus batched Parquet hydration over direct per-keystroke Parquet scans and the existing eager cache.

**Baseline:** the current mention cache materializes 74,124 symbols and 3,434 files as rich `MentionEntry` plus `CodeMentionPayload` values and duplicates important keys across the source and registry caches. The allocation model estimates about 177.33 MiB steady-state for the observed graph. The existing 77,000-row debug benchmark reports 24 µs median dispatch and 876,106 µs median background completion across seven samples.

**Goal:** retain the existing fuzzy symbol/path ranking semantics while keeping only compact searchable symbol metadata resident. Build complete payloads from Parquet only for code symbols that survive the query limit, batch those stable-ID lookups, and install results only after the generation guard accepts the query result.

**Hard invariants:**

- The compact index contains every live graph symbol needed by the existing fuzzy matcher.
- The source cache eagerly owns file entries/file payloads only; it does not own rich symbol entries or symbol payloads.
- A query hydrates at most its returned symbol count, therefore no more than its existing `limit`.
- Selected/accepted symbols remain expandable at submit time.
- Stale background generations never install hydrated payloads.
- Parquet hydration uses one stable-ID batch for the selected symbols, not one scan per symbol.
- JSON artifacts remain a compatible fallback; the Parquet directory is the optimized production path.

**Implemented outcome (2026-08-26):** the active 74,124-symbol artifact models to a 10.56 MiB retained compact index, about 94.0% below the prior 177.33 MiB eager symbol model. After independent review restored exact canonical URI tie ordering, the final 77,000-candidate compact query-core benchmark completed in 117.194 ms median versus the 876.106 ms rich-row directional baseline (about 7.5× faster / 86.6% lower latency). Z3 re-selected the hybrid architecture in `sol_e1c6119757b64613`; the counterexample `hydrated_symbols > query_limit` was UNSAT in `sol_56c5e961eded48ab`. The implementation and measurement guide is recorded in sections 12–15 of `crates/spur-graph/docs/parquet-exact-search-row-group-pruning.ipynb`.

---

### Task 1: expose bounded Parquet projections for mention indexing and hydration

**Task ID:** `bd-2tuc.1`

**Files:**

- Modify: `crates/spur-graph/src/query_client.rs`

**Steps:**

1. Add failing tests proving a Parquet client can return projected graph files, the complete search-symbol index, and a batched stable-symbol-ID lookup.
2. Add narrow public methods implemented with existing projected readers, row filters, and row-group pruning.
3. Verify the query-client tests with `scripts/spur-cargo test -p spur-graph query_client`.

**Acceptance criteria:** projection results preserve stable IDs and display/ranking fields; batched hydration returns only requested live symbols; no full `GraphIndexArtifact` load is introduced.

### Task 2: replace eager rich symbol cache with a compact source index

**Task ID:** `bd-2tuc.2`

**Depends on:** `bd-2tuc.1`

**Files:**

- Modify: `crates/spur-tui/src/mentions/entry.rs`
- Modify: `crates/spur-tui/src/mentions/code_graph/source.rs`

**Steps:**

1. Add failing source tests proving `build()` returns file rows only, exposes all symbols through an immutable compact index, and hydrates only explicitly selected stable IDs.
2. Store stable ID, entity, path, kind, scope, and line range in the compact candidate; intern repeated path/kind/scope strings.
3. Open `ParquetClient` for directory artifacts, build candidates from its projected search index, and hydrate selected symbols through one batched lookup. Keep the current JSON reader as the compatibility fallback.
4. Preserve content-hash cache invalidation and reuse compact-index `Arc`s on cache hits.

**Acceptance criteria:** no eager symbol `MentionEntry`/payload vector remains on the Parquet path; file behavior and graph-version metadata remain unchanged.

### Task 3: score compact candidates and install generation-safe result payloads

**Task ID:** `bd-2tuc.3`

**Depends on:** `bd-2tuc.2`

**Files:**

- Modify: `crates/spur-tui/src/mentions/registry.rs`
- Modify: `crates/spur-tui/src/components/query_source.rs`
- Modify: `crates/spur-tui/tests/mention_registry.rs`

**Steps:**

1. Add failing tests for ranking parity, `hydrated_symbol_payloads <= query_limit`, accepted-symbol retention, and stale-generation payload rejection.
2. Extend query work with shared compact indexes. Score candidates with the current entity/path algorithm and compare them in the same typed-query ordering as materialized entries.
3. Materialize entries for final rows only, batch-hydrate the returned symbol IDs, and carry those payloads in `MentionQueryResult`.
4. Install active payloads when a synchronous result returns or a current background generation is applied. Pin a payload on mention acceptance so later queries cannot break submit expansion.
5. Keep file payloads in the source cache and prune transient/accepted symbol payloads at the existing submit-retention boundary.

**Acceptance criteria:** current ranking and end-to-end expansion tests pass; stale results cannot mutate payload state; rich symbol payloads are proportional to visible/accepted mentions rather than total graph size.

### Task 4: post-measure, prove, verify, review, and commit

**Task ID:** `bd-2tuc.4`

**Depends on:** `bd-2tuc.3`

**Steps:**

1. Re-run the 77,000-candidate benchmark and a graph-specific compact-index benchmark against the active Parquet artifact.
2. Re-run Z3 to verify the payload bound and re-evaluate the chosen architecture with measured evidence.
3. Run focused tests, `scripts/spur-cargo test -p spur-tui --test mention_registry`, relevant `spur-graph` tests, formatting, and clippy with warnings denied.
4. Request independent code review, address findings, update `bd-2tuc` and the notebook/documentation with measured results, then commit only scoped files.

**Stop conditions:** ranking regressions, per-symbol Parquet scans, payload installation from stale generations, non-code unrelated diffs, or a post benchmark materially worse than baseline without an explained memory/latency tradeoff.
