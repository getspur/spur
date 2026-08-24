# Git Fsmonitor Overlay Snapshot Cache Implementation Plan

**Source spec:** `docs/superpowers/specs/2026-08-24-code-overlay-fsmonitor-cache-design.ipynb`

**Approved design epic:** `bd-3h31` (closed after written-spec approval)

**Authoring epic:** `bd-3a6y`

**Submitted plan:** `fb5c7280-e71b-4d77-8bb4-32df84107ec7`

**Orchestrator execution epic:** `bd-2m6c`

**Plan label:** `plan:code-overlay-fsmonitor-20260824`

**Graph-plan hash:** `c77b20b9fac6fcc919a853f0c101d44837b91cf939b1c28890facd97b3749c57`

**Execution issue mapping:**

| Task | Beads issue |
|---|---|
| Reproducible PRE/POST probe | `bd-2je4` |
| Porcelain-v2 fsmonitor observer | `bd-juvn` |
| Stable snapshot lifecycle | `bd-1l0y` |
| OID-addressed cache integration | `bd-258m` |
| First-base-query reuse | `bd-1jzq` |
| Cross-project release gates | `bd-2uzl` |

**Orchestrator issue mapping:**

| Authored issue | Runtime issue |
|---|---|
| `bd-2je4` | `bd-2d5w` |
| `bd-juvn` | `bd-3sgq` |
| `bd-1l0y` | `bd-13xu` |
| `bd-258m` | `bd-2hli` |
| `bd-1jzq` | `bd-3u6e` |
| `bd-2uzl` | `bd-34p7` |

**Planning solve:** `sol_f48ab4639f9b4075` — the six-task assignment,
capacity, and finish/start precedence model passed all 19 scheduling bindings.

**Formal design evidence:**

- `OVERLAY-ROUTING`, cell `395f5036-959f-47cb-acfb-ea278ac7c42e`,
  report `d453cded929c886aff16b5a0e3ad40120816f38f007b6f3e44747421335f3dd0`
  (5/5 checks);
- `OVERLAY-SNAPSHOT-LIFECYCLE`, cell
  `0f0d7404-2c8f-4407-a758-30723686758a`, report
  `c4a4b878c692b7203405182ad935e7cc5d3d6ac09047e0ccab33dd6936e88d0d`
  (14/14 checks); and
- `OVERLAY-PERFORMANCE-GATE`, cell
  `d26c8c71-bde3-47c9-9989-672218f922c9`, report
  `625a2c94f45c84a73bb4f288c7aa242e88d45b89e2479856005db3b278d9fd3d`
  (5/5 checks).

## Objective

Reduce warm `code_*` overlay validation from the observed 100–160 ms of
post-query work to a p95 below 30 ms while retaining exact working-tree
semantics. The implementation uses Git's built-in fsmonitor only when its
capability gates pass, keeps an OID-addressed snapshot for as long as its
identity remains valid, hashes only changed files, and reuses the first base
query when an overlay is required.

This plan does not increase the existing five-second metadata TTL as the
primary optimization. A TTL can only hide repeated work and creates stale
windows. The new snapshot has no time-based expiry: HEAD, index identity,
graph identity, status observations, and changed-file metadata determine
whether it remains valid.

## Measured starting point

- A direct Parquet query is about 40 ms.
- Hot `code_*` requests are about 142 ms; after the five-second metadata TTL,
  requests are about 198–200 ms.
- Explicit project selection adds about 29 ms.
- The current Git overlay scan took 53.8 ms on Spur with 3,061 indexed files
  and 170 changed paths.
- Repeated overlay query work took about 30–40 ms.
- On the current command shape, merely enabling fsmonitor was slower:
  status was 26.4 ms versus 23.8 ms, and the persisted
  status/`ls-files -t`/`ls-files -s` bundle was 64.7 ms. The optimization must
  therefore remove full-index sweeps; it must not claim a win from the flag
  alone.

The reproducible pre/post matrix uses disposable clones or worktrees and
three existing graph fixtures:

| Class | Project | Initial shape |
|---|---|---|
| small, untracked-heavy | `quack-flamegraph` | 21 tracked / 361 dirty records |
| medium Rust monorepo, dirty | `spur` | 3,213 tracked / 223 dirty records |
| large polyglot, mostly clean | `otobank` | 4,913 tracked / 33 dirty records |

Never mutate those repositories or their Git configuration. Copy/clone them
under `mktemp -d`, use process-scoped `git -c` options, and remove only the
temporary directory.

## Architecture

