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
   validation, directly combined warm validation, delta construction, overlay
   query, response shaping, and total session. The combined stage intentionally
   overlaps its two decomposition stages so its own sample distribution can
   supply p50/p95; never present a sum of separately sampled percentiles as a
   percentile of the combined work.
3. Shape one `code_search` response by transforming and serializing the
   already-produced search result. Response shaping and the response portion
   of total session must execute no symbol, manifest, caller, or other graph
   query.
4. Make invalid/missing fixtures fail with actionable messages before the
   measurement loop.
5. Run at least 30 warm samples for all three project classes from disposable
   clones/worktrees. Record p50, p95, command, fixture identity, Git version,
   tracked/dirty counts, and correctness digest in a `PRE results` section.
6. Preserve raw Criterion `sample.json` and `estimates.json` below a stable
   evidence root outside the disposable worker worktree, verify the files
   before reporting, and checksum them. Do not enable or persist repository or
   global Git configuration.

**Verify:**

```bash
SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay --no-run
SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- --noplot
scripts/spur-cargo fmt --all -- --check
```

**Acceptance:** the same command shape can target all three fixtures, reports
the latency decomposition and digest, directly measures combined warm
validation, shapes one response without extra graph operations, retains raw
artifacts after worker cleanup, and has a recorded SOLVE PRE/POST pair.

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

Task 1 (`bd-2je4`) completed the first measurement harness on 2026-08-24.
These PRE-production measurements are retained as provenance, but the reviewed
response-shaping, percentile, and artifact-path evidence below is superseded by
the corrected `bd-38yt` retry. Neither attempt is a release decision for the
later fsmonitor/cache implementation.

#### Bounds, RED, and SOLVE evidence

- Catalog navigation selected implemented hard rule
  `resource.request_within_limit` (family `resource`, profile `capacity`, rule
  version 1).
- **SOLVE PRE / measurement-only RED:** `sol_3555c3212e224caf`, raw status
  `unsat`, outcome `fail`. Facts modeled the original query-path and
  construction sample shortfalls as 10 and 20 respectively, requested
  measurement windows of 8 and 10 seconds against a 10-second bound, and the
  strict `<30 ms` warm-validation gate as integer request/limit 29. Invariant:
  zero sample shortfall, bounded measurement window, and a 29 ms maximum.
- The focused bad-fixture check exited 101 before any measurement with
  `invalid SPUR_GRAPH_PERF_FIXTURE ... No such file or directory`. Raw output:
  `/private/tmp/spur-task1-verify.DGC2B3/bad-fixture.txt`.
- **SOLVE POST:** `sol_131bd3c442e14900`, raw status `sat`, outcome `pass`.
  The same rule and resource shape was applied to `quack_flamegraph`, `spur`,
  and `otobank`: `warm_sample_shortfall=0 <= 0`, requested
  `measurement_timeout_seconds=3 <= 10`, and
  `warm_validation_gate_ms=29 <= 29`. This proves the probe applies the same
  finite configuration to every class; it does **not** claim that the current
  exact implementation passes the later release gate.
- Criterion was configured for 30 post-warm-up samples per stage and a
  3-second requested measurement window. For slow stages Criterion extended
  collection time rather than reducing the sample count; every raw
  `sample.json` contains exactly 30 samples.
- GREEN verification: the final benchmark compiled in optimized bench mode in
  `/private/tmp/spur-task1-verify.DGC2B3/repo` with
  `SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay --no-run`;
  direct `rustfmt --edition 2021 --check` and `git diff --check` passed.
  The assigned clean base itself cannot run the wrapper compile or workspace
  fmt because tracked `crates/spur-graph/src/extract/mod.rs` declares
  `openapi`, while `crates/spur-graph/src/extract/openapi.rs` exists only as an
  untracked file in the main checkout. The disposable verification copy
  supplied that unchanged module without adding it to this task's diff.
  Scope signal `6d488e0d-88c6-4c53-9537-d40705162013` records this pre-existing
  dependency; both wrapper failures remain `E0583`, not benchmark failures.

