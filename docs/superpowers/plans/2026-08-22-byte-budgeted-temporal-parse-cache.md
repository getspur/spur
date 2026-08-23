# Byte-Budgeted Temporal Parse Cache Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `beads://bd-mehk` (user-approved 2026-08-22), grounded by
`docs/superpowers/plans/2026-08-22-temporal-graph-build-telemetry.md`
**Formal @spec cells (if notebook):** none
**Design epic:** `bd-mehk` (source feature; intentionally remains open until the
implementation plan is integrated)

**Goal:** Replace immediate post-reduction parse-cache removal with a
deterministic byte-budgeted retained tier, enable only a Turso-measured default,
and quantify the resulting bottleneck with a fresh xctrace/quack-flamegraph
profile.

**Architecture:** `SharedParseCache` keeps entries required by unreduced
ordinals in a protected tier. Successfully initialized entries that become
eligible after reduction move into an LRU-by-temporal-ordinal retained tier;
that tier alone is bounded by estimated payload bytes and uses a stable blob
OID/language-rank tie-break. A validated environment override permits a single
binary to sweep candidate budgets, while the compiled default remains zero
until measured evidence selects a nonzero value.

**Tech Stack:** Rust 2021, `std::sync::{Arc, Mutex}`, tree-sitter extraction,
SPUR temporal graph artifacts, repository solver tools/Z3, `/usr/bin/time`,
Xcode Time Profiler (`xctrace`), FlameGraph folded stacks, DuckDB 1.5.x, and the
local `quack_flamegraph.duckdb_extension`.

---

## Measured basis and fixed decision boundary

The integrated cold Turso profile at SPUR
`55a2d7c05f591328397a1192861358a6b0b54aba` measured:

- 91,411 post-eviction reparses, 36.8743% of cache lookups;
- 1,107.005 seconds of reparse initialization, 49.4837% of summed
  active-worker time;
- 41.3592 MiB peak initialized payload estimate under immediate eviction;
- 3,442,720,768-byte peak RSS with telemetry enabled;
- 91.4697% exact-frame coverage in `cached_extract`, with extraction/query/
  parse still dominating the on-CPU profile.

The earlier ordered solver decision selected **cache only**
(`sol_b6105023a24b4db6`) and proved non-cache decisions unsatisfiable
(`sol_fb676902ec124415`). This plan must not change the scheduler, admission
window, reorder capacity, query walker, parent traversal, token collection, or
fact finalization.

Planning-time capacity checks established the sweep envelope:

- `sol_93528ada5888433a`: catalog rule
  `resource.aggregate_capacity` passed for the telemetry baseline plus a
  256 MiB retained-cache budget plus a separate 256 MiB untracked-overhead
  guard under the 4 GiB cap;
- `sol_433bcbdb0d6b4821`: generic finite-domain optimization selected 256 MiB as
  the largest member of `{0, 64, 128, 256} MiB` that satisfies that guarded
  envelope.

These solves authorize measurement of 256 MiB; they do not authorize enabling
it. Only Task 3's measured selection rule can change the default.

## File map

| File | Responsibility | Owning tasks |
|---|---|---|
| `crates/spur-graph/src/git_walk.rs` | Budget parsing, protected/retained cache state, deterministic eviction, byte accounting, telemetry, unit tests, measured default | Task 1; Task 3 only for the final constant |
| `crates/spur-graph/tests/temporal_parallel.rs` | Byte-identical jobs 1/2/4/8 artifact verification | Task 2 |
| `docs/superpowers/plans/2026-08-22-byte-budgeted-temporal-parse-cache.md` | Commands, provenance, sweep decision, final profile, and completion audit | Tasks 3 and 4 sequentially |

No task may modify `crates/spur-graph/src/extract/`, the worker-pool/reducer
admission policy, CLI flag definitions, `GitWalkConfig`, or benchmark source.
The hidden byte override stays local to `git_walk.rs`, avoiding a four-file
public-config refactor solely for experimentation.

## Dependency DAG

```text
task-1-cache-policy
        |
        v
task-2-determinism-matrix
        |
        v
task-3-turso-budget-selection
        |
        v
task-4-final-profile
```

The chain is intentional. Budget runs must not execute concurrently because
host contention would invalidate comparisons. Task 3 may edit the compiled
default only after Task 2 is approved, and Task 4 profiles exactly Task 3's
selected default.

---

### Task 1: Implement the deterministic byte-budgeted retained tier

**Task ID:** `task-1-cache-policy`

**Files:**

- Modify: `crates/spur-graph/src/git_walk.rs:1-17`
- Modify: `crates/spur-graph/src/git_walk.rs:938-960`
- Modify: `crates/spur-graph/src/git_walk.rs:1559-2050`
- Test: `crates/spur-graph/src/git_walk.rs:4393-4757`

**Depends on:** none

**Acceptance Criteria:**

- [ ] The compiled default is initially zero bytes, reproducing immediate
      eviction, and `SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=DECIMAL_U64`
      overrides it
      once per full walk; malformed/non-Unicode/overflowing values fail with a
      contextual error rather than silently falling back.
- [ ] Entries with `greatest_using_ordinal == None` or an ordinal greater than
      or equal to `next_unreduced_ordinal` are protected and never count against
      or yield to the retained-tier budget.
- [ ] Only successfully initialized eligible entries enter the retained tier;
      failed and incomplete entries are removed or left unretained.
- [ ] Retained bytes never exceed the configured budget after
      `evict_before`; a single oversized entry is evicted.
- [ ] Eviction is deterministic: oldest `greatest_using_ordinal` first, then
      blob OID, then an explicit stable language rank. HashMap iteration order
      is never used as policy.
- [ ] An eligible retained hit promotes the entry back to the protected/active
      state without reparsing and updates byte accounting exactly once.
- [ ] Payload estimation and accounting run with telemetry disabled as well as
      enabled, using saturating/checked arithmetic; telemetry changes
      observability only, never retention behavior.
- [ ] The global cache-state mutex is not held while parsing or while waiting
      for an entry-initialization mutex; same-key initialization remains
      single-flight.
- [ ] Debug telemetry includes configured budget, retained-tier hits, budget
      evictions, current/peak retained-tier bytes, and existing cache counters.
- [ ] Focused tests, all `spur-graph` library tests, check, and formatting pass
      through `scripts/spur-cargo`.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`. The user
selected Codex; this is a single-file, tightly scoped state-machine change.

**Scope Boundary:**

- IN scope: `SharedParseCacheState`, `SharedParseCacheEntry`,
  `SharedParseCache`, `retained_payload_estimate`, budget parsing, cache
  telemetry structs/log fields, and colocated unit tests.
- OUT of scope: `GitWalkConfig`, `crates/spur-cli`, worker scheduling,
  tree-sitter extraction/query functions, and any unrelated cleanup.
- If another file is required, the retained-tier invariant cannot be expressed
  without waiting on an entry lock, or the diff is likely to exceed the
  single-file boundary by more than 50%, emit `scope_drift` or `risk` before
  editing further.

**Implementation:**

- [ ] **Step 1: Reload and record the planning solves before editing.**

```text
get_solve_result("sol_93528ada5888433a")
get_solve_result("sol_433bcbdb0d6b4821")
```

Record their raw `sat`/`pass` meanings in the beads intent comment. Do not
interpret the 256 MiB feasible envelope as a performance win.

- [ ] **Step 2: Add RED tests for the cache contract before implementation.**

Add tests with these exact behavioral names and assertions:

```rust
#[test]
fn retained_tier_reuses_an_eligible_entry_without_reinitializing() {
    let cache = SharedParseCache::with_budget_and_telemetry(1 << 20);
    let key = (Language::Rust, "retained".to_owned());
    let first = cache
        .get_or_init_at(1, key.clone(), || {
            Ok(Ok(vec![cache_test_symbol("retained")]))
        })
        .unwrap()
        .unwrap();
    cache.evict_before(2);
    let second = cache
        .get_or_init_at(8, key, || panic!("retained hit must not reparse"))
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(cache.telemetry_snapshot().retained_tier_hits, 1);
}

