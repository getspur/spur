# Watcher-Maintained Overlay Generation Implementation Plan

> **Execution rule:** Every task is delegated to the `codex` worker with model
> `gpt-5.6-sol` and reasoning effort `xhigh`. Every task must record a persisted,
> reloadable SOLVE PRE result, commit an observed behavioral RED test before
> production edits, implement GREEN, record a persisted SOLVE POST result, and
> leave a clean worker worktree.

**Design source:**
`docs/superpowers/specs/2026-08-26-code-overlay-validation-lease-design.ipynb`

**Approved design epic:** `bd-hxut` (closed)

**Authoritative solver evidence:**

- Complete MaxSMT optimum: `sol_1a31eed6a32346a6` (242/250).
- Latency/complexity Pareto frontier: `sol_7b317858384d4315`.
- Hard feasibility: `sol_e1553eda5054495`.
- Mandatory-seam ablation: `sol_7c0e05d984244a58` (unsat).
- Missing watcher-liveness counterexample: `sol_b7b40a3b912b4f51`.
- Conditional bounded-liveness proof: `sol_1e5433d46e8e42ab`.

## Goal

Move exact Git observation and overlay-generation maintenance out of each
`code_*` request. A long-lived per-worktree actor owns the current immutable
`OverlayGeneration`; a healthy warm request performs one atomic state load,
pins that generation for the whole request, and performs no `git status`, HEAD,
index, merge, filter, overlay sort, or stable-ID dedup work.

Watchman is the primary change provider. `notify` is the conservative
cross-platform fallback. Exact Git remains authoritative for initialization,
provider loss/overflow, ambiguous Git metadata changes, and explicit strict
mode. Watcher freshness is a measured SLO, not a hard correctness claim.

## Target architecture

```text
worktree + gitdir + commondir
          |
          v
  Watchman primary ----> notify fallback
          |                     |
          +---- normalized event batch ----+
                                           v
                              one per-worktree actor
                              arm -> exact scan -> replay
                                           |
                          changed paths / trust revocation
                                           |
                              background exact generation rebuild
                                           |
                              ArcSwap<PublishedState>
                                           |
                     request load -> Arc pin -> code_* query
```

`PublishedState` is one coherent value containing the source-set identity,
provider/cursor set, monotonic epoch, trust state, exact snapshot identity, and
`Arc<OverlayGeneration>`. It is published only after every generation index is
complete. A request never combines fields from different epochs.

## Required invariants

1. The watched source set is complete: canonical worktree, resolved gitdir, and
   resolved commondir, with duplicate roots collapsed but roles preserved.
2. Initialization is race-free: capture cursor `c0`, arm all roots, run one
   exact Git observation, replay events since `c0`, then publish generation 0.
3. Provider error, overflow, recrawl/fresh-instance, channel disconnect, root
   replacement, or ambiguous rename revokes trust before any further publish.
4. Recovery performs an exact observation and re-arms before trust is restored.
5. Healthy events coalesce by normalized path and rebuild only the complete
   changed-path/dependency closure already supported by `OverlayGeneration`.
6. A request pins one immutable generation. There is no response-time retry and
   no pre/post exact identity fence on the healthy warm route.
7. Off mode and exact fallback retain their current response semantics.
8. The first request after process restart may use exact fallback, but must stay
   below the existing 100 ms rebuild budget. Healthy warm `code_*` p95 is below
   10 ms in the release matrix; one-file event-to-publish p95 is reported
   against a 100 ms freshness SLO.

## Non-negotiable protocol for every task

1. Read this plan, the approved design source, the listed files, and all
   dependency-task commits.
2. Reload the relevant design solve and persist a task-specific SOLVE PRE.
   Encode the invariant against the pre-change implementation; capture an
   expected counterexample when the missing capability is the RED condition.
3. Write the smallest behavioral test first and run it with
   `scripts/spur-cargo`. The test must fail for the intended missing behavior.
4. Commit RED separately using `test(spur-graph): <task-id> ...`.
5. Implement only the task scope. Do not weaken exact fallback or mutate agent
   editing behavior. Never require a mutator registration API.
6. Run focused tests, `scripts/spur-cargo test -p spur-graph --lib`, relevant
   integration tests, and `scripts/spur-cargo fmt -- --check`.
7. Persist SOLVE POST with implemented facts, reload it by ID, and show the PRE
   counterexample is eliminated without weakening the mandatory seams.
8. Commit GREEN only after verification. Report RED/GREEN commit IDs, commands,
   timings, solve IDs/statuses, and `git status --porcelain`.