#### Fixture identity and correctness digest

All repositories were cloned under `/private/tmp/spur-overlay-pre.JwtAoI`.
Tracked edits were replayed with `git diff --binary | git apply`; the exact
untracked pathname set was materialized as empty files so Git observed the
same dirty-record shape without copying heavyweight caches. Source repositories
and their Git configuration were not written. Every measured Git command used
`GIT_OPTIONAL_LOCKS=0`, `-c core.fsmonitor=false`, and
`-c core.untrackedCache=false`.

Measured Git: `git version 2.39.3 (Apple Git-146)`.

| Class / project | Revision | Tracked / dirty | Indexed sources | Query / changed file | Graph hash | Correctness digest |
|---|---|---:|---:|---|---|---|
| small, untracked-heavy / quack-flamegraph | `bcc78d53cbf6e1564490af5da106311b1eedec43` | 21 / 361 | 3 | `extension_entrypoint` / `src/lib.rs` | `c3684ef7c7139e0ea5d41b01f5100b994ff7b75e47ad5942fdb990987d139d8e` | `2a2d1484e528a09e9fcc09a7f96e089f4c7d7f5343396169eddbf2c00f102149` |
| medium dirty Rust / spur | `8358179ece0c488355b412749632e06276f2d3f7` | 3,214 / 223 | 3,231 | `handle_code_search` / `crates/spur-graph/src/mcp/mod.rs` | `f3f98fa5a5ceae2ce31ac75fd2dbb93a35f80fd5e9e30d555d92469c192827f6` | `8f3a5d83c008a61672afad6d5d370c2380ad679e316f5239ee049cbffa1371ec` |
| large mostly-clean polyglot / otobank | `4e934d438c009a875658466be2b811264d4295fb` | 4,913 / 33 | 4,295 | `run_refresh_tick` / `apps/api/src/bin/netops_worker.rs` | `4d2d488034e5352c8f0a599f29a71623ed94f5eac34352ba7bed9bdb2b6cd77d` | `f757b0edf5bfc4b6b5bf66380ca8c91e7f0aef6073312ec57d49164e301b88cd` |

All three manifests reported `artifact_indexed_commit_oid=null`; revision and
graph-content hash are therefore recorded independently rather than implying a
commit binding the artifact does not declare.

#### Stage timing matrix

Values are milliseconds. p50 is the mean of sorted samples 15 and 16; p95 is
nearest-rank sample 29 of 30. In this first attempt, `response shaping` also
performed symbol, file-manifest, and caller graph queries, and `total session`
repeated those queries. Those two stage names were semantically inaccurate;
their values are retained only to preserve PRE provenance.

| Project | Stage | p50 (ms) | p95 (ms) |
|---|---|---:|---:|
| quack-flamegraph | base Parquet query | 0.111 | 0.116 |
| quack-flamegraph | Git observation | 12.694 | 13.567 |
| quack-flamegraph | snapshot/OID validation | 13.677 | 14.946 |
| quack-flamegraph | delta construction | 69.695 | 73.397 |
| quack-flamegraph | overlay query | 0.114 | 0.118 |
| quack-flamegraph | response shaping | 0.359 | 0.366 |
| quack-flamegraph | total session | 97.082 | 102.161 |
| spur | base Parquet query | 13.066 | 13.408 |
| spur | Git observation | 26.007 | 27.202 |
| spur | snapshot/OID validation | 17.299 | 18.923 |
| spur | delta construction | 279.825 | 290.952 |
| spur | overlay query | 13.234 | 13.626 |
| spur | response shaping | 39.179 | 40.505 |
| spur | total session | 404.154 | 409.880 |
| otobank | base Parquet query | 15.196 | 15.660 |
| otobank | Git observation | 27.210 | 28.152 |
| otobank | snapshot/OID validation | 18.709 | 20.090 |
| otobank | delta construction | 92.878 | 96.826 |
| otobank | overlay query | 14.814 | 15.492 |
| otobank | response shaping | 48.630 | 49.933 |
| otobank | total session | 230.367 | 243.598 |