#[test]
fn retained_tier_evicts_oversized_entries_but_keeps_unreduced_entries() {
    let symbol = cache_test_symbol("oversized");
    let budget = retained_payload_estimate(std::slice::from_ref(&symbol)) - 1;
    let cache = SharedParseCache::with_budget(budget);
    let old_key = (Language::Rust, "old".to_owned());
    let live_key = (Language::Rust, "live".to_owned());
    cache.get_or_init_at(1, old_key.clone(), || Ok(Ok(vec![symbol]))).unwrap().unwrap();
    cache.get_or_init_at(9, live_key.clone(), || Ok(Ok(vec![cache_test_symbol("live")]))).unwrap().unwrap();
    cache.evict_before(2);
    assert!(!cache.contains(&old_key));
    assert!(cache.contains(&live_key));
}
```

Also add tests named:

```text
retained_tier_ties_are_independent_of_hashmap_and_insertion_order
retained_tier_tracks_exact_bytes_with_telemetry_off_and_on
retained_tier_failed_initialization_releases_every_accounted_byte
retained_tier_same_key_initialization_remains_single_flight
retained_tier_initializer_can_reenter_cache_state_without_deadlock
parse_cache_budget_accepts_zero_and_u64_values_and_rejects_invalid_input
```

The tie test must insert equal-sized Rust/Python entries in at least two
different orders and assert the same survivors. The byte-parity test must use
`retained_payload_estimate` as the expected value, not a duplicated hand-coded
size formula. Extend the existing protected-entry, failure, and concurrency
tests rather than creating a second competing harness.

- [ ] **Step 3: Run the focused tests and capture the expected RED result.**

```bash
scripts/spur-cargo test -p spur-graph retained_tier -- --nocapture
```

Expected: compilation or assertion failure because the retained-tier
constructors/fields/behavior do not yet exist. Attach the exact failing output
to the task audit before implementation.

- [ ] **Step 4: Implement the minimal state and configuration interface.**

Use these names so downstream tasks have a stable contract:

```rust
const TEMPORAL_PARSE_CACHE_BUDGET_ENV: &str =
    "SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES";
const DEFAULT_TEMPORAL_PARSE_CACHE_BUDGET_BYTES: u64 = 0;

fn configured_temporal_parse_cache_budget_bytes() -> Result<u64>;

struct SharedParseCacheEntry {
    initialization: Arc<Mutex<ParseCacheInitializationState>>,
    greatest_using_ordinal: Option<usize>,
    retained_payload_bytes: u64,
    initialized: bool,
    retained_since_ordinal: Option<usize>,
}

struct SharedParseCacheState {
    entries: HashMap<ParseCacheKey, SharedParseCacheEntry>,
    exact_ghost_ordinals: Option<HashMap<ParseCacheKey, usize>>,
    current_payload_bytes: u64,
    retained_tier_payload_bytes: u64,
}

impl SharedParseCache {
    fn new(telemetry_enabled: bool, retained_budget_bytes: u64) -> Self;
    #[cfg(test)]
    fn with_budget(retained_budget_bytes: u64) -> Self;
    #[cfg(test)]
    fn with_budget_and_telemetry(retained_budget_bytes: u64) -> Self;
}
```

`run_full_walk_into_with_stats` must resolve the environment/default once,
then construct the shared cache with that immutable budget. Keep parsing
outside the state lock. On successful initialization, compute
`retained_payload_estimate` regardless of telemetry, set `initialized`, and
update total bytes while holding only the state lock needed for registration.

At `evict_before`, mark newly eligible initialized entries, build a candidate
list, sort it by `(greatest_using_ordinal, blob_oid, language_rank)`, and remove
oldest candidates until `retained_tier_payload_bytes <= retained_budget_bytes`.
Never lock an entry initialization mutex from this eviction path. Budget zero
must therefore retain no eligible entry.

When `get_or_init_with_ordinal` finds an entry with
`retained_since_ordinal.is_some()`, clear that marker and subtract its bytes
from the retained-tier total before treating it as an active hit. Preserve the
existing exact-ghost classification for entries actually evicted by budget
pressure.

- [ ] **Step 5: Re-run a post-change arithmetic counterexample solve.**

Use `solve_constraints` with bounded nonnegative `current`, `candidate`, and
`budget` values up to `2^40`. Encode the admission premise
`current <= budget && candidate <= budget - current`, assert the
counterexample `current + candidate > budget`, and persist the expected
`unsat` result. Runtime tests remain authoritative for state transitions; the
solve proves the overflow-safe admission arithmetic over the encoded domain.

- [ ] **Step 6: Run GREEN verification and format.**

```bash
scripts/spur-cargo test -p spur-graph retained_tier -- --nocapture
scripts/spur-cargo test -p spur-graph shared_parse_cache -- --nocapture
scripts/spur-cargo test -p spur-graph --lib
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
```

Expected: all commands exit zero. Existing unrelated warnings must be recorded
but not “fixed while here.”

- [ ] **Step 7: Commit exactly once.**

```bash
git add crates/spur-graph/src/git_walk.rs
git commit -m "feat(spur-graph): task-1 add byte-budgeted temporal parse retention"
```

The completion audit must include the RED output, GREEN commands, solve ID,
changed constants, and confirmation that only `git_walk.rs` changed.

---

### Task 2: Expand temporal artifact determinism to jobs 1/2/4/8

**Task ID:** `task-2-determinism-matrix`

**Files:**

- Modify: `crates/spur-graph/tests/temporal_parallel.rs:28-55`

**Depends on:** `task-1-cache-policy`

**Acceptance Criteria:**

- [ ] The integration test constructs fresh clones at jobs 1, 2, 4, and 8 and
      compares graph, commit index, reloaded artifact, shard index, manifest,
      every artifact file, and normalized semantic bytes against jobs=1.
- [ ] A second jobs=8 run verifies repeat determinism.
- [ ] The same test passes once with budget zero and once with the maximum
      candidate budget 268,435,456 bytes.
- [ ] No production file changes in this task.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: the top-level determinism test loop in
  `crates/spur-graph/tests/temporal_parallel.rs`.
- OUT of scope: fixture semantics, artifact normalization, `git_walk.rs`, and
  production configuration.
- Emit `scope_drift` if the approved Task 1 interface requires a production
  edit or if artifact differences reveal a semantic change rather than a test
  expectation problem.

**Implementation:**

- [ ] **Step 1: Replace the 1-vs-three-8s matrix with the explicit job set.**

```rust
let serial = construct_from_fresh_clone(source.path(), 1)?;
for jobs in [2, 4, 8] {
    let parallel = construct_from_fresh_clone(source.path(), jobs)?;
    assert_walk_output_eq(
        &format!("jobs=1 vs jobs={jobs}"),
        &serial,
        &parallel,
    );
}
let repeat_a = construct_from_fresh_clone(source.path(), 8)?;
let repeat_b = construct_from_fresh_clone(source.path(), 8)?;
assert_walk_output_eq("jobs=8 repeat", &repeat_a, &repeat_b);
```

- [ ] **Step 2: Run the zero-budget and max-budget matrices.**

```bash
env -u SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES \
  scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=268435456 \
  scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
