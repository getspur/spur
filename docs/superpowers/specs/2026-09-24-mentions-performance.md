# Mentions performance refinement

Tracking issue: `bd-1kyu9`. Scope: `crates/spur-utilities/mentions`.

## Contract

Keep public APIs, ranking order (including stable ties), URI encoding, traversal
failure policy, and cache identity unchanged. Improve demonstrated wasted work:

- Filesystem snapshots should use walker-provided types for ordinary entries.
  Symlinks and missing type metadata must fall back to `Path::is_dir`, preserving
  target classification, dangling-link handling, and both traversal profiles.
- A pure ranking request with `Some(0)` needs no scoring, selection, or allocation.
  The engine's separate source-resolution and diagnostic contract is unchanged.
- Benchmarks must report the region actually timed. Copying excluded from a timer
  must not also be subtracted from its result.

Filesystem equivalence assumes an unchanged entry between directory enumeration
and classification and successful target metadata access. Concurrent file
replacement was never an atomic snapshot. When a directory can be enumerated but
its child cannot be stat-ed, the TUI now keeps the walker's known Directory kind
and trailing slash instead of misclassifying that child as File. This is an
intentional refinement to legacy parity. Walk errors still follow the profile's
existing permissive/strict policy; symlink classification still uses target stat.

## Evidence and verification

Use release benchmarks over 1k/10k/100k candidates, empty/matching/nonmatching
queries, and limits 0/20. Build generated 1k/10k-file trees outside timers and
rebuild snapshots on every sample. Report OS caches as warm, separately from the
absent engine cache. Compare walker-only and walker-plus-stat costs.

Catalog-first solve found no domain rule for this Boolean branch equivalence.
Generic preflighted requests model known type, symlink, type-directory, target
directory, and metadata success (`stat_ok`). Non-symlink known types agree with
actual types under the stable-entry assumption; symlink types are not directory
types. A counterexample search with successful metadata has no solution
(`sol_4e9d4130670b4482`: unsat). With denied metadata, it produces the documented
known-directory counterexample (`sol_3a4151e957644f25`: sat). These results prove
the declared Boolean model, not arbitrary concurrent filesystem behavior.

Runtime gates: ranking exactness properties; zero-limit allocation regression;
filesystem profiles including symlink targets and dangling links; complete crate
tests and optional code-feature tests. Preserve unrelated concurrent edits.

## Measurements (Apple M2, macOS, release build)

Generated fixtures: 100 directories, 1,000 or 10,000 empty files. Three warmups
and 25 timed samples per case; table values are medians. Fixture creation is
outside sample timers. Every build traverses again; no engine cache is used.
The OS cache is warm. Saved before/after executables were run sequentially on
the same host to exclude compilation from comparisons.

| Files | Profile | Before | After | Observed speedup |
|---:|---|---:|---:|---:|
| 1,000 | TUI | 10.24 ms | 7.77 ms | 1.32× |
| 1,000 | Notebook | 13.24 ms | 6.45 ms | 2.05× |
| 10,000 | TUI | 58.15 ms | 22.71 ms | 2.56× |
| 10,000 | Notebook | 124.50 ms | 26.12 ms | 4.77× |

Initial 10k-file runs were slower (164→53 ms TUI, 145→59 ms notebook), so these
are workload observations, not latency guarantees. Whole file-benchmark process
time, including fixture creation and walker comparison cases, changed from
15.62 s wall / 2.63 s user / 10.77 s sys to 10.15 / 2.02 / 7.14 s. The substantial
kernel time and direct walker-with/without-stat comparisons support targeting
metadata calls, rather than claiming the CPU scorer caused build latency.

For pure `rank_top_k` with 100k matching candidates and limit zero, measured
median fell from 59.83 ms to the timer floor (42 ns in this harness). The useful
guarantee is no scoring and zero allocations, pinned by the allocator test;
nanosecond measurements are not a throughput guarantee. The fused engine and TUI
have separate scoring paths, so this is not an engine/TUI query speedup. Normal
nonzero ranking and stable tie ordering retain the existing algorithm.

Reproduce with `scripts/spur-cargo bench -p spur-mentions --bench file_build
--bench rank_top_k`. The ranking benchmark no longer subtracts untimed copies.
The selector regression compares actual stable-sort and selection comparison
counts on the same shuffled input; it no longer misuses `log2(M!)` as a bound
on every individual input.

## Further review findings

- Typed queries still score all candidates. At 100k rows the measured scoring
  phase was tens of milliseconds and dominates normal top-20 completion. The
  stable partial selector already avoids a full sort; a different selector alone
  cannot remove that scoring cost.
- Selection allocates an indexed O(M) buffer to retain input order across equal
  ranking keys. Removing it requires preserving the stable-boundary contract,
  not merely replacing the comparator with an unstable sort.
- Snapshot TTL controls freshness, not memory reclamation. Root/profile/token
  identities remain retained until explicit invalidation or engine drop.
  Bounding retention requires an explicit eviction and failed-build policy.
- Fused queries canonicalize a root and then per-source resolution canonicalizes
  it again. Code-source hash cache hits still clone cached file rows. These are
  candidates for targeted benchmarks; this patch does not claim a measured
  benefit from changing them.

Independent review: `bd-1ibzs`. It found the metadata-denial exception, the
per-input sorting-bound overclaim, and a weak notebook symlink assertion; all
three are addressed by the revised contract and tests.

## Final verification

- Remote `scripts/spur-cargo test -p spur-mentions --features code
  --no-fail-fast`: **116 passed, 1 failed**. All ranking, allocation, filesystem,
  engine-query, engine-seam, library, and code-expansion targets passed.
- The sole failure is the concurrent engine-cache test
  `resolving_a_removed_alias_preserves_its_previous_identity_for_invalidation`
  (`tests/engine_cache.rs:165`), owned by `bd-2mr4m`. Refreshing an unresolved
  alias overwrites its remembered canonical identity. This performance patch
  does not edit `engine.rs` or `engine_cache.rs`; the failure was reported to
  that issue and those edits were preserved. The full suite is therefore not
  green. An existing `spur-graph` dead-code warning was also emitted.
- Both new regression behaviors were observed failing against the old
  implementation before the final run: zero-limit ranking allocated nine
  times; the denied-metadata directory was reported as File.
- `scripts/spur-cargo fmt -p spur-mentions -- --check` and `git diff --check`
  passed. Release benchmark binaries built and ran successfully.
- Fresh post-implementation solver calls preserved the results:
  accessible metadata `sol_2d530038b42743d7` → unsat; denied metadata
  `sol_3bc645b9ef8b4b4a` → sat with the documented directory counterexample.

Validation used an isolated snapshot of the working tree because concurrent
edits invalidated the first remote upload. No remote test failure was retried
locally to obtain a passing result.
