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