```text
Parquet client + graph identity
        |  (file manifest materialized once per artifact)
        v
OverlaySnapshotStore[canonical worktree]
        |
        +-- gated Git observer
        |     built-in fsmonitor + untracked cache
        |     OR exact porcelain-v2 fallback
        |
        +-- conditional indexed-HEAD..HEAD path delta
        +-- changed-file metadata revalidation
        +-- changed-only blob OID hashing
        v
stable (path, state, OID) fingerprint
        |
        v
OverlayDelta cache / singleflight
        |
        v
request-scoped replay of first base query + overlay delta query
        |
        v
one exact code_* response
```

The two root tasks have disjoint files and may run concurrently. Tasks 3–5
are dependency-ordered because they touch the same MCP integration seam.

## Mandatory protocol for every task

No task may be closed with only a green test claim. Its issue and final worker
report must include all four evidence blocks below.

1. **SOLVE PRE:** call `solve_rule_spec` for the task's named family/rules,
   then persist a `solve_rules` verification or synthesis. Record the
   `solve_id`, raw status, facts, and invariant. If the catalog reports the
   family unavailable, record that exact result before using
   `solve_constraint_spec` → `solve_constraint_check` → `solve_constraints`.
2. **RED:** add the smallest behavioral test first, run the focused command,
   and preserve the expected assertion/behavior failure. A compile failure,
   test typo, or missing fixture is not acceptable RED evidence.
3. **GREEN:** implement only enough production code to satisfy the RED test,
   then run focused tests and formatting with `scripts/spur-cargo`; never run
   bare `cargo`.
4. **SOLVE POST + MEASURE:** rerun the same model with observed implementation
   facts, preserve a new `solve_id`, and compare the same pre/post measurement.
   A task whose POST model fails must remain open or escalate; do not relax an
   invariant to obtain `sat`.

For production tasks, commit the RED test before the GREEN implementation
when practical:

```text
test(spur-graph): <task-id> specify <behavior>
feat(spur-graph): <task-id> implement <behavior>
```

Task 1 is measurement/test infrastructure only, so the production-code RED
gate is not applicable; its probe validation and PRE baseline are still
required before later production tasks start.

## Task 1 — Make the cross-project pre/post probe reproducible

**Issue:** `bd-2je4`

**Depends on:** none

**Suggested worker:** `codex`

**Write scope:**

- `crates/spur-graph/benches/overlay.rs`
- `docs/superpowers/plans/2026-08-24-code-overlay-fsmonitor-cache.md`

**SOLVE invariant:** navigate `resource.request_within_limit`; model the
declared sample count, timeout, and 30 ms warm-validation limit. PRE records
the currently failing performance gate; POST proves the probe itself applies
the same finite bounds to every project.

**Steps:**

1. Extend the existing Criterion overlay benchmark to accept an explicit
   repository root, graph fixture, representative query/symbol, and changed
   file through `SPUR_GRAPH_PERF_*` variables. Defaults must preserve the
   current Spur benchmark.
2. Add named stages for base Parquet query, Git observation, snapshot/OID
   validation, delta construction, overlay query, response shaping, and total
   session. Do not combine stages in a way that re-labels unmeasured time.
3. Make invalid/missing fixtures fail with actionable messages before the
   measurement loop.
4. Run at least 30 warm samples for all three project classes from disposable
   clones/worktrees. Record p50, p95, command, fixture identity, Git version,
   tracked/dirty counts, and correctness digest in a `PRE results` section.
5. Preserve raw Criterion output paths. Do not enable or persist repository or
   global Git configuration.

**Verify:**