The first attempt's arithmetic `sum(stage p95)` for Git observation plus
snapshot/OID validation was 28.513 ms, 46.125 ms, and 48.242 ms respectively.
These are sums of separately sampled percentiles, **not** p95 estimates of the
combined work, and they are not direct gate evidence. The corrected retry below
adds one combined stage and uses its measured distribution for the gate claim.

#### Exact commands and raw output

Fixture recipe (run once for each project name):

```bash
PRE_ROOT=/tmp/spur-overlay-pre.JwtAoI
for name in quack-flamegraph spur otobank; do
  source_repo="/Volumes/Projects/Projects/$name"
  fixture_repo="$PRE_ROOT/$name"
  git clone --quiet --shared "$source_repo" "$fixture_repo"
  if ! GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false diff --quiet --no-ext-diff --; then
    GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false diff --binary --no-ext-diff -- | git -C "$fixture_repo" apply
  fi
  while IFS= read -r -d '' relative_path; do
    mkdir -p "$fixture_repo/${relative_path:h}"
    touch "$fixture_repo/$relative_path"
  done < <(GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false ls-files --others --exclude-standard -z)
done
```

Benchmark commands (all run from the disposable verification copy with the
same optimized benchmark binary):

```bash
SPUR_GRAPH_PERF_REPO='/tmp/spur-overlay-pre.JwtAoI/quack-flamegraph' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/quack-flamegraph/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='extension_entrypoint' SPUR_GRAPH_PERF_CHANGED_FILE='src/lib.rs' SPUR_GRAPH_PERF_LABEL='quack_flamegraph' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot

SPUR_GRAPH_PERF_REPO='/tmp/spur-overlay-pre.JwtAoI/spur' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/spur/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='handle_code_search' SPUR_GRAPH_PERF_CHANGED_FILE='crates/spur-graph/src/mcp/mod.rs' SPUR_GRAPH_PERF_LABEL='spur' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot

SPUR_GRAPH_PERF_REPO='/tmp/spur-overlay-pre.JwtAoI/otobank' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/otobank/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='run_refresh_tick' SPUR_GRAPH_PERF_CHANGED_FILE='apps/api/src/bin/netops_worker.rs' SPUR_GRAPH_PERF_LABEL='otobank' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot
```

Raw console outputs:

- `/private/tmp/spur-overlay-pre.JwtAoI/quack-flamegraph.txt`
- `/private/tmp/spur-overlay-pre.JwtAoI/spur.txt`
- `/private/tmp/spur-overlay-pre.JwtAoI/otobank.txt`
- `/private/tmp/spur-overlay-pre.JwtAoI/criterion-percentiles.tsv`

Raw Criterion directories:

- `/Volumes/Projects/Projects/spur/.spur/worktrees/d789546a-5459-40cb-babb-e56ea8e43914/target/criterion/overlay_stage_probe_quack_flamegraph/`
- `/Volumes/Projects/Projects/spur/.spur/worktrees/d789546a-5459-40cb-babb-e56ea8e43914/target/criterion/overlay_stage_probe_spur/`
- `/Volumes/Projects/Projects/spur/.spur/worktrees/d789546a-5459-40cb-babb-e56ea8e43914/target/criterion/overlay_stage_probe_otobank/`

Each stage directory originally retained `new/sample.json`,
`new/estimates.json`, and the Criterion report artifacts. Those paths were
inside Attempt 1's disposable worktree and temporary root and therefore do not
survive delegation cleanup; they are superseded by the verified stable evidence
root below.

### Corrected Task 1 retry evidence

Retry issue `bd-38yt` corrected the three review defects without changing the
PRE matrix parameterization. This is still a measurement of the pre-production
exact path. It does not replace the Task 6 release POST.