## Dependency DAG

```text
task-1-watch-provider
  -> task-2-generation-actor
    -> task-3-mcp-fast-path
      -> task-4-release-matrix
```

The tasks are serial because each consumes the prior task's committed API.

---

### Task 1: Implement the complete change-provider seam

**Files:**

- Create: `crates/spur-graph/src/overlay_watch.rs`
- Modify: `crates/spur-graph/src/lib.rs`
- Modify: `crates/spur-graph/Cargo.toml`
- Modify: `Cargo.lock`

**SOLVE PRE:** Reload `sol_1a31eed6a32346a6` and
`sol_7c0e05d984244a58`. Encode source-set completeness, provider ordering,
loss revocation, exact recovery, and arm/scan/replay. Record the current
counterexample: no provider seam can observe and replay all three roots.

**RED tests:**

- resolve normal repositories, linked worktrees, and shared commondirs into a
  deterministic composite source set;
- collapse identical physical roots without losing their semantic roles;
- select Watchman first, then `notify`, then exact-only recovery;
- normalize add/modify/delete/rename and Git metadata events;
- surface Watchman recrawl/fresh-instance and every provider/channel error as a
  trust-loss event, never as an empty successful batch;
- prove a deterministic fake subscription replays events after cursor `c0`.

The RED commit must fail because the provider/source-set API is absent.

**GREEN:** Add a provider-neutral async subscription contract and concrete
Watchman and `notify` adapters. Use the maintained Rust crates selected by the
approved external review (`watchman-client` 0.9.x and `notify` 8.x), with
dependency versions compatible with workspace Rust 1.88. Watch the worktree,
gitdir, and commondir. Watchman is optional at runtime: failure to connect or
subscribe falls through to `notify`; failure of both produces exact-only mode.
Do not change repository Git config and do not require Watchman to be installed.

Expose only bounded semantic types: `ChangeSourceSet`, `CompositeCursor`,
`ChangeBatch`, `ChangeProviderKind`, `TrustLoss`, and a subscription factory.
Keep MCP response logic out of this module.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph overlay_watch -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Persist and reload a model proving complete roots, total provider
fallback ordering, replay availability, and fail-closed trust loss. Include a
negative ablation showing that omitting any source role or treating provider
loss as an empty batch is unsat.

---

### Task 2: Add the single-owner generation actor and atomic publication

**Files:**

- Create: `crates/spur-graph/src/mcp/overlay_runtime.rs`
- Modify: `crates/spur-graph/src/mcp/mod.rs` (module declaration and test-only
  hooks only; request routing remains Task 3)
- Modify: `crates/spur-graph/Cargo.toml` (direct `arc-swap` dependency only if
  Task 1 did not already add it)

**SOLVE PRE:** Reload `sol_7c0e05d984244a58` and
`sol_b7b40a3b912b4f51`. Encode one owner, one atomic publication seam,
immutable request pins, loss revocation before publish, and background exact
rebuild. `McpOverlayGenerationBuilder::rebuild_incremental` currently delegates
to `exact_scan`; the work is background-per-change and does not rebuild from
only the changed paths.
Record the current counterexample: the request cache owns reusable generations
but there is no long-lived trusted publisher or epoch.

**RED tests:** With a deterministic fake provider and fake exact builder, prove:

- `c0 -> arm -> exact scan -> replay -> publish` ordering;
- events arriving during the exact scan are included before generation 0;
- one coalesced changed-path set yields one incremental update;
- a second event during a rebuild schedules another pass and cannot be lost;
- provider loss atomically publishes `Untrusted` before exact recovery;
- a failed or cancelled build never publishes a partial generation;
- simultaneous readers observe one coherent `PublishedState` and pin one
  generation even while the actor publishes the next epoch;
- registry keys isolate worktrees and base-graph identities and actors stop when
  their final handle is dropped.

The RED commit must fail because `OverlayRuntimeRegistry` and
`PublishedState` do not exist.

**GREEN:** Implement one actor per canonical worktree/base identity. Use
`ArcSwap` to publish a single `Arc<PublishedState>` containing provider,
composite cursor, epoch, trust, snapshot identity, and generation. The actor
owns all transitions and calls a narrow exact/incremental builder supplied by
the MCP layer. Coalesce events without a magic TTL; drain the immediately
available batch, preserve events that arrive while building, and iterate.
Recovery must re-arm and perform exact scan/replay before returning to trusted.

