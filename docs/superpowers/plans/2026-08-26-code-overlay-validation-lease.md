# Consolidated Overlay Validation Lease Implementation Plan

**Design source:** `docs/superpowers/specs/2026-08-26-code-overlay-validation-lease-design.ipynb`

**Beads epic:** `bd-3fh1`

**Execution:** Inline Codex implementation. Every task uses strict RED -> GREEN TDD, a persisted and reloadable SOLVE PRE result before production edits, and a persisted SOLVE POST result after verification. Use `scripts/spur-cargo`; never bare `cargo`.

## Goal

Keep a fresh, OID-matched overlay generation authoritative even when the base graph index is stale, while removing repeated filesystem validation from warm `code_*` requests.

The Phase 0 probe on Git 2.55.0 found that `git fsmonitor--daemon` exposes only `start`, `run`, `stop`, and `status`. It does not expose a supported synchronous token query, and the current worktree is not watched. Therefore the production release path must not use undocumented daemon IPC. It implements the approved design's safe exact-observer fallback: one exact observation before generation construction and one exact post-query fence. A future token path remains capability-gated.

## Correctness contract

- The static graph may be stale; a fresh overlay generation is the request's source of truth when its current worktree/base identity matches at the post-query fence.
- One request pins one immutable `Arc<OverlayGeneration>`.
- If the post-query identity differs, discard the result and use exact fallback.
- A fresh generation may report `response_file_oids_match=true` without restatting response files only after the post-query fence proves the generation identity unchanged.
- Off and rebuild-only behavior remain unchanged.
- Exact fallback is permanent and is not removed by the optimization.

## PRE proof

Persisted workflow result `sol_9f08cac10b114603` models the current warm request as:

`Start -> ExactObserved -> GenerationPinned -> QueryComplete -> DuplicateValidation -> ResponseReturned`

The trace obeys declared transitions but fails `workflow.safety_invariant`; `DuplicateValidation` is the counterexample state. POST must replace it with `MetadataDerived` and verify the complete bounded trace.

## Dependency DAG

```text
bd-2goo capability gate
  -> bd-rjhv consolidated overlay route
    -> bd-rvzl generation-derived metadata
      -> bd-2pa0 release matrix
```

## Task 1 — Gate unsupported fsmonitor token fencing (`bd-2goo`)

**Files:**

- Modify: `crates/spur-graph/src/git.rs`
- Modify: this plan

**RED:** Add a route test proving fsmonitor status acceleration is not equivalent to a synchronous request token fence. The test must fail because `FsmonitorCapabilities` currently has no token-fence capability.

**GREEN:** Add the explicit capability and require it only for validation-lease eligibility. Keep the existing optimized `git status` route independent so Auto can still accelerate exact observations. The real probe sets token fencing false until Git exposes a supported seam.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph git::tests -- --nocapture
scripts/spur-cargo fmt -- --check
```

**Evidence:**

- Phase 0: Git `2.55.0`; public daemon commands are `start`, `run`, `stop`, and `status`; the worktree daemon reported not watching.
- RED commit: `e6762bdab`; focused compile failed with expected `E0432` missing validation-route API.
- SOLVE POST: `sol_52748ae2f8234b66`; bounded trace ends in `ExactObservation` and excludes `TokenFence`.
- GREEN verification: all `git::tests` passed; formatting applied successfully.

## Task 2 — Consolidate the overlay request route (`bd-rjhv`)

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`
- Modify only if the observation API requires it: `crates/spur-graph/src/mcp/overlay_snapshot.rs`

**RED:** Instrument exact observation phases and prove the current Auto generation route performs more than two request-level observations. Add behavior tests for clean reuse, modified file, new file, rename, deletion, and a mutation between query and final fence.

**GREEN:** Route Auto directly through overlay preparation before legacy metadata preflight. Treat the snapshot builder's own certification as the start observation; remove the duplicate `prepare_stable_overlay_for_worktree` observation; retain one post-query identity fence. On mismatch discard and exact-fallback. If direct overlay preparation fails or exceeds budget, retain the legacy escalation path without serving an uncertified result.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph mcp::tests::overlay_generation -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**Evidence:**

- RED commit: `627c5191f`; generation diagnostic was `Null` instead of the required two-observation contract.
- GREEN route: Auto enters the exact overlay observer before legacy metadata preflight, uses the snapshot builder as the start certification, pins one generation, and retains the post-query identity fence.
- SOLVE POST: `sol_12af9bd7108948a2`; the bounded `StartObserved -> GenerationPinned -> QueryComplete -> PostObserved` trace passes transition and safety rules without `DuplicateObservation`.
- Verification: focused warm-reuse, generation-route family, authoritative-correction, and full `mcp::tests` filters passed.