scripts/spur-cargo fmt --all -- --check
```

Both runs must exit zero. Do not normalize away an actual byte difference.

- [ ] **Step 3: Commit exactly once.**

```bash
git add crates/spur-graph/tests/temporal_parallel.rs
git commit -m "test(spur-graph): task-2 cover temporal jobs 1 2 4 and 8"
```

---

### Task 3: Sweep Turso budgets and conditionally enable the measured default

**Task ID:** `task-3-turso-budget-selection`

**Files:**

- Modify: `docs/superpowers/plans/2026-08-22-byte-budgeted-temporal-parse-cache.md`
- Conditionally modify only the constant at
  `crates/spur-graph/src/git_walk.rs:DEFAULT_TEMPORAL_PARSE_CACHE_BUDGET_BYTES`

**Depends on:** `task-2-determinism-matrix`

**Acceptance Criteria:**

- [ ] A profiling binary is built once from the approved Task 2 base; source,
      tree, binary, tool, extension, and artifact hashes are recorded.
- [ ] Every run uses Turso source
      `a45cd87ff7b25a30476491037a028c43ff95d6f5`, a distinct fresh
      `git clone --local --no-hardlinks`, Full mode, jobs=8, identical graph
      flags, telemetry enabled, and a timeout of at least 1,500 seconds.
- [ ] Budgets 0, 64 MiB, 128 MiB, and 256 MiB are each measured. Zero and the
      provisional winner are then repeated from fresh clones using the same
      binary; runs are sequential, never concurrent.
- [ ] Each row reports real/user/system, CPU work, peak RSS, commits/s, cache
      hits, retained-tier hits, cold/reparse initializations, reparse active-
      worker share, budget evictions, current/peak retained-tier bytes, average
      active workers, and identical workload/artifact summaries.
- [ ] A persisted solver decision applies the rule below. A nonzero compiled
      default is enabled only if it passes; otherwise the compiled default
      remains zero.
- [ ] The chosen configuration remains below 4 GiB RSS in every observation,
      reduces reparses in both comparisons, improves paired-median wall or CPU
      work by at least 3%, and does not regress the other by more than 3%.
- [ ] Focused/unit/integration verification passes after the conditional
      constant edit.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`. Although
this task may touch two files, the source edit is exactly one already-tested
constant; it is not a multi-file refactor.

**Scope Boundary:**

- IN scope: cold measurement artifacts under one fresh `/tmp` directory,
  appended Task 3 results in this document, and the one default-budget
  constant if the solver selects a nonzero value.
- OUT of scope: cache algorithm changes, scheduler changes, CLI flags,
  extraction/query changes, and editing raw benchmark results after capture.
- Emit `risk` before selecting a default if any run is censored/non-Full, source
  or workload hashes diverge, telemetry appears non-representative, RSS reaches
  4 GiB, or repeats disagree in sign. Emit `scope_drift` before any source edit
  other than the named constant.

**Implementation:**

- [ ] **Step 1: Verify the approved base and create a unique artifact root.**

```bash
git status --short --untracked-files=no
git rev-parse HEAD^{commit} HEAD^{tree}
CACHE_RUN_ROOT="$(mktemp -d /tmp/spur-turso-cache.XXXXXX)"
git -C /Volumes/Projects/Projects/turso rev-parse HEAD^{commit} HEAD^{tree}
```

Abort if the Turso commit differs from the required SHA. Preserve
`CACHE_RUN_ROOT` and record it in the report.

- [ ] **Step 2: Build and freeze one profiling binary.**

Use the repository-supported build path; never invoke bare `cargo`:

```bash
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 \
  scripts/spur-cargo zigbuild -p spur-cli --profile profiling -j 8
SPUR_CLOUD=aws-my SPUR_REMOTE_NAMESPACE=spur \
  /Volumes/Projects/Projects/spur-notebook/scripts/cloud-build/fetch.sh \
  --via-s3 --to "$CACHE_RUN_ROOT/spur-cache-sweep" \
  target/aarch64-apple-darwin/profiling/spur
shasum -a 256 "$CACHE_RUN_ROOT/spur-cache-sweep"
```

If the configured remote differs, use the equivalent successful
`scripts/spur-cargo` route and record it exactly. Do not reuse the pre-change
binary.

- [ ] **Step 3: Run the preliminary sweep sequentially.**

Run the four byte values with explicit stable labels:

```bash
while IFS=: read -r label bytes; do
  mkdir -p "$CACHE_RUN_ROOT/$label"
  git clone --quiet --local --no-hardlinks \
    /Volumes/Projects/Projects/turso "$CACHE_RUN_ROOT/$label/repo"
  /usr/bin/time -l -o "$CACHE_RUN_ROOT/$label/time.txt" \
    /opt/homebrew/bin/timeout --signal=TERM --kill-after=10s 1500s \
    /usr/bin/env -u SPUR_GRAPH_TEMPORAL_JOBS \
    SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES="$bytes" \
    SPUR_EMBEDDING_MODEL=jina-code RUST_LOG=spur_graph::git_walk=debug \
    "$CACHE_RUN_ROOT/spur-cache-sweep" graph build \
    --root "$CACHE_RUN_ROOT/$label/repo" \
    --output "$CACHE_RUN_ROOT/$label/graph" \
    --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
    --no-analyst --temporal-jobs 8 \
    >"$CACHE_RUN_ROOT/$label/stdout.txt" \
    2>"$CACHE_RUN_ROOT/$label/stderr.txt"
done <<'BUDGETS'
zero:0
mib64:67108864
mib128:134217728
mib256:268435456
BUDGETS
```

Save this expanded label/byte mapping beside the outputs. Verify before each
run that the clone has no `.spur` or `.git/spur-graph` state.

- [ ] **Step 4: Select a provisional winner and repeat it against zero.**

Filter out any candidate with RSS at or above 4,294,967,296 bytes, any artifact
or workload mismatch, or no reparse reduction. Repeat zero and the best
remaining nonzero candidate from two more fresh clones. Compute paired medians
from the two zero observations and the two candidate observations.

Encode candidates as a finite solver domain. A nonzero candidate is eligible
only when both observations reduce reparses, median wall or CPU improves by at
least 3%, the other metric regresses by no more than 3%, and all RSS values are
below 4 GiB. Optimize first for the larger qualifying CPU/wall improvement and
then for the smaller budget within one percentage point of the best result.
Persist the solver result and record raw status, model, constraints, and solve
ID. If no nonzero model exists, select zero; never reinterpret `unknown` or
`timeout` as permission to enable caching.

- [ ] **Step 5: Apply only the measured default and run verification.**

If and only if the solver selected a nonzero byte value, replace the right-hand
side of `DEFAULT_TEMPORAL_PARSE_CACHE_BUDGET_BYTES` with the base-10 integer
returned in the persisted solver model. Do not substitute a human-readable
expression. If selection is zero, leave the source unchanged and record the
no-enable decision in this document.

```bash
scripts/spur-cargo test -p spur-graph retained_tier -- --nocapture
scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=0 \
  scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
```

- [ ] **Step 6: Append complete results and commit exactly once.**

The report must include provenance, exact commands, uncensored status, raw and
derived tables, artifact equivalence, planning and measured solve IDs, selected
default, and a checksum manifest for ordinary files under `CACHE_RUN_ROOT`.

```bash
git add docs/superpowers/plans/2026-08-22-byte-budgeted-temporal-parse-cache.md \
  crates/spur-graph/src/git_walk.rs
git commit -m "perf(spur-graph): task-3 select measured temporal cache budget"
```

If the default remains zero, omit the unchanged Rust file from `git add` while
keeping the same evidence-focused commit intent.

---

### Task 4: Re-profile the selected default through DuckDB/quack-flamegraph

**Task ID:** `task-4-final-profile`

> **User-approved execution amendment (2026-08-22):** Use the mounted
> `/Volumes/Projects/Projects/ade-bench` checkout for a faster feedback loop,
> and compare explicit zero against explicit 64 MiB. The compiled default
> remained zero after Task 3, so profiling an unset default would only repeat
> the control and would not expose the retained-cache path. Reuse the frozen
> profiling binary only after proving that Task 3 changed no Rust or build
> input. This amendment supersedes Task 4's Turso/new-binary/default-run details;
> its Full-mode, provenance, recursion, identity, DuckDB, and no-Rust-change
> gates remain binding. Results are directional for ade-bench and are not a
> controlled CPU-profile delta against Turso.

**Files:**

- Modify: `docs/superpowers/plans/2026-08-22-byte-budgeted-temporal-parse-cache.md`

**Depends on:** `task-3-turso-budget-selection`

**Acceptance Criteria:**

- [ ] A new profiling binary is built once from the approved Task 3 commit and
      hashed; it is not the Task 3 sweep binary if the compiled default changed.
- [ ] A fresh default-budget cold Turso run and a fresh environment-forced zero
      control use identical jobs=8 flags and complete Full below 4 GiB RSS.