No filesystem or Git command may execute in `acquire_published()`; it is an
atomic load plus validation of the registry key. Response retry stays absent.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph overlay_runtime -- --nocapture
scripts/spur-cargo test -p spur-graph request_cache::tests -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Persist/reload a transition-system proof for publication
coherence, monotonic epoch, no publish while untrusted, no lost rebuild event,
and immutable request pins. Re-run the task PRE counterexample and show unsat.

---

### Task 3: Route healthy warm MCP requests through the published generation

**Files:**

- Modify: `crates/spur-graph/src/mcp/mod.rs`
- Modify: `crates/spur-graph/src/mcp/overlay_snapshot.rs` only if an exact
  builder extraction is necessary; preserve its exact fallback algorithm
- Modify: `crates/spur-graph/src/mcp/request_cache.rs` only if the actor needs a
  public compatible-seed boundary; do not reintroduce TTL

**SOLVE PRE:** Reload `sol_1a31eed6a32346a6` and the exact-only Pareto point in
`sol_7b317858384d4315`. Encode request-path observations and publication trust.
Record the existing counterexample: a second warm request reports two exact
validation observations and invokes the authoritative post-query fence.

**RED tests:** Extend the existing `OverlayGenerationMcpFixture` and test hooks
to prove:

- after runtime readiness, sequential and concurrent warm `code_*` requests
  report `validation_observations == 0` and execute no Git observer;
- nested `code_subgraph` operations pin one generation ID;
- add/modify/delete/rename and staged/HEAD changes eventually publish a new
  generation equal to a freshly rebuilt exact oracle;
- a shared commondir update invalidates every affected linked worktree;
- an untrusted, lost, or unavailable provider takes the existing exact fallback
  route and never serves a state advertised as trusted;
- Off mode preserves byte-for-byte response behavior;
- no response retry or post-query exact fence remains on the healthy route.

The RED commit must fail on the current `validation_observations == 2` behavior.

**GREEN:** Add the runtime registry to `GraphMcpDeps`. In Auto mode, acquire one
`Arc<PublishedState>` and pin its `Arc<OverlayGeneration>` before calling the
handler. A healthy published state bypasses both
`prepare_overlay_for_worktree` and `authoritative_overlay_identity`. Cold,
warming, exact-only, or untrusted states use the current exact overlay path and
seed/recover the actor asynchronously. Do not block a healthy request on the
actor and do not promise that the source stayed unchanged during the query.

Update bounded diagnostics with provider, epoch, trust, generation ID,
`validation_observations`, and fallback reason. Preserve exact metadata and
fresh-oracle equality.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph generation_route -- --nocapture
scripts/spur-cargo test -p spur-graph overlay_runtime -- --nocapture
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Persist/reload a routing proof: trusted warm implies zero exact
request observations and one pinned generation; every other state implies exact
fallback. Show Off mode unchanged and response retry false.

---

### Task 4: Enforce the correctness, freshness, and end-to-end release matrix

**Files:**

- Modify: `crates/spur-graph/benches/overlay.rs`
- Modify: `crates/spur-graph/tests/perf_gates.rs`
- Modify: `docs/superpowers/plans/2026-08-26-code-overlay-watcher-generation.md`

**SOLVE PRE:** Reload the architecture/routing evidence and Task 4a PRE/POST,
then persist a task-specific model of the legacy/missing matrix contract. The
Task 4b PRE is `sol_de5179e9a9ce4d21` (reloaded `unsat`): the legacy contract has
three project classes, three repetitions, no linked-worktree cell, and is
missing the required scenario/evidence coverage.

**RED:** Add a machine-readable validator for a 30-run matrix over:

- small untracked-heavy repository;
- medium dirty Rust repository;
- large mostly-clean polyglot repository;
- one linked-worktree/shared-commondir case.

Measure exact fallback, cold initialization/restart, healthy warm unchanged,
one-file event-to-publication through a background exact rebuild, provider loss,
overflow, trusted recovery, Off mode, and the full MCP `code_*` response. Record
exact and background observation counts, query operations, finalization stages,
correctness digest/oracle match, provider, epoch/generation identity, immutable
generation pins, and event-to-publication latency. The validator must reject the
legacy three-project/three-run contract and every record missing behavioral
evidence.

**GREEN/evidence:** Run the same command and fixtures after Task 3. Require:

- zero oracle mismatches across all repetitions;
- healthy warm requests have zero exact observations and zero overlay
  finalization stages;