#### Measurement RED, GREEN, and response shape

The focused measurement-semantic guard was run before the correction:

```bash
zsh -c 'if awk '\''/fn shape_response\(/,/^}/'\'' crates/spur-graph/benches/overlay.rs | rg -q '\''symbol_by_id|file_manifest_by_path|find_caller_edges'\''; then print -u2 "RED: response shaping still performs graph queries"; exit 1; fi'
```

It exited 1 with the expected RED message because `shape_response` executed
all three forbidden graph operations. The identical guard exited 0 after
GREEN. The corrected function accepts `&SearchResult`, creates the candidate
response rows, and serializes one JSON value. `stage_response_shaping` consumes
the precomputed `overlay_search`; `total_session_digest` passes its one overlay
query result to the same pure function. Neither path performs a post-search
graph query.

`stage_warm_validation_combined` runs one Git observation followed by one
snapshot/OID validation in the same Criterion iteration. Its distribution is
the only warm-validation p50/p95 gate evidence below. The individual stages
remain decomposition evidence and are intentionally overlapping; their
percentiles are never added and relabeled as a combined percentile.

#### Corrected SOLVE POST

- Catalog navigation reconfirmed implemented hard rule
  `resource.request_within_limit` (family `resource`, profile `capacity`, rule
  version 1).
- Persisted **SOLVE POST**: `sol_9f65330e34214948`, raw status `sat`, outcome
  `pass`, Z3 4.16.0. It uses the same three-workload/resource shape as the
  original POST: for `quack_flamegraph`, `spur`, and `otobank`,
  `warm_sample_shortfall=0 <= 0`,
  `measurement_timeout_seconds=3 <= 10`, and
  `warm_validation_gate_ms=29 <= 29`.
- This same-shape POST proves that every corrected run applied the declared
  finite probe bounds. It deliberately does not encode the observed PRE p95s
  as passing the future release gate; the directly measured table shows which
  current projects fail.

#### Corrected fixture identity and digest

The source repositories and their Git configuration were not written. Dirty
state was replayed into shared disposable clones under
`/private/tmp/spur-overlay-corrected.HJ7UeA`; tracked diffs were applied and
untracked pathname sets were materialized as empty files. Every measured Git
command used `GIT_OPTIONAL_LOCKS=0`, `-c core.fsmonitor=false`, and
`-c core.untrackedCache=false`.

Measured Git: `git version 2.55.0`.

| Class / project | Revision | Tracked / dirty | Indexed sources | Query / changed file | Graph hash | Correctness digest |
|---|---|---:|---:|---|---|---|
| small, untracked-heavy / quack-flamegraph | `bcc78d53cbf6e1564490af5da106311b1eedec43` | 21 / 361 | 3 | `extension_entrypoint` / `src/lib.rs` | `c3684ef7c7139e0ea5d41b01f5100b994ff7b75e47ad5942fdb990987d139d8e` | `6ec2c488c05c418f0e32a76aa425695d1ef773be501d210181728be6a83f1573` |
| medium dirty Rust / spur | `8358179ece0c488355b412749632e06276f2d3f7` | 3,214 / 223 | 3,231 | `handle_code_search` / `crates/spur-graph/src/mcp/mod.rs` | `f3f98fa5a5ceae2ce31ac75fd2dbb93a35f80fd5e9e30d555d92469c192827f6` | `b9b804495e4d52a60341f5e0a89503615ba8a95e8cfbd6c77b36eb18ddf17aa8` |
| large mostly-clean polyglot / otobank | `4e934d438c009a875658466be2b811264d4295fb` | 4,913 / 37 | 4,295 | `run_refresh_tick` / `apps/api/src/bin/netops_worker.rs` | `4d2d488034e5352c8f0a599f29a71623ed94f5eac34352ba7bed9bdb2b6cd77d` | `f6201764a1846047fc8b92a4e3a53ae19960b00a2598a3bec6a35ad590ab6838` |