- [ ] The default's graph/artifact summary is byte-identical to the zero
      control and to Task 3's expected workload.
- [ ] A rootless Xcode Time Profiler trace of the default completes within the
      1,500-second bound and is converted to deterministic folded and SVG files.
- [ ] DuckDB loads
      `/Volumes/Projects/Projects/quack-flamegraph/build/debug/quack_flamegraph.duckdb_extension`,
      discovers function/table-macro schemas before analysis, and records the
      extension hash and quack-flamegraph commit.
- [ ] Recursive frames use exact-frame unique-stack coverage; occurrence-summed
      inclusive coverage is reported only after proving max one occurrence per
      stack.
- [ ] The report compares before/after extract, query, parse, parent-union,
      token, tree-delete, finalization, cache-hit/reparse, byte/RSS, lock-wait,
      and worker-utilization metrics and names exactly one evidence-backed next
      bottleneck or explicitly states that no further patch is justified.
- [ ] No Rust source changes in this task.

**Suggested Worker:** `codex`, model `gpt-5.6-sol`, effort `xhigh`.

**Scope Boundary:**

- IN scope: fresh `/tmp` traces/folded/SVG/DuckDB artifacts and the final
  results/completion-audit section in this plan.
- OUT of scope: implementing the next bottleneck, changing the selected cache
  budget, changing scheduler/query/token behavior, or editing any Rust file.
- Emit `risk` if the final default no longer reproduces Task 3's direction,
  xctrace is censored, symbolication is unusable, schema discovery fails, or
  recursion makes a requested metric ambiguous.

**Implementation:**

- [ ] **Step 1: Build/hash the final binary and run default vs zero.**

Repeat Task 3's repository-supported profiling build from the approved commit.
Use two fresh `--no-hardlinks` clones. The default run must unset
`SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES`; the control must set it to `0`. Capture
`/usr/bin/time -l`, debug telemetry, stdout, artifact hashes, and exact commands
under a new `mktemp` directory. Use 1,500-second timeouts.

- [ ] **Step 2: Capture and fold a rootless selected-default trace.**

```bash
DEVELOPER_DIR=/Applications/Xcode-15.4.0.app/Contents/Developer \
  xcrun xctrace record --template 'Time Profiler' --time-limit 1500s \
  --output "$FINAL_RUN_ROOT/profile/default.trace" --no-prompt \
  --target-stdout "$FINAL_RUN_ROOT/profile/stdout.txt" \
  --env SPUR_EMBEDDING_MODEL=jina-code \
  --env RUST_LOG=spur_graph::git_walk=debug --launch -- \
  "$FINAL_RUN_ROOT/spur-final" graph build \
  --root "$FINAL_RUN_ROOT/profile/repo" \
  --output "$FINAL_RUN_ROOT/profile/graph" \
  --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
  --no-analyst --temporal-jobs 8

XCTRACE=/Applications/Xcode-15.4.0.app/Contents/Developer/usr/bin/xctrace \
  flamegraph --deterministic \
  --title 'spur graph build — Turso byte-budgeted temporal cache' \
  --post-process "/usr/bin/tee $FINAL_RUN_ROOT/profile/default.folded" \
  --perfdata "$FINAL_RUN_ROOT/profile/for-flamegraph.trace" \
  -o "$FINAL_RUN_ROOT/profile/default.svg"
```

Preserve a ZIP copy of the trace before giving a disposable copy to
`flamegraph`.

- [ ] **Step 3: Discover quack-flamegraph schemas before querying.**

```sql
LOAD '/Volumes/Projects/Projects/quack-flamegraph/build/debug/quack_flamegraph.duckdb_extension';
SELECT function_name, function_type, parameters, parameter_types
FROM duckdb_functions()
WHERE function_name IN (
  'read_folded', 'flamegraph_coverage', 'flamegraph_edges',
  'flamegraph_exclusive', 'flamegraph_hot_stacks', 'flamegraph_inclusive'
)
ORDER BY function_name;
DESCRIBE SELECT * FROM read_folded('$FINAL_RUN_ROOT/profile/default.folded');
DESCRIBE SELECT * FROM flamegraph_edges('$FINAL_RUN_ROOT/profile/default.folded');
DESCRIBE SELECT * FROM flamegraph_inclusive('$FINAL_RUN_ROOT/profile/default.folded');
```

Materialize the folded rows only after these queries succeed.

- [ ] **Step 4: Run recursion-safe coverage and edge analyses.**

For every target, first compute maximum occurrences per stack. Use
`SUM(samples)` over `list_contains(frames, 'FRAME_NAME')` when the maximum is
greater than one. In particular:

```sql
SELECT sum(samples)
FROM read_folded('$FINAL_RUN_ROOT/profile/default.folded')
WHERE list_contains(
  frames,
  'spur`spur_graph::extract::languages::collect_symbol_tokens'
);

SELECT sum(samples)
FROM read_folded('$FINAL_RUN_ROOT/profile/default.folded')
WHERE list_contains(frames, 'ts_node_parent')
   OR list_contains(frames, 'ts_node_child_with_descendant');
```

Report direct-child edges for `BytesExtractor::extract` and `run_query`, hot
stacks for cache initialization and query/parent traversal, and the pre/post
delta against the integrated baseline in the source report. Keep telemetry
lock-wait/worker-time values separate from on-CPU sample coverage.

- [ ] **Step 5: Append the completion audit and commit exactly once.**

Include source/binary/trace/folded/SVG/extension/checksum-manifest hashes,
default-vs-zero timing, artifact equivalence, recursion audit, exact SQL,
before/after table, and exactly one next action item. Do not implement it.

```bash
git add docs/superpowers/plans/2026-08-22-byte-budgeted-temporal-parse-cache.md
git commit -m "docs(spur-graph): task-4 record temporal cache profile outcome"
```

---

## Self-review

- **Spec coverage:** Task 1 covers protected/retained tiers, deterministic
  eviction, failures, single-flight, locks, bytes, environment parsing, and
  telemetry parity. Task 2 covers jobs 1/2/4/8 artifact determinism. Task 3
  covers cold budget selection, RSS, solver gating, and conditional enablement.
  Task 4 covers the final xctrace/DuckDB/quack-flamegraph comparison and next
  bottleneck.
- **Placeholder scan:** No deferred implementation marker or unresolved
  command/path token remains.
- **Type consistency:** All tasks use byte counts as `u64`, the exact environment
  name `SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES`, and the same four candidate
  values `{0, 67108864, 134217728, 268435456}`.
- **DAG validation:** `task-1 -> task-2 -> task-3 -> task-4` is acyclic. File
  overlap exists only along approved dependency edges.
- **beads compatibility:** Every task has a unique ID, explicit dependency,
  acceptance criteria, worker route, scope boundary, signal checkpoint, test or
  evidence command, and commit-ready outcome.

## Task 3 results — strict Turso budget selection (2026-08-22)

### Decision

The compiled default remains **0 bytes**. The 64 MiB candidate was a large,
repeatable performance win and its two observations stayed below 4 GiB RSS,
but the strict enablement gate did not pass:

1. Ordinary-file artifact manifests and `diagnostics.parquet` multisets did not
   match, including between the two zero controls. Canonical graph content and
   workload summaries matched, but strict artifact identity is false. The zero
   controls also differ, so the diagnostic differences cannot be attributed to
   the cache.
2. Independent review subsequently completed catalog-first discovery, typed
   request validation, a persisted solve, and reload. The authoritative
   `sat` model selects zero because `artifact_identity=false` excludes 64 MiB.
3. The 128 MiB and 256 MiB sweep observations also exceeded 4 GiB RSS. They
   are excluded by their false RSS gates.

No Rust source was changed. In particular,
`DEFAULT_TEMPORAL_PARSE_CACHE_BUDGET_BYTES` remains the approved Task 1 value
`0`.

### Provenance and exact method

All generated data is preserved under the single fresh root
`/tmp/spur-turso-cache.JRhBfX`. SPUR was clean at
`6689c8ed0ca78771263ba5a1eeab4921be874d02`, tree
`315c56a49057e405c6b055dd082e0831a21687b1`; approved Task 1
`76f9023ec4b6e78f8f7e6ac79adedad353e75858` is its ancestor. Turso was
`a45cd87ff7b25a30476491037a028c43ff95d6f5`, tree
`4da9932858e594383fb379cca8f87293cf9848af`. The source checkout had only its
pre-existing untracked `.spur/`; none of the six clones contained `.spur` or
`.git/spur-graph` before execution.

The sole profiling binary was built and fetched with:

```bash
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 \
  scripts/spur-cargo zigbuild -p spur-cli --profile profiling -j 8
