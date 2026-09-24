# Mentions performance refinement

Tracking issue: `bd-1kyu9`. Scope: `crates/spur-utilities/mentions`.

## Contract

Keep public APIs, ranking order (including stable ties), URI encoding, source
failure behavior, and cache identity unchanged. Improve demonstrated wasted work:

- Filesystem snapshots should use walker-provided types for ordinary entries.
  Symlinks and missing type metadata must fall back to `Path::is_dir`, preserving
  target classification, dangling-link handling, and both traversal profiles.
- A pure ranking request with `Some(0)` needs no scoring, selection, or allocation.
  The engine's separate source-resolution and diagnostic contract is unchanged.
- Benchmarks must report the region actually timed. Copying excluded from a timer
  must not also be subtracted from its result.

Filesystem equivalence assumes an unchanged entry between directory enumeration
and classification. Concurrent file replacement was never an atomic snapshot;
the optimized implementation uses the walker's observation for ordinary entries.

## Evidence and verification

Use release benchmarks over 1k/10k/100k candidates, empty/matching/nonmatching
queries, and limits 0/20. Build generated 1k/10k-file trees outside timers and
rebuild snapshots on every sample. Report OS caches as warm, separately from the
absent engine cache. Compare walker-only and walker-plus-stat costs.

Catalog-first solve found no domain rule for this Boolean branch equivalence.
Generic preflighted requests model known type, symlink, type-directory, and target
directory. Non-symlink known types agree with targets under the stable-entry
assumption; symlink types are not directory types. A type-only implementation has
a counterexample; the fallback predicate has none in this encoded domain
(`sol_e5744ff413764e2c`: sat; `sol_4801ef54b9d3440a`: unsat).

Runtime gates: ranking exactness properties; zero-limit allocation regression;
filesystem profiles including symlink targets and dangling links; complete crate
tests and optional code-feature tests. Preserve unrelated concurrent edits.