## Task 3 — Derive metadata from the pinned generation (`bd-rvzl`)

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`

**RED:** Add a test seam counting response-file stat/hash scans. Require zero scans on a generation response and metadata equality for full and compact formats. Prove identity mismatch never reports a match.

**GREEN:** Build `GraphResponseMetadata` from the matching graph pointer, the observed `SnapshotIdentity`, and the pinned generation's file manifests. Set `response_file_oids_match=Some(true)` only after the unchanged final identity fence. Remove `analyze_source_inner` from all certified generation-success branches; exact fallback may retain exact metadata analysis.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph mcp::tests -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**Evidence:**

- RED commit: `61ebf1591`; certified-generation diagnostics lacked the zero-scan contract (`Null` versus `0`).
- GREEN metadata: successful generation responses no longer collect response files for filesystem verification; they derive graph build/head fields from the matching pointer and freshness fields from the unchanged `SnapshotIdentity`.
- SOLVE POST: `sol_0c0a3ede474748b6`; the complete six-step response trace reaches `ResponseReturned`, satisfies every transition and safety rule, and never enters `DuplicateValidation`.
- Verification: focused generation metadata test passed; `spur-graph --lib` passed 491 tests with 3 ignored; formatting check passed.

## Task 4 — Measure the release gate (`bd-2pa0`)

**Files:**

- Modify: `crates/spur-graph/benches/overlay.rs`
- Modify: `crates/spur-graph/tests/perf_gates.rs`
- Modify: this plan

**RED:** Add structural gates for exactly two request fences and zero response metadata scans; fail before the new diagnostics exist.

**GREEN:** Extend the existing 30-run small/medium/large matrix with start observation, generation build/reuse, query, final observation, metadata derivation, and complete MCP timings plus correctness digests. Timing assertions remain report-only across hosts; structural counts are enforced in tests.

Release the token lease only if a future supported token seam passes all race/fallback gates and complete warm p95 is at most 10 ms. With the current exact CLI observer, report the remaining two-fence cost rather than claiming the under-10 ms target.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph --test perf_gates -- --nocapture
scripts/spur-cargo bench -p spur-graph --bench overlay -- --sample-size 30
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo fmt -- --check
```

**Evidence:**

- RED commit: `7809d4602`; all three deterministic cells failed because the full-MCP structural fields were absent.
- Matrix command: `SPUR_GRAPH_TASK6_MATRIX=1 SPUR_GRAPH_RELEASE_REPEATS=30 scripts/spur-cargo bench -p spur-graph --bench overlay -- task6_matrix_only --noplot`.
- Raw report: `.spur/bench-evidence/task6-overlay-generation-matrix.json`; 2,332 lines, 68,265 bytes; SHA-256 `8bfd67aaf4bf0f82314904f3e59d77ec35ab58088715a269f5a968d35cbf947a`.

| Project | Complete warm MCP p50 / p95 ms | One exact Git observation p50 / p95 ms | Metadata derivation p50 / p95 ms |
|---|---:|---:|---:|
| small untracked-heavy | 94.201 / 97.384 | 38.167 / 39.656 | 0.000292 / 0.000417 |
| medium dirty Rust | 94.932 / 96.690 | 38.404 / 43.459 | 0.000292 / 0.000375 |
| large mostly-clean polyglot | 96.840 / 100.471 | 38.530 / 40.540 | 0.000292 / 0.000417 |

- All cells: 30/30 matching digests, zero identity mismatches, exactly two validation observations per request, zero response metadata scans, zero warm overlay finalization stages.
- Compared with the prior 128–132 ms p50 and 132–139 ms p95 matrix, complete warm latency improved by roughly 26–28%. The remaining floor is the two 38–41 ms exact Git observations.
- Release verdict: exact-observer Auto is structurally safe; `complete_warm_under_10ms=false`; token lease remains disabled with blocker `no_supported_synchronous_git_token_fence`.
- SOLVE POST: `sol_070b930957e54d9d`; the measured decision trace reaches `ExactObserverReleased`, while `TokenLeaseReleased` remains outside the safe state set.
- Verification: `scripts/spur-cargo test -p spur-graph --lib` passed 491 tests (3 ignored); `scripts/spur-cargo test -p spur-graph --test perf_gates` passed all 4 active gates (9 ignored); the emitted evidence validator passed all 4 Task 6 gates; formatting and diff checks passed.
- Baseline caveat: the unfiltered crate command still fails seven extractor golden tests because structured JSON extraction indexes each fixture's `expected_graph_index.json`. The same failure was reproduced unchanged at the design baseline `fe6eaea1b`, so it is outside this task's three-file diff.

## Commit sequence

Each task produces an intent-focused RED commit and a GREEN/evidence commit:

```text
test(spur-graph): validation-lease task-N define <contract>
feat(spur-graph): validation-lease task-N implement <behavior>
```

Preserve the user's unrelated dirty files and commit only task-scoped paths.