SPUR_CLOUD=aws-my SPUR_REMOTE_NAMESPACE=spur \
  /Volumes/Projects/Projects/spur-notebook/scripts/cloud-build/fetch.sh \
  --via-s3 --to /tmp/spur-turso-cache.JRhBfX/spur-cache-sweep \
  target/aarch64-apple-darwin/profiling/spur
```

The build completed in 25m22s after the supported wrapper's one documented
Darwin `libproc` bindings repair. The 309,903,616-byte arm64 Mach-O binary
reported `spur 1.21.0`; every observation recorded SHA-256
`42ee2f0b07411111a9a153f656c79621e1ef2736cc9088cec93cc9d9a7645f06`.
Relevant tool hashes were:

| Tool/artifact | SHA-256 |
|---|---|
| `scripts/spur-cargo` | `c0fbf1ea9b40788a5c08c5a8c798e382b620beca0c02c29d5f240a6c45ea0007` |
| `/usr/bin/time` | `b5b68522b051bf4e9481794c06e1daed24eed8801f676bedb2e8285f0f081c21` |
| `/opt/homebrew/bin/timeout` | `3620232c8cd4a8ce2d9d646cf648bd819c33264529d975796546b47b5c17add1` |
| Xcode 15.4 `xctrace` (not invoked in Task 3) | `5af2fb6481ac7e73bd4240bd4ddc268e215c9226a9709f4075a818d30f383e0f` |
| `quack_flamegraph.duckdb_extension` (not invoked in Task 3) | `6679979078c54714808bf30dc28992d9cf5eb21ba3d8fe0c9db941902f235e4b` |

For each label/byte pair, sequentially and with no concurrent observation:

```bash
git clone --quiet --local --no-hardlinks /Volumes/Projects/Projects/turso \
  /tmp/spur-turso-cache.JRhBfX/<label>/repo
/usr/bin/time -l -o /tmp/spur-turso-cache.JRhBfX/<label>/time.txt \
  /opt/homebrew/bin/timeout --signal=TERM --kill-after=10s 1500s \
  /usr/bin/env -u SPUR_GRAPH_TEMPORAL_JOBS \
  SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=<bytes> \
  SPUR_EMBEDDING_MODEL=jina-code RUST_LOG=spur_graph::git_walk=debug \
  /tmp/spur-turso-cache.JRhBfX/spur-cache-sweep graph build \
  --root /tmp/spur-turso-cache.JRhBfX/<label>/repo \
  --output /tmp/spur-turso-cache.JRhBfX/<label>/graph \
  --with-temporal --no-section-embeddings --no-code-symbol-embeddings \
  --no-analyst --temporal-jobs 8
```

The recorded order was exactly `zero:0`, `mib64:67108864`,
`mib128:134217728`, `mib256:268435456`, `zero_repeat:0`, then
`mib64_repeat:67108864`. Each clone had two packed Git object files and zero
object files with link count greater than one, independently confirming the
`--no-hardlinks` contract.

### Raw observations

All runs exited zero, were uncensored, reported `mode: Full`, processed 5,269
commits with eight temporal workers, and produced the same summary: 1,620
files, 59,141 nodes, 289,851 displayed edges, 1,583 section rows, and 52,943
final code-symbol rows.

| Run | Budget bytes | Real s | User s | Sys s | CPU work s | Peak RSS bytes | Commits/s | Avg active workers |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| zero | 0 | 776.26 | 1,665.92 | 37.31 | 1,703.23 | 3,407,659,008 | 6.787674 | 3.040 |
| mib64 | 67,108,864 | 268.95 | 596.54 | 25.56 | 622.10 | 4,005,658,624 | 19.591002 | 3.545 |
| mib128 | 134,217,728 | 254.80 | 578.16 | 26.41 | 604.57 | 4,692,672,512 | 20.678964 | 3.644 |
| mib256 | 268,435,456 | 252.39 | 576.30 | 24.24 | 600.54 | 5,148,704,768 | 20.876421 | 3.671 |
| zero repeat | 0 | 775.30 | 1,666.08 | 35.81 | 1,701.89 | 3,449,864,192 | 6.796079 | 3.038 |
| mib64 repeat | 67,108,864 | 268.17 | 598.03 | 24.75 | 622.78 | 4,078,960,640 | 19.647984 | 3.557 |

| Run | Cache hits | Retained hits | Cold init | Reparses | Reparse share of active worker | Reparse delta vs paired zero | Budget evictions | Retained tier current / peak bytes | Total payload current / peak bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| zero | 133,666 | 0 | 22,530 | 91,703 | 49.5453% | 0 | 114,233 | 0 / 0 | 0 / 43,368,240 |
| mib64 | 222,240 | 98,331 | 22,530 | 3,129 | 3.3004% | -88,574 (-96.5879%) | 24,104 | 66,901,182 / 67,108,802 | 66,901,182 / 83,911,723 |
| mib128 | 224,493 | 99,272 | 22,530 | 876 | 0.6177% | -90,827 (-99.0447%) | 20,492 | 133,993,049 / 134,217,695 | 133,993,049 / 146,370,393 |
| mib256 | 225,149 | 99,922 | 22,530 | 220 | 0.0366% | -91,483 (-99.7601%) | 18,221 | 268,381,560 / 268,435,452 | 268,381,560 / 280,065,297 |
| zero repeat | 133,666 | 0 | 22,530 | 91,703 | 49.6432% | 0 | 114,233 | 0 / 0 | 0 / 43,368,240 |
| mib64 repeat | 222,235 | 98,354 | 22,530 | 3,134 | 3.3403% | -88,569 (-96.5824%) | 24,109 | 66,901,182 / 67,108,802 | 66,901,182 / 84,330,897 |

The telemetry localizes the measured gain to the intended single lever:
post-eviction reparses fell from 91,703 to about 3,130 and reparse worker share
fell from about 49.6% to 3.3%. No scheduler/query/CLI change was mixed into the
observation.

### Paired quantitative result

With the two-value median defined conventionally as their mean:

| Metric | Zero median | 64 MiB median | Improvement |
|---|---:|---:|---:|
| Wall | 775.78 s | 268.56 s | 65.3819% |
| CPU work | 1,702.56 s | 622.44 s | 63.4409% |

Both initial and repeat comparisons reduced wall, CPU work, and reparses. The
largest selected-pair RSS was 4,078,960,640 bytes, 216,006,656 bytes below the
4 GiB boundary. Thus 64 MiB passes the timing, reparse, sign, and selected-pair
RSS sub-gates. The larger budgets fail RSS.

### Artifact and workload identity

Every run recorded the same source commit/tree and binary hash. After replacing
only each run-root path, the complete stdout workload summary had one shared
SHA-256:
`2920b19799b7f8f75d28b34c28975dec1b3e258ae7ddd1981dc2fd0ab40bab41`.
The canonical `manifest.json` was byte-identical with SHA-256
`5ab117806ab3cbf9b64bff1b308dee3447d8c454addd1ec09dd85839e2c72936`,
and every run advertised graph content hash
`e93932af19c194ac7106ea6ccd43a0f646d150bc9cde0b541ebf1808a92bf70c`.
Order-independent DuckDB row-count plus per-column XOR hashes matched for
commits, edges, edges-by-destination, unresolved edges, file manifests, files,
nodes, 8,761,084 symbol snapshots, and 9,199,355 temporal edges. The exact SQL
and output are `artifact-identity.sql` and `artifact-identity.csv` under the
artifact root.

Strict ordinary-file identity nevertheless failed. Each graph had 684 files,
but the raw file-manifest hashes were all distinct and total bytes ranged from
853,794,614 to 853,972,636 because Lance transaction/index files contain fresh
UUID-bearing metadata. More importantly, the 291,701-row diagnostics datasets
had different message multisets. Symmetric differences versus initial zero
were 91,328 rows for mib64, 92,204 for mib128, 95,304 for mib256, 91,044 for
mib64 repeat, and **92,800 even for zero repeat**. Independent review
recomputed the selection-relevant pairwise multiset distances as follows:

| Diagnostics pair | Symmetric multiset distance |
|---|---:|
| zero vs zero repeat | 92,800 |
| 64 MiB vs 64 MiB repeat | 93,276 |
| initial zero vs initial 64 MiB | 91,328 |
| repeat zero vs repeat 64 MiB | 95,284 |

The nonzero zero-vs-zero-repeat distance establishes baseline nondeterminism.
Therefore these diagnostic differences cannot be attributed to the cache, but
strict artifact identity is still false and the rule still denies enablement.

### Solver and gate audit

`solve_rule_spec({})` completed first (registry schema 1) and showed no
applicable implemented rule among accessibility/design/policy/resource, so the
generic typed solver was required. As operational history, the original worker
attempted `solve_constraint_spec` with catalog summary, request example, limits
summary, and a clean summary retry; the limits and clean retry each returned
`timed out awaiting tools/call after 300s`. The raw catalog and timeout record
remain preserved as `solver-rule-catalog.txt` and `solver-timeout.txt`.

The solver service later recovered. Independent review performed catalog-first
discovery, validated the exact typed request with `solve_constraint_check`,
persisted the solve, and reloaded it with
`get_solve_result("sol_871df45029524836")`. The finite request declared
`budget_bytes` in `{0, 67108864, 134217728, 268435456}`, fixed the measured gate
facts, and encoded nonzero eligibility as:

```text
budget_bytes =  67108864 ->
  mib64_rss_pass && mib64_reparse_both_pass && mib64_perf_pass &&
  mib64_other_metric_pass && mib64_repeat_sign_pass &&
  mib64_artifact_identity_pass