- one immutable generation ID per request;
- warm full `code_*` p95 below 10 ms for every project class;
- cold/restart full request p95 below 100 ms;
- report one-file event-to-publish p50/p95 against the 100 ms freshness SLO;
- provider loss always routes exact until a trusted recovery publish;
- provider overflow always routes exact until a trusted recovery publish;
- Off and exact fallback remain exercised.

Do not hide a missed freshness SLO. If correctness passes but a percentile
misses, preserve the raw evidence and signal the measured blocker rather than
tuning an arbitrary debounce constant.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph --features perf-gates --test perf_gates -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo check -p spur-graph --locked
scripts/spur-cargo fmt -p spur-graph -- --check
```

**SOLVE POST:** Persist/reload the actual matrix as a resource/data-integrity
model. Correctness and routing are hard constraints; latency percentiles remain
measured release constraints. Record which provider ran in each environment and
do not claim a hard watcher freshness theorem.

## Task 4b measured release evidence (2026-08-27)

The RED validator commit is `188cecb65b98392103b4a0b89ce732eee9df0ecb`.
It rejects the legacy three-project/three-run record and requires every v2
behavioral field. The GREEN implementation commit is
`d85484935213359730d702e1b3d733edfe4ff7ec`; it drives the real
`OverlayRuntimeLifecycle` actor and `GraphMcpModule` route through the approved,
default-off Task 4a support seam. No production code or threshold changed.

Measurement command (local macOS aarch64, optimized bench profile):

```bash
SPUR_REMOTE=0 SPUR_GRAPH_TASK4B_MATRIX=1 SPUR_GRAPH_RELEASE_REPEATS=30 \
  SPUR_GRAPH_TASK4B_EVIDENCE=/tmp/spur-task4b-overlay-release-matrix.json \
  scripts/spur-cargo bench -p spur-graph --features perf-gates --bench overlay -- --noplot
