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
                              incremental generation update
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
immutable request pins, loss revocation before publish, and incremental rebuild.
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

**SOLVE PRE:** Reload `sol_7b317858384d4315` and
`sol_b7b40a3b912b4f51`. Encode the observed pre-change p50/p95 values, two exact
observations, zero mismatch requirement, 100 ms cold budget, 10 ms warm target,
and 100 ms event-to-publish freshness SLO. Persist the failing pre-change model.

**RED:** Add a machine-readable validator for a 30-run matrix over:

- small untracked-heavy repository;
- medium dirty Rust repository;
- large mostly-clean polyglot repository;
- one linked-worktree/shared-commondir case.

Measure exact fallback, cold initialization, healthy warm unchanged, one-file
incremental publish, provider-loss recovery, and full MCP response. Record
observer count, query operations, finalization stages, correctness digest,
provider, epoch, and event-to-publish latency. The validator must fail the PRE
record because warm requests perform two exact observations and exceed 10 ms.

**GREEN/evidence:** Run the same command and fixtures after Task 3. Require:

- zero oracle mismatches across all repetitions;
- healthy warm requests have zero exact observations and zero overlay
  finalization stages;
- one immutable generation ID per request;
- warm full `code_*` p95 below 10 ms for every project class;
- cold/restart full request p95 below 100 ms;
- report one-file event-to-publish p50/p95 against the 100 ms freshness SLO;
- provider loss always routes exact until a trusted recovery publish;
- Off and exact fallback remain exercised.

Do not hide a missed freshness SLO. If correctness passes but a percentile
misses, preserve the raw evidence and signal the measured blocker rather than
tuning an arbitrary debounce constant.

**Verification:**

```bash
scripts/spur-cargo test -p spur-graph --features perf-gates --test perf_gates -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo test -p spur-graph --test overlay_client -- --nocapture
scripts/spur-cargo check -p spur-graph
scripts/spur-cargo fmt -- --check
```

**SOLVE POST:** Persist/reload the actual matrix as a resource/data-integrity
model. Correctness and routing are hard constraints; latency percentiles remain
measured release constraints. Record which provider ran in each environment and
do not claim a hard watcher freshness theorem.

## Final review gate

Before merge, independently inspect every RED/GREEN pair, reload every PRE/POST
solve, and verify the cumulative diff against this plan. The merge decision is
`APPROVE` only if the warm path contains no exact Git observation, loss paths
fail closed, the generation is pinned for each request, the same-shape matrix
has zero mismatches, and the worker branches are clean.