budget_bytes = 134217728 -> mib128_rss_pass
budget_bytes = 268435456 -> mib256_rss_pass
```

Zero is the finite-domain fallback. The fixed facts were
`mib64_rss_pass=true`, `mib64_reparse_both_pass=true`,
`mib64_perf_pass=true`, `mib64_other_metric_pass=true`,
`mib64_repeat_sign_pass=true`, `mib64_artifact_identity_pass=false`,
`mib128_rss_pass=false`, and `mib256_rss_pass=false`. The raw reloaded result
was preceded by a valid schema-1 preflight covering nine variables, twelve hard
constraints, zero objectives, and no soft constraints. The reload returned:

```json
{
  "solve_id": "sol_871df45029524836",
  "z3_version": "Z3 version 4.16.0 - 64 bit",
  "status": "sat",
  "model": {
    "budget_bytes": 0,
    "mib128_rss_pass": false,
    "mib256_rss_pass": false,
    "mib64_artifact_identity_pass": false,
    "mib64_other_metric_pass": true,
    "mib64_perf_pass": true,
    "mib64_reparse_both_pass": true,
    "mib64_repeat_sign_pass": true,
    "mib64_rss_pass": true
  },
  "duration_ms": 26,
  "reason": null
}
```

Interpretation: the original timeouts remain useful operational history but
are no longer a decision blocker. The persisted `sat` model is authoritative:
64 MiB is excluded only by strict artifact identity, 128 MiB and 256 MiB are
excluded by RSS, and zero is the sole eligible candidate.

| Gate | Evidence | Result |
|---|---|---|
| One frozen binary; fresh clones; exact Full jobs=8 workload | One binary hash; six distinct cache-free no-hardlinks clones; all exit 0/Full | pass |
| Every selected-pair RSS observation below 4 GiB | zero max 3,449,864,192; 64 MiB max 4,078,960,640 | pass |
| Every sweep observation below 4 GiB | 128 MiB 4,692,672,512; 256 MiB 5,148,704,768 | fail |
| Reparse reduction in both comparisons | -96.5879% initial; -96.5824% repeat | pass |
| Median wall or CPU improves at least 3% | wall +65.3819%; CPU +63.4409% | pass |
| Other timing metric regresses no more than 3% | both improve | pass |
| Initial/repeat direction agrees | wall, CPU, reparses all improve both times | pass |
| Artifact and workload identity | workload/canonical graph match; all six raw manifests and diagnostics differ, including zero vs zero repeat | **fail (`artifact_identity=false`)** |
| Persisted/reloaded solver decision | `sol_871df45029524836`: `sat`, `budget_bytes=0` | **pass → zero** |

The single production decision is therefore: **retain zero bytes**.

### Verification and SPUR record

All scoped commands exited zero through the supported wrapper:

```bash
scripts/spur-cargo test -p spur-graph retained_tier -- --nocapture
scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=0 \
  scripts/spur-cargo test -p spur-graph --test temporal_parallel -- --nocapture