All manifests again reported `artifact_indexed_commit_oid=null`. Otobank has
four additional untracked paths versus Attempt 1; its exact 37-record shape is
recorded rather than silently describing it as the older 33-record fixture.

#### Corrected 30-sample timing matrix

Values are milliseconds derived from each persisted `sample.json` as
`times[i] / iters[i]`. p50 is the mean of sorted samples 15 and 16; p95 is the
nearest-rank sample 29 of 30.

| Project | Stage | p50 (ms) | p95 (ms) |
|---|---|---:|---:|
| quack-flamegraph | base Parquet query | 0.1079 | 0.1147 |
| quack-flamegraph | Git observation | 9.624 | 10.507 |
| quack-flamegraph | snapshot/OID validation | 7.466 | 8.344 |
| quack-flamegraph | **combined warm validation** | **17.027** | **17.480** |
| quack-flamegraph | delta construction | 50.668 | 52.696 |
| quack-flamegraph | overlay query | 0.1124 | 0.1186 |
| quack-flamegraph | response shaping (pure serialization) | 0.00244 | 0.00250 |
| quack-flamegraph | total session | 67.818 | 71.496 |
| spur | base Parquet query | 13.367 | 13.502 |
| spur | Git observation | 22.196 | 23.623 |
| spur | snapshot/OID validation | 10.819 | 11.324 |
| spur | **combined warm validation** | **33.332** | **35.654** |
| spur | delta construction | 262.599 | 269.545 |
| spur | overlay query | 13.434 | 13.790 |
| spur | response shaping (pure serialization) | 0.00452 | 0.00458 |
| spur | total session | 338.491 | 389.670 |
| otobank | base Parquet query | 15.371 | 16.395 |
| otobank | Git observation | 23.378 | 24.125 |
| otobank | snapshot/OID validation | 12.473 | 13.155 |
| otobank | **combined warm validation** | **36.650** | **40.917** |
| otobank | delta construction | 74.082 | 78.047 |
| otobank | overlay query | 14.829 | 15.874 |
| otobank | response shaping (pure serialization) | 0.00447 | 0.00464 |
| otobank | total session | 151.341 | 153.187 |

The directly measured combined warm-validation p95 is therefore 17.480 ms,
35.654 ms, and 40.917 ms. Quack passes the strict `<30 ms` future release
gate; Spur and otobank fail it on the current exact path. This is the truthful
PRE signal later tasks must improve.

#### Exact corrected commands

Fixture preparation used this process-scoped recipe:

```bash
CORRECTED_ROOT=/private/tmp/spur-overlay-corrected.HJ7UeA
EVIDENCE_ROOT=/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-38yt-corrected-task1
mkdir -p "$EVIDENCE_ROOT/console" "$EVIDENCE_ROOT/criterion"
for project_name in quack-flamegraph spur otobank; do
  source_repo="/Volumes/Projects/Projects/$project_name"
  fixture_repo="$CORRECTED_ROOT/$project_name"
  GIT_OPTIONAL_LOCKS=0 git -c core.fsmonitor=false -c core.untrackedCache=false clone --quiet --shared "$source_repo" "$fixture_repo"
  if ! GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false diff --quiet --no-ext-diff --; then
    GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false diff --binary --no-ext-diff -- | GIT_OPTIONAL_LOCKS=0 git -C "$fixture_repo" -c core.fsmonitor=false -c core.untrackedCache=false apply
  fi
  while IFS= read -r -d '' relative_path; do
    mkdir -p "$fixture_repo/${relative_path:h}"
    touch "$fixture_repo/$relative_path"
  done < <(GIT_OPTIONAL_LOCKS=0 git -C "$source_repo" -c core.fsmonitor=false -c core.untrackedCache=false ls-files --others --exclude-standard -z)
done
```

The three benchmark commands were run from the disposable verification clone
with the same optimized binary and persistent target:

```bash
SPUR_GRAPH_PERF_REPO='/private/tmp/spur-overlay-corrected.HJ7UeA/quack-flamegraph' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/quack-flamegraph/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='extension_entrypoint' SPUR_GRAPH_PERF_CHANGED_FILE='src/lib.rs' SPUR_GRAPH_PERF_LABEL='quack_flamegraph_corrected' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 CARGO_TARGET_DIR='/Volumes/Projects/Projects/spur/.spur/worktrees/618383bb-51be-400c-8377-fe59b4a66287/target' SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot

SPUR_GRAPH_PERF_REPO='/private/tmp/spur-overlay-corrected.HJ7UeA/spur' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/spur/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='handle_code_search' SPUR_GRAPH_PERF_CHANGED_FILE='crates/spur-graph/src/mcp/mod.rs' SPUR_GRAPH_PERF_LABEL='spur_corrected' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 CARGO_TARGET_DIR='/Volumes/Projects/Projects/spur/.spur/worktrees/618383bb-51be-400c-8377-fe59b4a66287/target' SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot

SPUR_GRAPH_PERF_REPO='/private/tmp/spur-overlay-corrected.HJ7UeA/otobank' SPUR_GRAPH_PERF_FIXTURE='/Volumes/Projects/Projects/otobank/.spur/graph/CURRENT' SPUR_GRAPH_PERF_QUERY='run_refresh_tick' SPUR_GRAPH_PERF_CHANGED_FILE='apps/api/src/bin/netops_worker.rs' SPUR_GRAPH_PERF_LABEL='otobank_corrected' SPUR_GRAPH_PERF_SAMPLE_SIZE=30 SPUR_GRAPH_PERF_MEASUREMENT_SECONDS=3 CARGO_TARGET_DIR='/Volumes/Projects/Projects/spur/.spur/worktrees/618383bb-51be-400c-8377-fe59b4a66287/target' SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay -- overlay_stage_probe --noplot
```

The bad-fixture command used the quack environment above with
`SPUR_GRAPH_PERF_FIXTURE=/private/tmp/spur-overlay-corrected.HJ7UeA/missing-fixture`.
It exited 101 before measurement with actionable text:
`invalid SPUR_GRAPH_PERF_FIXTURE ... No such file or directory`.

#### Compile, format, and persistent raw artifacts

- `CARGO_TARGET_DIR=.../618383bb-51be-400c-8377-fe59b4a66287/target
  SPUR_REMOTE=0 scripts/spur-cargo bench -p spur-graph --bench overlay
  --no-run` passed in the disposable verification clone and produced
  `overlay-693f3cb4ccaa1274`. It reported only four pre-existing dead-code
  warnings in `crates/spur-graph/src/mcp/mod.rs`; the benchmark emitted no
  warning.
- `scripts/spur-cargo fmt --all -- --check` and
  `rustfmt --edition 2021 --check crates/spur-graph/benches/overlay.rs` passed
  in the disposable verification clone; the direct scoped `rustfmt` check and
  `git diff --check` also passed in the assigned worktree. As in Attempt 1,
  workspace fmt in the assigned worktree alone fails before formatting because
  tracked `extract/mod.rs` declares missing `openapi.rs`; the verification
  clone supplied the unchanged untracked module without adding it to this
  task's diff.
- Stable evidence root:
  `/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-38yt-corrected-task1`.
  It is outside `.spur/worktrees` and remained valid after the active
  `/private/tmp/spur-overlay-corrected.HJ7UeA` fixture root was moved to Trash.
- Raw Criterion group directories are
  `criterion/overlay_stage_probe_{quack_flamegraph,spur,otobank}_corrected/`.
  Every one of the 24 stage directories contains `new/sample.json` with 30
  `iters` and 30 `times`, plus `new/estimates.json`.
- Derived exact percentiles are in `criterion-percentiles.tsv`; raw console,
  compile, fmt, bad-fixture, and diff-check outputs are under `console/`.
  `SHA256SUMS` contains 202 checksums covering those persisted files.

### POST results

Pending Task 6.