```

The method is nearest-rank ceiling: sort ascending, compute one-based rank
`ceil(p * N)`, and select `rank - 1`. With 30 observations, p50 is the 15th and
p95 the 29th sorted observation. All times below are milliseconds.

| Project class | Scenario | p50 | p95 |
|---|---|---:|---:|
| small untracked-heavy | exact fallback | 46.615792 | 48.354167 |
| small untracked-heavy | cold/restart | 37.368958 | 42.670625 |
| small untracked-heavy | healthy warm full MCP | 0.143500 | 0.153041 |
| small untracked-heavy | one-file event-to-publication | 83.635042 | 91.733125 |
| small untracked-heavy | provider loss | 46.868083 | 49.720708 |
| small untracked-heavy | recovery after loss | 39.102417 | 45.460250 |
| small untracked-heavy | provider overflow | 45.876833 | 47.994750 |
| small untracked-heavy | recovery after overflow | 38.734625 | 41.099625 |
| small untracked-heavy | Off mode | 51.162000 | 54.910667 |
| medium dirty Rust | exact fallback | 48.438583 | 56.501417 |
| medium dirty Rust | cold/restart | 37.659875 | 38.835875 |
| medium dirty Rust | healthy warm full MCP | 0.151417 | 0.204000 |
| medium dirty Rust | one-file event-to-publication | 82.395834 | 85.262292 |
| medium dirty Rust | provider loss | 46.309333 | 50.318875 |
| medium dirty Rust | recovery after loss | 38.575292 | 39.848125 |
| medium dirty Rust | provider overflow | 48.753708 | 54.424042 |
| medium dirty Rust | recovery after overflow | 40.531208 | 43.779709 |
| medium dirty Rust | Off mode | 52.110750 | 57.605625 |
| large mostly-clean polyglot | exact fallback | 47.636750 | 57.519292 |
| large mostly-clean polyglot | cold/restart | 37.994750 | 40.317875 |
| large mostly-clean polyglot | healthy warm full MCP | 0.166500 | 0.180084 |
| large mostly-clean polyglot | one-file event-to-publication | 97.407125 | **100.067209** |
| large mostly-clean polyglot | provider loss | 47.448333 | 53.423334 |
| large mostly-clean polyglot | recovery after loss | 38.943542 | 41.981958 |
| large mostly-clean polyglot | provider overflow | 48.072834 | 50.698792 |
| large mostly-clean polyglot | recovery after overflow | 39.615125 | 54.624750 |
| large mostly-clean polyglot | Off mode | 54.040833 | 58.132333 |
| linked worktree/shared commondir | exact fallback | 47.971958 | 52.522583 |
| linked worktree/shared commondir | cold/restart | 38.452834 | 40.392209 |
| linked worktree/shared commondir | healthy warm full MCP | 0.148916 | 0.162625 |
| linked worktree/shared commondir | one-file event-to-publication | 84.601792 | 89.286291 |
| linked worktree/shared commondir | provider loss | 47.465959 | 70.308625 |
| linked worktree/shared commondir | recovery after loss | 39.938167 | 62.545083 |
| linked worktree/shared commondir | provider overflow | 47.693583 | 50.158291 |
| linked worktree/shared commondir | recovery after overflow | 40.019458 | 44.459125 |
| linked worktree/shared commondir | Off mode | 53.099375 | 56.435208 |

Every scenario has 30 observations per project class. The following behavioral
counts are per project class; multiply by four for matrix totals. Off mode has no
overlay finalization diagnostic by design, so the evidence records `null` with
source `not_exposed_in_off_mode`, never a fabricated zero.

| Scenario | Exact | Background exact rebuild | Queries | Legacy finalizations | Immutable pins | Provider/route/trust |
|---|---:|---:|---:|---:|---:|---|
| exact fallback | 30 | 0 | 30 | 120 | 0 | unavailable / exact fallback / unavailable |
| cold/restart | 0 | 30 | 30 | 0 | 30 | notify / generation / trusted |
| healthy warm | 0 | 0 | 30 | 0 | 30 | notify / generation / trusted |
| one-file event-to-publication | 0 | 30 | 30 | 0 | 30 | notify / generation / trusted |
| provider loss | 30 | 0 | 30 | 120 | 0 | notify / exact fallback / untrusted |
| recovery after loss | 0 | 30 | 30 | 0 | 30 | notify / generation / trusted |
| provider overflow | 30 | 0 | 30 | 120 | 0 | notify / exact fallback / untrusted |
| recovery after overflow | 0 | 30 | 30 | 0 | 30 | notify / generation / trusted |
| Off mode | 30 | 0 | 30 | not exposed | 0 | unavailable / off / off |

Matrix totals are 1,080 full MCP requests, 480 request-time exact observations,
480 background exact rebuild observations, 1,080 query operations, 1,440
observed legacy finalization operations on exact routes, and 600 immutable
generation pins. All 1,080 response digests match their exact oracle. Every
healthy warm request has exactly one immutable generation pin, zero exact Git
observations, zero background rebuild observations, and zero legacy
finalization. Each raw row records provider, trust, epoch, generation ID, and
pin identity. Provider loss and overflow route exact while untrusted; both
recovery scenarios publish a trusted notify generation after one background
exact rebuild per run.

The task-specific measured POST is `sol_95b0183b86e14156` (persisted and
reloaded `unsat`). It uses only the recorded matrix values. The failed release
constraint is the large mostly-clean polyglot event-to-publication p95:
100.067209 ms is not below 100 ms. The 29th sorted sample is 100.067209 ms and
the 30th is 107.154333 ms; neither was removed. The machine-readable verdict is
therefore **DO NOT RELEASE** with hard failure
`large_mostly_clean_polyglot:event_to_publication_p95`.

Verification results:

- emitted v2 evidence validation (local, evidence path set): 1 passed;
- `scripts/spur-cargo test -p spur-graph --features perf-gates --test
  perf_gates -- --nocapture`: 12 passed, 1 ignored, and the single permitted
  pre-existing baseline failure remained exactly
  `git_path_as_ref_inbound=45` against the untouched threshold 20;
- `scripts/spur-cargo test -p spur-graph --lib`: 513 passed, 3 ignored;
- `scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture`:
  12 passed;
- `scripts/spur-cargo check -p spur-graph --locked`: passed;
- `scripts/spur-cargo fmt -p spur-graph -- --check`: passed.

The full raw JSON is preserved losslessly below as deterministic gzip (`gzip
-n`) plus base64. Its uncompressed SHA-256 is
`1c5ee001c929b4099f2405c022911b0b6f54cb996273e319bf4de77d1de877b9`.
Decode the text between the markers with `base64 --decode | gzip -dc`.

<!-- TASK4B_RAW_MATRIX_BASE64_BEGIN -->
```text
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
```
<!-- TASK4B_RAW_MATRIX_BASE64_END -->

## Final review gate

Before merge, independently inspect every RED/GREEN pair, reload every PRE/POST
solve, and verify the cumulative diff against this plan. The merge decision is
`APPROVE` only if the warm path contains no exact Git observation, loss paths
fail closed, the generation is pinned for each request, the same-shape matrix
has zero mismatches, and the worker branches are clean.