scripts/spur-cargo check -p spur-graph --tests --benches
scripts/spur-cargo fmt --all -- --check
```

The two temporal integration runs each passed 1/1 tests; check finished in
1m54s; formatting was clean. Per-command logs and exit-status CSV are under the
artifact root.

The artifact root occupied 6.2 GiB at manifest time. Its checksum manifest
contains 28,170 ordinary files (excluding the manifest and its check output),
independently passed `shasum -a 256 -c` with exit 0 for every entry, and has
SHA-256
`b8726f27e52e34e9292147af3f55b91d7f3c5e77e1dd850bb01163239d01a8d3`.
Independent review also recomputed the paired medians as 775.78 s wall and
1,702.56 s CPU for zero versus 268.56 s wall and 622.44 s CPU for 64 MiB, and
confirmed one shared canonical-manifest hash, graph-content hash, and workload
hash across all six runs while all six raw manifests differ.

The original worker attempted a high-severity `scope_drift` signal for bead
`bd-35n9` with signal ID `b9b871ba-f2ce-4402-be13-75b32d530fb9`: strict
diagnostics artifact identity cannot be established or fixed without
out-of-scope artifact/test or production changes. Its worker-side call timed
out, but subsequent review processed the signal, recorded by
`spur:signal-processed:86758fd5`. This amendment discovered no new drift or
risk, so it emitted no duplicate signal. Source issue `bd-mehk` remains open;
no integration action was taken.

## Task 4 results — ade-bench xctrace and quack-flamegraph (2026-08-22)

### Decision

The explicit 64 MiB retained tier is a directional win on ade-bench, but it
does **not** change the Task 3 production decision: the compiled default stays
at **0 bytes**. One fresh zero run and one fresh 64 MiB run completed Full with
the same canonical graph. Relative to zero, 64 MiB improved wall time by
14.1221%, CPU work by 16.9435%, eliminated all 1,861 reparses, and reduced RSS
by 2.1957%. Strict output identity still failed because diagnostics and raw
Lance/Parquet bytes differ even though canonical core-table multisets match.

The current forced-64 profile localizes the next locally owned,
semantics-preserving opportunity to tree-sitter **query compilation**, not the
previous parent-walk or recursive-token readings. `compile_queries` has
1,388/6,777 unique-stack samples (20.481%) with maximum one occurrence per
stack. Parent traversal has 227/6,777 unique-stack samples (3.350%).
`collect_symbol_tokens` has only 263/6,777 unique-stack samples (3.881%); its
2,014 occurrence-summed samples (29.718%) are inflated by recursion up to 22
occurrences per stack.

### Provenance and amended method

The complete 519 MiB evidence root is
`/tmp/spur-ade-cache.m80Zts`. The approved Task 3 tip was
`432f560bcedfe913ca77b18b6e3fae5755b79237`, tree
`933f300bf42d31ace2df7255fdebf1dcd91d2198`. The frozen binary was built from
Task 2 commit `6689c8ed0ca78771263ba5a1eeab4921be874d02`; an exact scoped diff from that
commit through the Task 3 tip over `crates`, `Cargo.toml`, `Cargo.lock`,
`xtask`, and `scripts` exited zero, proving that the intervening commits changed
no Rust or build input. Reusing the binary therefore did not reuse stale
production code.

ade-bench provenance was:

| Item | Value |
|---|---|
| Mounted source | `/Volumes/Projects/Projects/ade-bench` |
| Commit | `efb8cd576f8127a2427106a553e96ccc8083b7a0` |
| Tree | `49493be6b430ee6444a22c40053d9ec5d70edaad` |
| Branch | `feat/spur-agent` |
| Source history / tracked files | 427 commits / 1,526 files |
| Walked Full workload | 278 reachable commits / 1,049 input files |
| Output summary | 1,057 files, 5,799 displayed nodes, 15,856 displayed edges |

The zero, forced-64, and profile clones were distinct local
`--no-hardlinks` clones. Each was clean, contained neither `.spur` nor
`.git/spur-graph`, and had no hard-linked Git objects. All commands used
`--with-temporal --no-section-embeddings --no-code-symbol-embeddings
--no-analyst --temporal-jobs 8`, `SPUR_EMBEDDING_MODEL=jina-code`, Full mode,
and a 1,800-second timeout for the unprofiled controls. The only A/B variable
was:

```text
zero:     SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=0
forced64: SPUR_GRAPH_TEMPORAL_PARSE_CACHE_BYTES=67108864
```

Exact expanded commands are preserved in `zero/command.txt`,
`forced64/command.txt`, `profile/record-command.txt`, and
`profile/fold-command.txt`. Both A/B runs, xctrace, and both deterministic fold
passes exited zero. The trace used actual `xctrace version 15.3 (15F31d)` from
the requested Xcode application path.

| Tool/artifact | Version or SHA-256 |
|---|---|
| SPUR profiling binary (`spur 1.21.0`) | `42ee2f0b07411111a9a153f656c79621e1ef2736cc9088cec93cc9d9a7645f06` |
| DuckDB CLI | `v1.5.5 (Variegata) d8cdaa33fd`; binary `1018d0d1ed5b87b4bee5fc0f09d060dbf20bd1d329c2cc911facec8847c1a4f8` |
| quack-flamegraph commit / tree | `bcc78d53cbf6e1564490af5da106311b1eedec43` / `3ede9c6562da2fa02d5d545fb381c02fcb53d1c8` |
| quack extension | `6679979078c54714808bf30dc28992d9cf5eb21ba3d8fe0c9db941902f235e4b` |
| Xcode `xctrace` | `5af2fb6481ac7e73bd4240bd4ddc268e215c9226a9709f4075a818d30f383e0f` |
| Trace ZIP | `a58a7278cd1c3ccfa7171e8551cc203e6b059b7db94ecd581cc286f18cb1d3e4` |
| Folded (786,419 bytes) | `cbba483213669606534efe2fd431e26c6ba2f4ac4919d8a6e0abba0cb0634924` |
| SVG (614,726 bytes) | `70ed6362ee9338b8506cf30f2912ee6b1bb5c2a26d3ecc68de10da8a24ca355d` |
| Exact SQL suite | `bae527ccdde820fdeb93c05c332730719c248f312b781bcbeda399b141d8c4d0` |
| 30-entry checksum manifest | `f694f8ad47975645a8a00dd1f015b7bcca89f7b7a75dd39397abfbd3555126e1` |

The repeat fold and SVG are byte-identical to the first fold and SVG. The
checksum manifest is `analysis/checksums.sha256`; `shasum -a 256 -c` returned
`OK` for all 30 entries.

### A/B runtime and telemetry

Both runs were uncensored Full executions below 4 GiB:

| Run | Real s | User s | Sys s | CPU work s | Peak RSS bytes | Pool s | Avg active workers |
|---|---:|---:|---:|---:|---:|---:|---:|
| explicit zero | 5.24 | 8.36 | 0.67 | 9.03 | 293,994,496 | 2.525185 | 2.782 |
| explicit 64 MiB | 4.50 | 6.83 | 0.67 | 7.50 | 287,539,200 | 1.763205 | 3.227 |

| Metric | Zero | 64 MiB | Relative change |
|---|---:|---:|---:|
| Wall | 5.24 s | 4.50 s | -14.1221% |
| CPU work | 9.03 s | 7.50 s | -16.9435% |
| Peak RSS | 293,994,496 B | 287,539,200 B | -2.1957% |
| Active-worker work | 7.026525 s | 5.690195 s | -19.0184% |
| Average active workers | 2.782 | 3.227 | +15.9957% |
| Admission-window receive wait | 2.434673 s | 1.660821 s | -31.7847% |
| Next-ordinal blocked wait | 2.361293 s | 1.601224 s | -32.1887% |

Average active occupancy rose from 34.775% to 40.338% of the eight configured
slots during the worker-pool interval. This is not an eight-core utilization
claim for the whole command; it is the pool's own active-worker telemetry.

| Cache metric | Zero | 64 MiB |
|---|---:|---:|
| Cache hits | 4,171 | 6,032 |
| Cold initializations | 2,664 | 2,664 |
| Reparse initializations | 1,861 | 0 |
| Retained-tier hits | 0 | 1,818 |
| Budget evictions | 4,525 | 0 |
| Initialization work | 5.671369 s | 4.092867 s |
| Reparse initialization work | 1.606348 s | 0 s |
| Retained-tier current/peak | 0 / 0 B | 7,139,780 / 7,139,780 B |

The ade-bench working set used only 6.809 MiB of the 64 MiB allowance and had
no budget eviction. Total cache-lock wait rose 30.6644% because cache hits rose
44.6176%; wait per hit fell from 168.286 to 152.050 microseconds (-9.6483%).
The on-CPU profile attributes only eight samples (0.119%) to the two explicit
cache-lock frames, so lock time remains off-CPU telemetry and is not added to
sample coverage.

The separate xctrace run reproduced the intended cache state: 2,664 cold
initializations, zero reparses, 1,837 retained hits, zero budget evictions,
7,139,780 peak retained-tier bytes, 3.125 average active workers, and
5.687683 seconds of active-worker work.

### Artifact identity: canonical pass, strict fail

The normalized stdout workload summary is byte-equal with shared SHA-256
`dc5d9c06c02de7f224229d2da5ef4aafb6821de39286d3a9a681da5dcd98a5a0`.
Both graphs advertise content hash
`f375e6833c3fd0e919e5d3b33b2ea6cb478d29a44bed13d523d09a9da9ea5f3d`,
and `manifest.json` is byte-identical with SHA-256
`a3a843f630c6c2c4a0a0a833434e4b4f2692339a2e0d8ba8d6f8ea802e2bf193`.

DuckDB `EXCEPT ALL` in both directions proves multiset equality for every
canonical core table below:

| Table | Rows per run | Zero-only / 64-only rows |
|---|---:|---:|
| commits | 278 | 0 / 0 |
| edges | 9,566 | 0 / 0 |
| edges_by_dst | 9,566 | 0 / 0 |
| edges_unresolved | 6,290 | 0 / 0 |
| file_manifests | 1,057 | 0 / 0 |
| files | 1,057 | 0 / 0 |
| nodes | 4,742 | 0 / 0 |
| symbol_snapshots | 17,659 | 0 / 0 |
| temporal_edges | 29,166 | 0 / 0 |
| tombstones | 0 | 0 / 0 |

Strict identity is false. Each diagnostics relation has 284 rows, with 20
zero-only and 20 forced-64-only `ambiguous_rename` messages. The complete raw
artifact trees have 66 differing entries: fresh UUID-bearing Lance index and
transaction metadata dominate the physical differences, and the snapshot and
temporal Parquet files have different bytes despite equal row multisets. This
mirrors Task 3's zero-vs-zero nondeterminism and does not prove a cache-induced
semantic change, but it still fails the declared byte/diagnostic identity gate.

### DuckDB/quack-flamegraph schema and query discipline

The original analysis attempt stopped correctly because the extension was
built for DuckDB 1.5.5 while PATH still resolved 1.5.3. The final analysis used
the explicit compatible executable:

```bash
/opt/homebrew/opt/duckdb/bin/duckdb -unsigned :memory: \
  < /tmp/spur-ade-cache.m80Zts/analysis/final-analysis.sql
