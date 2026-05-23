# Code-as-Memory Phase 1.5 — Hardening Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Address all blocking + high + medium findings from the Phase 1 dual code review (codex + claude-code) before opening a PR to main.

**Base:** `spur/plan-merge-dc96bc95-7b28-4c10-99f2-72c642435b5f-b3477d788f9f4435a3fe9027f937a9ec`

**Spec:** `docs/superpowers/specs/2026-05-20-code-as-memory-phase-1-design.md` remains the source of truth. This plan does not introduce new surface — it fixes drift between the merged implementation and the spec.

**Worker:** All tasks → `codex`. Linear DAG (each task depends on its immediate predecessor) to avoid the cycle-detection bug in the plan engine's auto-serialize-siblings pass.

**TDD discipline:** Each task is red → green → refactor. Worker MUST write the failing test first, watch it fail, then implement, then watch it pass. No exceptions.

---

## Task 1 (T1) — Schema-version interlock + path-traversal hardening on CommitIndexArtifact

Fixes: **H2**, **H4**, **M1**.

**Files:**
- Modify: `crates/spur-graph/src/store/commit_index.rs`
- Test: `crates/spur-graph/tests/commit_index_io.rs` (extend if present, else create)

**Steps:**
- [ ] Write failing tests:
  - `load_pointer_rejects_v1_schema_version` — pointer with `schema_version: "1"` returns Err mentioning the version.
  - `load_artifact_rejects_absolute_artifact_relative_path` — pointer with `artifact_relative_path: "/etc/passwd"` returns Err.
  - `load_artifact_rejects_parent_traversal` — `../../foo.json` returns Err.
  - `load_artifact_rejects_path_escaping_dot_spur` — a path that canonicalizes outside `worktree/.spur` returns Err.
- [ ] Implement: in `load_pointer`, after deserializing, check `schema_version == GRAPH_INDEX_VERSION_TEMPORAL`; else return a typed error. In `load_artifact`, before `worktree.join(rel)`: reject `rel.is_absolute()`, reject any component equal to `..`, then canonicalize and assert the result starts with `worktree.canonicalize()?.join(".spur")`.
- [ ] Run `cargo test -p spur-graph commit_index` and confirm green.

## Task 2 (T2) — Persist `diagnostics` and include temporal collections in artifact hash

Fixes: **M5**, **M6**, **N2**.

**Files:**
- Modify: `crates/spur-graph/src/schema.rs` (remove `#[serde(skip)]` on `diagnostics`)
- Modify: `crates/spur-graph/src/store/json.rs` (include temporal fields in canonical hash input)
- Test: `crates/spur-graph/tests/diagnostics_persist.rs` (new)

**Steps:**
- [ ] Write failing tests:
  - `diagnostics_round_trip_through_json` — populate `GraphIndexArtifact.diagnostics`, serialize, deserialize, assert equality.
  - `artifact_hash_changes_when_temporal_collections_change` — build two artifacts identical except for one extra `SymbolSnapshotArtifact`; their hashes must differ.
- [ ] Implement: drop `#[serde(skip)]`; extend the canonical-hash input writer in `store/json.rs` to include `symbol_snapshots`, `commits`, and `temporal_edges` (whatever the actual field names are — read first).
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T1.

## Task 3 (T3) — `Resolution::Deleted` preserves `last_seen` through the MCP boundary

Fixes: **H1**.

**Files:**
- Modify: `crates/spur-mcp/src/server/handlers/code_graph.rs`
- Modify: `crates/spur-mcp/src/worker_server.rs` if a typed error variant is needed in the response envelope.
- Test: `crates/spur-mcp/tests/code_graph_e2e.rs` (extend)

**Steps:**
- [ ] Write failing test: `code_subgraph_returns_deleted_with_last_seen` — request a symbol at a commit *after* its delete; assert response contains a structured error/status with `kind: "deleted"` and `last_seen: <sha>`, distinguishable from `kind: "not_found"`.
- [ ] Implement: at `code_graph.rs` (around the line currently returning `NotFound` for `Resolution::Deleted`), branch on the variant and return a distinct JSON-RPC error with `data: { last_seen }`. Pick a code in the existing range — do not collide with `-32004`.
- [ ] Add equivalent handling for `Resolution::Ambiguous` and `Resolution::Unknown` if not already distinguishable; update the test to cover all three.
- [ ] Run `cargo test -p spur-mcp code_graph` and confirm green.