```bash
SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay --no-run
SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- --noplot
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** the same command shape can target all three fixtures, reports
the latency decomposition and digest, and has a recorded SOLVE PRE/POST pair.

## Task 2 — Add a gated porcelain-v2 fsmonitor observer

**Issue:** `bd-juvn`

**Depends on:** none

**Suggested worker:** `claude-code`

**Write scope:**

- `crates/spur-graph/src/git.rs`

**SOLVE invariant:** rerun `OVERLAY-ROUTING` with the catalog
`configuration.attribute_allowed_pair` and `configuration.requires_any`
rules. Fsmonitor is selected if and only if release enablement, built-in
support, local filesystem, and watcher health are all true; every other model
selects exact scan.

**RED cases:**

- porcelain-v2 ordinary, staged, unstaged, untracked, deletion, rename, copy,
  spaces, and malformed records;
- all capability truth-table branches;
- unsupported daemon, unhealthy watcher, non-local filesystem, and command
  failure route to exact fallback; and
- command assembly uses only process-scoped `git -c` options and never invokes
  `git config --global` or writes repository config.

**GREEN implementation:**

1. Introduce a typed porcelain-v2 observation preserving new path, old path
   for renames/copies, and worktree/index state.
2. Add an injectable capability decision so tests do not depend on the host
   watcher. Probe built-in support and health without mutating user config.
3. Execute one warm
   `git status --porcelain=v2 -z --untracked-files=all` observation. In the
   eligible route, supply built-in fsmonitor and untracked-cache options with
   process-scoped `-c`; otherwise run the exact command.
4. If the optimized command fails or becomes unhealthy, return a typed
   fallback signal and complete the exact observation in the same request.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph git::tests::porcelain_v2
scripts/spur-cargo test -p spur-graph git::tests::fsmonitor
scripts/spur-cargo check -p spur-graph
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** parser equivalence and the complete routing truth table pass;
SOLVE POST preserves all five `OVERLAY-ROUTING` checks.

## Task 3 — Build the stable overlay snapshot lifecycle

**Issue:** `bd-1l0y`

**Depends on:** Tasks 1 and 2

**Suggested worker:** `claude-code`

**Write scope:**

- `crates/spur-graph/src/mcp/overlay_snapshot.rs` (new)
- `crates/spur-graph/src/mcp/mod.rs`

**SOLVE invariant:** rerun `OVERLAY-SNAPSHOT-LIFECYCLE` with
`workflow.initial_state_allowed`, `workflow.transition_allowed`, and
`workflow.safety_invariant`. Required states are cold, validating, valid,
retrying, and exact-fallback; mutation during validation permits one retry,
then requires exact fallback.

**RED cases using real temporary Git repositories:**

- clean repeat reuses the snapshot without a full `ls-files` sweep;
- staged/unstaged edits and untracked files hash only changed paths;
- deletion produces a tombstone;
- rename produces old-path tombstone plus new-path OID;
- clean current HEAD ahead of indexed graph HEAD is found by a conditional
  HEAD-lag diff;
- index/HEAD mutation during validation retries once; a second mutation uses
  exact fallback; and
- identical normalized `(path,state,OID)` observations produce an identical
  fingerprint independent of enumeration order.

**GREEN implementation:**

1. Move overlay change discovery behind an `overlay_snapshot` module with a
   typed `SnapshotIdentity`, `OverlaySnapshot`, observer result, and route.
2. Key identity by canonical worktree, graph content hash, indexed HEAD,
   current HEAD, index identity, and normalized changed-state fingerprint.
3. Materialize the tracked index baseline only on cold/index-invalidated
   paths. On warm validation, consume the Task 2 observation and read/hash only
   changed supported files.
4. Run the indexed-HEAD..current-HEAD name/OID delta only when those OIDs
   differ. Preserve clean HEAD-lag correctness without a full tree scan.
5. Revalidate HEAD, index identity, and each read file's metadata after
   hashing; retry once, then exact fallback.
6. Keep the non-Git filesystem scan as the exact compatibility fallback.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph overlay_snapshot
scripts/spur-cargo test -p spur-graph changed_paths_for_overlay
scripts/spur-cargo check -p spur-graph
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** all lifecycle cases are exact against a fresh full scan, a
warm unchanged validation performs no full index/file sweep, and SOLVE POST
preserves 14/14 lifecycle checks.

## Task 4 — Integrate OID-addressed caches without TTL staleness

**Issue:** `bd-258m`

**Depends on:** Task 3

**Suggested worker:** `claude-code`

**Write scope:**

- `crates/spur-graph/src/mcp/overlay_snapshot.rs`
- `crates/spur-graph/src/mcp/request_cache.rs`
- `crates/spur-graph/src/query_client.rs`
- `crates/spur-graph/src/mcp/mod.rs`

**SOLVE invariant:** navigate `data_integrity.unique`,
`data_integrity.conditional_required`, and `data_integrity.temporal_consistency`.
One complete cache identity maps to one snapshot/delta; any changed identity
component invalidates reuse; an unchanged identity remains reusable without a
time expiry.

**RED cases:**

- same worktree and identity reuse Parquet file OIDs, snapshot, and delta after
  more than the old five-second TTL;
- graph hash, indexed HEAD, current HEAD, index identity, changed state/OID,
  deletion, and rename each invalidate the correct layer;
- two worktrees with the same path names never alias;
- concurrent identical requests share one build; and
- cache capacity remains bounded and eviction cannot return a mismatched
  entry.

**GREEN implementation:**

1. Memoize `ParquetClient::file_oids()` once per opened artifact/manifest;
   reopening on manifest identity change remains the invalidation boundary.
2. Replace the overlay delta's worktree-plus-`u64` key with the complete typed
   snapshot identity or a collision-resistant digest plus equality-checked
   identity. Do not treat `DefaultHasher` output alone as correctness identity.
3. Store validated snapshots per canonical worktree with bounded eviction;
   remove time-based expiry from this snapshot layer.
4. Preserve singleflight behavior and make followers receive the exact leader
   result/error.
5. Wire `overlay_delta_for_worktree` to consume the validated snapshot rather
   than reconstructing current OIDs on every request.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph request_cache
scripts/spur-cargo test -p spur-graph overlay_snapshot
scripts/spur-cargo test -p spur-graph query_client
scripts/spur-cargo check -p spur-graph
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** an unchanged snapshot survives beyond five seconds, every
declared identity mutation invalidates deterministically, and SOLVE POST
proves uniqueness/temporal consistency.

## Task 5 — Reuse the first base query during overlay refresh

**Issue:** `bd-1jzq`

**Depends on:** Task 4

**Suggested worker:** `claude-code`

**Write scope:**

- `crates/spur-graph/src/mcp/request_replay.rs` (new, if the replay seam is
  chosen)
- `crates/spur-graph/src/mcp/mod.rs`
- `crates/spur-graph/src/overlay.rs` (only if required by the minimal merge
  seam)

**SOLVE invariant:** use `data_integrity.cardinality` for exactly one base
query per request and `workflow.safety_invariant` for semantic equivalence
between direct-overlay and replayed-overlay results.

**RED cases with a counting `GraphQueryClient`:**

- clean successful request performs exactly one base query;
- dirty successful request performs exactly one base query plus only the
  required delta-side work;
- base not-found followed by a new/renamed overlay symbol still succeeds
  without a second Parquet query;
- callers, callees, resolve, file-symbols, read-symbol, and search preserve
  their current response ordering, limits, metadata, and error behavior; and
- stale-budget and overlay-failure fallbacks never return a partially merged
  response.

**GREEN implementation:**

1. Capture request-scoped base query results in a typed replay/memo layer, or
   introduce an equally small merge seam that consumes the already computed
   base result. The second overlay handler pass must not execute the same
   Parquet operation again.
2. Cache only within one request. Do not introduce a cross-request result
   cache whose key must replicate every tool argument.
3. Apply overlay shadowing/tombstones before limit and response formatting so
   results equal a direct query against a freshly rebuilt graph.
4. Keep the existing latency-budget and rebuild fallback semantics.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph mcp::tests::overlay
scripts/spur-cargo test -p spur-graph request_replay
scripts/spur-cargo test -p spur-graph --test mcp_code_graph
scripts/spur-cargo check -p spur-graph
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** instrumented tests prove base-query cardinality is exactly
one for clean, dirty, and overlay-new-symbol cases; all selected tool families
are byte/JSON equivalent to the direct-overlay oracle; SOLVE POST passes.

## Task 6 — Run the post matrix and enforce the release gate

**Issue:** `bd-2uzl`

**Depends on:** Task 5

**Suggested worker:** `codex`

**Write scope:**

- `crates/spur-graph/benches/overlay.rs`
- `docs/superpowers/plans/2026-08-24-code-overlay-fsmonitor-cache.md`

**SOLVE invariant:** rerun `OVERLAY-PERFORMANCE-GATE` with
`resource.request_within_limit` for the latency bound and
`data_integrity.value_range`/`data_integrity.cardinality` for zero correctness
failures and exactly one base query.

**Steps:**

1. Run the complete Spur unit/integration suite before performance claims.
2. Recreate all three disposable project fixtures at the exact PRE revisions
   and dirty-state recipes. Run the identical sample counts and stage names.
3. Record p50/p95 for every stage, base-query count, fallback route, watcher
   health, output digest, and speedup/regression versus PRE.
4. Exercise clean, one edited file, many edited files, untracked-heavy,
   delete, rename, HEAD-lag, fsmonitor unsupported, watcher failure, and
   concurrent request scenarios.
5. Repeat correctness cases at least three times. Require zero digest/result
   mismatches against exact scan/fresh rebuild.
6. Append exact commands, raw output paths, table, solve IDs, and the release
   decision. If a gate fails, do not hide it with a larger TTL or change the
   threshold in this task; emit a SPUR escalation identifying the measured
   stage.

**Verify:**

```bash
scripts/spur-cargo test -p spur-graph
scripts/spur-cargo check --workspace
SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- --noplot
```

**Acceptance gates:**

- warm unchanged snapshot validation p95 is below 30 ms in every project
  class;
- each measured `code_*` request executes exactly one base Parquet query;
- correctness failures are zero across all scenarios and repetitions;
- optimized and exact-fallback routes are both exercised; and
- a reproducible PRE/POST latency matrix and SOLVE PRE/POST evidence are
  committed.

## Results

### PRE results

Pending Task 1.

### POST results

Pending Task 6.