```

Schema discovery ran before materialization. `read_folded(path VARCHAR)`
returns `(frames VARCHAR[], samples BIGINT, source VARCHAR)`. The table macros
return:

| Macro | Output |
|---|---|
| `flamegraph_exclusive` | `(leaf VARCHAR, exclusive BIGINT)` |
| `flamegraph_coverage` | `(frame VARCHAR, coverage BIGINT, n_stacks BIGINT)` |
| `flamegraph_edges` | `(parent VARCHAR, child VARCHAR, samples BIGINT)` |
| `flamegraph_hot_stacks` | `(samples BIGINT, depth BIGINT, leaf VARCHAR, frames VARCHAR[])` |
| `flamegraph_inclusive` | `(frame VARCHAR, inclusive BIGINT)` |

The exact 512-line query suite and every CSV result are under `analysis/` in
the evidence root. It assigns a deterministic stack ID, aggregates occurrences
per `(stack, frame)`, and derives both unique-stack coverage and
occurrence-summed inclusive counts:

```sql
SELECT frame,
       sum(samples) AS coverage,
       sum(samples * occurrences) AS occurrence_inclusive,
       max(occurrences) AS max_occurrences
FROM stack_frames
GROUP BY frame;
```

Only rows with `max_occurrences = 1` are treated as safe occurrence-summed
coverage. Union metrics such as parent traversal instead use one stack-level
predicate:

```sql
SELECT sum(samples)
FROM read_folded('/tmp/spur-ade-cache.m80Zts/profile/forced64.folded')
WHERE list_contains(frames, 'ts_node_parent')
   OR list_contains(frames, 'ts_node_child_with_descendant');
```

### On-CPU profile from different angles

The folded file contains 1,095 nonzero stacks, 6,777 samples, 1,020 distinct
frame names, and 101,448 occurrence-weighted frames. Sample-weighted mean,
median, and p90 stack depths are 14.969, 14, and 19; maximum depth is 73.
Application symbols appear in 1,027 stacks covering 6,667 samples (98.377%).
Only one sample contains an unknown/address frame, so symbolication is adequate
for directional attribution.

The mutually exclusive stack-domain partition is:

| Domain | Samples | Share |
|---|---:|---:|
| Parse | 2,483 | 36.639% |
| Query compilation | 1,388 | 20.481% |
| Query execution | 936 | 13.811% |
| Pending-edge resolution | 516 | 7.614% |
| Runtime/other | 479 | 7.068% |
| Graph store and sidecars | 445 | 6.566% |
| Token collection | 263 | 3.881% |
| Cache-path remainder | 142 | 2.095% |
| Other extraction | 106 | 1.564% |
| Process spawn | 19 | 0.280% |

Overlapping exact-frame coverage provides the call-path view:

| Target | Unique-stack samples | Coverage | Max occurrences |
|---|---:|---:|---:|
| `SharedParseCache::get_or_init_with_ordinal` path | 4,122 | 60.823% | 1 |
| `BytesExtractor::extract` | 2,994 | 44.179% | 1 |
| `ts_parser_parse_with_options` | 2,483 | 36.639% | 1 |
| `compile_queries` | 1,388 | 20.481% | 1 |
| `run_query` | 936 | 13.811% | 1 |
| `resolve_pending_edges` | 516 | 7.614% | 1 |
| `collect_symbol_tokens` | 263 | 3.881% | 22 |
| parent-node union | 227 | 3.350% | 2 across the union |
| `buckets_from_facts` | 93 | 1.372% | 1 |
| `ts_tree_delete` | 90 | 1.328% | 1 |
| cache state + initialization locks | 8 | 0.119% | 1 |

The cache wrapper's 60.823% is not self time; it encloses extraction on misses.
Direct edges show 2,994 samples from the cache wrapper into extraction and
1,114 samples into `BytesExtractor::new`. Every one of the 1,388
`compile_queries` samples flows directly into `tree_sitter::Query::new`.
Within that phase, the largest exclusive leaves are
`ts_query__perform_analysis` (730 samples, 10.773% of the whole profile),
`ts_query_new` (271, 3.999%), `analysis_state_set__insert_sorted` (243,
3.586%), and `_platform_memmove` (90, 1.328%).

The recursion audit overturns the misleading unnest reading. Token collection
appears as 2,014 occurrence-summed samples (29.718%) but covers only 263 sample
stacks (3.881%); 153 stacks repeat the frame and the maximum is 22. Parent
traversal similarly has 434 occurrence-summed samples but only 227 unique-stack
samples. `compile_queries` has no recurrence, so its 20.481% is both inclusive
and unique-stack coverage.

There is no zero-budget CPU profile in this amended short loop, so this report
does not claim a controlled before/after hotspot shift. The earlier Turso
profile is a different repository and workload; its percentages are context,
not a statistical delta. The A/B runtime and telemetry comparison is controlled
only for the two unprofiled ade-bench runs.

### Solver-backed prioritization and exactly one next action

Catalog discovery (`solve_rule_spec`, registry schema 1) found no implemented
performance-selection rule, so a finite generic optimization was used. The
request mapped measured unique-stack coverage and implementation facts for
query compilation, parsing, query execution, pending-edge resolution, token
collection, parent traversal, and cache locks. Eligibility required at least
5% measured coverage, a locally owned lever, a proven dependency contract, and
no query/parse algorithm change. Lexicographic objectives maximized coverage
then minimized risk.

`solve_constraint_check` returned valid (six variables, eleven hard
constraints, two objectives). The persisted and reloaded result
`sol_77030f6ff4584447` returned `sat` in 50 ms with Z3 4.16.0 and the model:

```text
candidate=query_compile
coverage_bp=2048
local_lever=true
share_contract_proven=true
requires_algorithm_change=false
risk_score=1
```

The sharing contract is grounded at the exact locked dependency revision,
`tree-sitter` 0.25.10 (Cargo checksum
`78f873475d258561b06f1c595d93308a7ed124d9977cb26b148c2084a4a3cc87`).
Upstream explicitly implements both `Send` and `Sync` for `Query`. Locally,
`BytesExtractor` owns `CompiledQueries`, each worker owns a
`SymbolDiffCtx`/extractor map, and `run_query` creates a fresh `QueryCursor` per
call. Thus immutable query objects can be shared without sharing mutable parser
or cursor state.

**Exactly one next action:** prototype a build-scoped immutable
`Arc<CompiledQueries>` cache keyed by `Language` (or exact language/query-source
identity), shared across all `BytesExtractor` instances, while keeping each
`Parser` worker-local and each `QueryCursor` call-local. Add a compilation-count
assertion plus existing jobs 1/2/4/8 canonical-output checks, then accept the
change only after a controlled ade-bench A/B and a larger Turso confirmation
reduce wall/CPU without changing canonical graph or diagnostic semantics.

This target has an Amdahl on-CPU ceiling, not a forecast: eliminating all
20.481% query-compilation samples would bound ideal sample-time speedup at
`1 / (1 - 0.20481) = 1.2576x`. Parallel overlap, one unavoidable compilation
per language, and the short 6,777-sample run make the achievable wall gain
smaller and currently unknown. The solver proves only that this is the unique
eligible candidate under the encoded selection gates; the benchmark must prove
the speedup.

### Verification and collaboration record

The SQL suite completed against DuckDB 1.5.5 with exit zero. Both deterministic
fold passes were byte-identical, all 30 checksum entries verified, canonical
core-table multiset comparisons were symmetric, and the documented strict
diagnostic/raw-byte failure is preserved rather than normalized away. The
repository change for Task 4 is this plan document only; no Rust source,
compiled cache default, scheduler, query walker, or token logic changed.