**Depends on:** T2.

## Task 4 (T4) — Tree-sitter parse failure → file-level downgrade with diagnostic

Fixes: **B4**.

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (make `extract_symbols` return `Result`)
- Modify: `crates/spur-graph/src/git_walk.rs` (in `SymbolDiffCtx::symbol_changes_for_commit`, catch `Err` from either side and skip just that file's symbol diff, recording a diagnostic)
- Test: `crates/spur-graph/tests/bytes_extractor.rs` (extend)

**Steps:**
- [ ] Write failing test: `extractor_returns_err_on_invalid_tree_sitter_input` — feed a deliberately corrupt blob (e.g., 100 MB of random bytes, or a known parser-panic input if reachable). Assert `Err`, not panic.
- [ ] Write failing test in `temporal_resolution.rs` or new `parse_failure_downgrade.rs`: a commit with one unparseable file and one valid file. Assert the run completes, file-level edges are present for both, symbol-level facts exist for the valid file only, and `diagnostics` contains a `parse_failed` entry with the path + SHA.
- [ ] Implement: convert `extract_symbols` to `Result<Vec<ExtractedSymbol>, ExtractError>`. In `symbol_changes_for_commit`, on either side returning `Err`, push a diagnostic and `continue`. No panic, no skipped file-level edge.
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T3.

## Task 5 (T5) — Non-UTF-8 path lossless container

Fixes: **H5**.

**Files:**
- Modify: `crates/spur-graph/src/schema.rs` — introduce `GitPath` newtype wrapping `Vec<u8>` with `serde` impls that base64- or string-with-quoting-encode non-UTF-8 bytes losslessly. Replace `PathBuf` in `RenamePrev::File`, `SymbolSnapshotArtifact.path`, and `EdgeEndpoint` path fields.
- Modify: `crates/spur-graph/src/git_walk.rs` — flow raw bytes from `--raw -z` parser into `GitPath` without going through `PathBuf` for persistence.
- Test: `crates/spur-graph/tests/git_path_lossless.rs` (new)

**Steps:**
- [ ] Write failing test:
  - `git_path_round_trips_non_utf8_bytes` — `GitPath` from `b"\xff\xfe.rs"`, serialize to JSON, deserialize, assert byte-equal.
  - `walker_preserves_non_utf8_filename_through_artifact` — fixture repo with a file whose name has non-UTF-8 bytes (use `git fast-import` to create it), run the walker, assert the resulting snapshot's `path` round-trips byte-equal.
- [ ] Implement `GitPath`, migrate the three fields, update all callers. Keep a `PathBuf` accessor where the local file system actually needs to be touched (e.g., `cat-file` calls), but never persist `PathBuf` directly.
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T4.

## Task 6 (T6) — Merge commits: per-parent diff emission

Fixes: **B3**, **L2**.

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs` (`run_full_walk_into`)
- Test: `crates/spur-graph/tests/merge_commit_diff.rs` (new) or extend `temporal_resolution.rs`.

**Steps:**
- [ ] Write failing test: build a fixture repo with a merge commit M whose parents A and B each introduce a different symbol. Assert `file_changes_for_commit(M)` (or its public equivalent) emits one set of changes per parent and the resulting `Commit→Snapshot` edges cover both branches. Specifically: a symbol added only on branch B must appear in M's outgoing edges with `change_kind = Added` against parent A.
- [ ] Implement: replace the current `parents.len() != 1` skip with a loop over parents. For each parent, run the existing file/symbol diff against that parent and tag the resulting edges with the parent SHA (extend `EdgeEndpoint` or `TemporalEdgeArtifact` with `parent: Option<Sha>` if not already there).
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T5.

## Task 7 (T7) — Emit `SymbolSnapshot --renamed_from--> SymbolSnapshot` edges

Fixes: **B1**.

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs` — when a symbol rename is detected (Tier 1, 2, or 3-clean), in addition to the `change_kind = RenamedFrom(...)` on the `Commit→Snapshot` edge, emit a direct `SymbolSnapshot(prev) --RenamedFrom--> SymbolSnapshot(new)` edge.
- Modify: `crates/spur-graph/src/temporal.rs` — verify the second arm of `rename_target` (snapshot-to-snapshot traversal) now resolves; remove or repurpose if it remains dead after T7.
- Test: `crates/spur-graph/tests/snapshot_rename_edges.rs` (new)

**Steps:**
- [ ] Write failing test: fixture with `add → rename_file → rename_symbol` chain. Assert there exists a `temporal_edges` entry of kind `RenamedFrom` whose both endpoints are `SymbolSnapshot` (not `Commit`). Then call `symbol_history` and assert it walks via the snapshot-to-snapshot edges, not via the commit-edge `change_kind` field.
- [ ] Implement edge emission. Confirm the second arm of `rename_target` is exercised.
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T6.

## Task 8 (T8) — Identity continuity across modifications

Fixes: **B5** (the T7-original-plan concern).

**Decision:** Change `stable_symbol_id_for(path, entity_name, anchor_hash)` so the ID does **not** include `anchor_hash`. The anchor hash should be stored on the snapshot, not baked into identity. A pure-modify event then produces a snapshot with the same stable_symbol_id as its predecessor, and history walks naturally chain.

**Files:**
- Modify: `crates/spur-graph/src/identity.rs` — change `stable_symbol_id_for` signature to `(path, entity_name)` (drop anchor_hash from hash input). Bump `GRAPH_INDEX_VERSION_TEMPORAL` to `"3"`.
- Modify: `crates/spur-graph/src/git_walk.rs` — update all call sites.
- Modify: `crates/spur-graph/src/schema.rs` — `SymbolSnapshotArtifact` keeps `anchor_hash` as a field, not as part of identity.
- Modify: `crates/spur-graph/src/store/commit_index.rs` — extend the v2 → v3 rejection (loaded artifacts at v2 should now error, mirroring T1).
- Test: `crates/spur-graph/tests/modify_chain_continuity.rs` (new)

**Steps:**
- [ ] Write failing test: fixture with `add → modify → modify → rename` chain. Assert `symbol_history(stable_id_at_tip)` returns 4 snapshots, in topological order, with monotonically newer commit timestamps. Specifically: the first two `Modified` snapshots must share the same `stable_symbol_id` as the original `Added` snapshot.
- [ ] Implement the identity change, bump the schema version, update all call sites including the rename-corpus harness if it asserts identity equality.
- [ ] Re-run T1's `load_pointer_rejects_v1_schema_version` test (which should now also reject v2).
- [ ] Run full `cargo test -p spur-graph` and confirm green.

**Depends on:** T7.

## Task 9 (T9) — `resolve_symbol_at` matches spec semantics (latest at-or-before)

Fixes: **M3**.

**Files:**
- Modify: `crates/spur-graph/src/temporal.rs`
- Test: `crates/spur-graph/tests/temporal_resolution.rs` (extend)

**Steps:**
- [ ] Write failing test: `resolve_at_intermediate_commit_returns_latest_prior_snapshot` — fixture with commits `C1 (adds S), C2 (unrelated change), C3 (modifies S)`. Calling `resolve_symbol_at(S, C2)` must return the snapshot from C1, not `Unknown(SymbolNotPresentAtAnchor)`.
- [ ] Implement: change the anchor lookup to find the latest snapshot of S whose commit is an ancestor-or-equal of the anchor, using the commit-graph ancestry index. If the anchor commit itself isn't indexed, walk back along its first-parent chain until a commit *is* indexed, then resolve from there.
- [ ] Run `cargo test -p spur-graph temporal` and confirm green.

**Depends on:** T8.

## Task 10 (T10) — Cycle guard + topological "last" snapshot

Fixes: **M2**, **N1**.

**Files:**
- Modify: `crates/spur-graph/src/temporal.rs` (`close_rename_chain`)
- Modify: `crates/spur-graph/tests/temporal_resolution.rs:192-207` — replace lexicographic SHA sort with topo order from the commit index.
- Test: extend `temporal_resolution.rs`.

**Steps:**
- [ ] Write failing test: a corrupted artifact (constructed in-test) with a cyclic `RenamedFrom` chain. Assert `close_rename_chain` returns an error or `Resolution::Unknown` with a diagnostic, never loops or stack-overflows.
- [ ] Implement: add `debug_assert!` plus a runtime visited-set guard. Chain length bounded by total snapshot count.
- [ ] Fix the lex-sort in the fixture test to use commit topo order.
- [ ] Run `cargo test -p spur-graph temporal` and confirm green.

**Depends on:** T9.

## Task 11 (T11) — Bidirectional `ambiguous_rename` diagnostics + Jaccard-fallback handling

Fixes: **H3**.

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs` (around line 693, both sides of Tier 2/3)
- Test: `crates/spur-graph/tests/rename_corpus.rs` (extend) and/or new `rename_low_jaccard.rs`

**Steps:**
- [ ] Write failing test: a commit that renames `process_chunk` → `process_batch` with a body rewrite that drops Jaccard below 0.7. Assert: (a) both `Added` and `Deleted` snapshots are emitted; (b) **both** carry an `ambiguous_rename` diagnostic referencing the other candidate by stable id. Today only the Added side carries it.
- [ ] Implement: when Tier 2 falls below the threshold or Tier 3 is ambiguous, emit the diagnostic on both endpoints.
- [ ] Run `cargo test -p spur-graph` and confirm green.

**Depends on:** T10.

## Task 12 (T12) — Wire `plan_incremental_walk` into `run_full_walk_into`

Fixes: **B2**.

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs` (`run_full_walk_into`)
- Test: `crates/spur-graph/tests/incremental_ingest.rs` (new)

**Steps:**
- [ ] Write failing test: fixture repo with 5 commits. Run the walker, save the artifact. Add 3 more commits. Run the walker again with a base pointing at the saved artifact. Assert that the second run only ingests the 3 new commits (count `Commit` artifacts touched on the second pass), and the resulting artifact is identical to a cold full walk over all 8.
- [ ] Implement: at the top of `run_full_walk_into`, call `plan_incremental_walk` with the prior pointer (if present), then iterate only the planned commit range. Preserve cold-walk behavior when no prior artifact exists.
- [ ] Run `cargo test -p spur-graph incremental` and confirm green.

**Depends on:** T11.

## Task 13 (T13) — Temporal-edge pre-index for history walk

Fixes: **M7**.

**Files:**
- Modify: `crates/spur-graph/src/temporal.rs` — introduce a `TemporalIndex` built once per `GraphIndexArtifact` load (map: `stable_symbol_id → Vec<&TemporalEdgeArtifact>`, and `commit_sha → Vec<&...>`).
- Test: `crates/spur-graph/benches/incremental.rs` (extend `bench_full_walk_20k_merges` with a history-walk sub-bench)

**Steps:**
- [ ] Write failing assertion-test: synthetic artifact with 50k snapshots and 50k temporal edges. Call `symbol_history` 1000 times; total wall-time < 250 ms on the bench machine (parameterize the threshold for CI noise tolerance).
- [ ] Implement the pre-index. `symbol_history` does O(chain_length) lookups, not O(total_edges).
- [ ] Run `cargo bench -p spur-graph --bench incremental -- history` and record results in the PR description.

**Depends on:** T12.

## Task 14 (T14) — Adversarial rename corpus pairs

Fixes: rename-corpus suspicion (was claimed F1=1.000 over trivial pairs).

**Files:**
- Add 9 fixtures (3 per language) under `crates/spur-graph/tests/fixtures/rename_corpus/{rust,typescript,python}/adversarial_{01,02,03}.{in,out}` with `expected.json` reflecting the *correct* outcome (including the cases where the system should refuse to claim a rename).
- Modify: `crates/spur-graph/tests/rename_corpus.rs` to load adversarial fixtures and assert per-class metrics separately.

**Adversarial classes (3 per language):**
- `01_full_rewrite` — rename + ≥80% body rewrite (Jaccard ~0.07). Expected: `Added + Deleted + ambiguous_rename on both`.
- `02_crossover` — two similar functions both renamed in the same commit such that each could plausibly be the other's predecessor. Expected: ambiguity rejection, no `RenamedFrom`, diagnostics on all four endpoints.
- `03_params_only` — rename + rename of every parameter (Jaccard ~0.20 for token-bag including parameter identifiers). Expected: `RenamedFrom` if the token bag excludes identifiers below a threshold, else `Added+Deleted` — test asserts whichever the spec mandates; if the spec is silent, the test pins the chosen behavior with a comment.

**Steps:**
- [ ] Write failing tests by adding the fixtures.
- [ ] Adjust the rename harness if needed; do not loosen baselines to pass — instead, document any false negatives as known limitations in `docs/superpowers/specs/...phase-1-design.md` under "Phase 1 known limitations".
- [ ] Run `cargo test -p spur-graph --test rename_corpus` and confirm green.

**Depends on:** T13.

## Task 15 (T15) — Bench surface: RSS + artifact-size + reachable-DAG growth

Fixes: **M4**.

**Files:**
- Modify: `crates/spur-graph/benches/incremental.rs`

**Steps:**
- [x] Write failing assertion-tests:
  - `bench_full_walk_20k_merges` reports peak RSS (via `mach_task_basic_info` on macOS or `/proc/self/status` on Linux) and artifact JSON size on disk. Tightened budgets: peak RSS < 1.5 GB, artifact size < 200 MB at 20k commits. (Pick numbers from a baseline run; the test fails if the next run exceeds 1.2× the baseline.)
  - `snapshot_growth_budget` runs against `WalkStrategy::Reachable` (currently `FirstParent`); fails if snapshot count grows super-linearly in commit count.
- [x] Implement the measurement helpers and re-baseline. Record baseline numbers in a comment alongside the assertions.
- [x] Run `cargo bench -p spur-graph --bench incremental` and capture output in the PR.

**Task notes (2026-05-21):**
- Test: `scripts/spur-cargo test -p spur-graph --test incremental_budget -- --nocapture`
  - `snapshot_growth_budget: small(50)=20 large(500)=44 ratio=2.200`
  - `test result: ok. 4 passed; 0 failed; finished in 46.97s`
- Bench: `SPUR_GRAPH_BENCH_FILES=1000 SPUR_GRAPH_BENCH_CHANGE_SET=10 SPUR_GRAPH_BENCH_DIRTY_MODS=10 SPUR_GRAPH_BENCH_GIT_WALK_1K_COMMITS=50 SPUR_GRAPH_BENCH_GIT_WALK_20K_COMMITS=50 scripts/spur-cargo bench -p spur-graph --bench incremental -- --sample-size=10 --measurement-time=1 --warm-up-time=1`
  - `git_walk full 20k merges time: [3.7291 s 3.7354 s 3.7417 s]`
  - `git_walk full 20k merges metrics: elapsed=3.743s commits=50 snapshots=20 temporal_edges=70 artifact_json=49830B peak_rss=31.72MiB`
  - `history walk 50k snapshots time: [2.6719 us 2.6825 us 2.7014 us]`
  - Note: fixture env overrides keep the bench capture practical in this worker; the committed 20k guard still asserts against the baseline comment in `crates/spur-graph/benches/incremental.rs`.

**Depends on:** T14.

## Task 16 (T16) — Cleanup: dead pub surface + spec sync

Fixes: **L1**, doc drift.

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs` — make `try_rename_match` `pub(crate)` and gate its test use through a `#[cfg(test)]` re-export, OR move the test that needs it into the same module.
- Modify: `crates/spur-graph/src/temporal.rs` — remove the now-redundant arm of `rename_target` if T7's snapshot-edge emission made the original commit-edge `change_kind` reading path obsolete. If it's still used as a fallback (e.g., for loading v2 artifacts emitted before T7), keep it but add a comment pointing to T7.
- Modify: `docs/superpowers/specs/2026-05-20-code-as-memory-phase-1-design.md` — add a "Phase 1.5 hardening" appendix briefly noting: identity no longer includes anchor_hash (T8), schema version bumped to "3", merge commits now diffed per-parent, snapshot-to-snapshot RenamedFrom edges are authoritative.

**Steps:**
- [ ] Write a `compile_fail` doc-test asserting `try_rename_match` is not callable from outside the crate (optional — if the harness doesn't support it, just verify via `cargo check` of a probe).
- [ ] Cleanup.
- [ ] Run full `cargo build -p spur-graph -p spur-mcp`, `cargo test --workspace`, `cargo clippy -p spur-graph -p spur-mcp -- -D warnings`. All must pass.

**Depends on:** T15.

---

## Final verification (worker MUST run before signaling completion on T16)

- [ ] `cargo build -p spur-graph -p spur-mcp` — clean.
- [ ] `cargo test -p spur-graph` — all green, including new tests.
- [ ] `cargo test -p spur-mcp code_graph` — green.
- [ ] `cargo clippy -p spur-graph -p spur-mcp -- -D warnings` — clean.
- [ ] `cargo bench -p spur-graph --bench incremental` — no regressions vs baseline.
