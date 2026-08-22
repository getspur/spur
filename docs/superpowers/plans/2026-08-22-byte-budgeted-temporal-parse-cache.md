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
