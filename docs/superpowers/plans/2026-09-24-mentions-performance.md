# Mentions Performance Implementation Plan

> Direct execution under beads issue `bd-1kyu9`; no worker dispatch.

**Source spec:** `docs/superpowers/specs/2026-09-24-mentions-performance.md`

**Goal:** Remove measured redundant filesystem calls and zero-limit ranking work.

**Architecture:** Keep the existing scorer and stable top-K selector. Reuse walker
metadata for ordinary filesystem entries with an explicit symlink fallback.

**Tech stack:** Rust, ignore, nucleo-matcher, Z3, std::time release benchmarks.

1. Measure and correct benchmark accounting; add end-to-end rank and file-build
   cases (`benches/rank_top_k.rs`, `benches/file_build.rs`, `Cargo.toml`).
2. Before implementation, add the zero-limit allocation regression and symlink
   compatibility cases. Record baseline failure and the existing behavior.
3. Add the zero-limit guard in `rank.rs`; preserve existing nonzero selection.
   Use known non-symlink walker types in `file_source.rs`, otherwise stat the path.
4. Re-run the solver request and tests; compare the same benchmark binaries and
   workload on the same host. Record limits and deferred findings in the report.

Dependencies: 1 → 2 → 3 → 4. Owner: codex. All steps belong to `bd-1kyu9`.
Acceptance: unchanged ranking/profile results, no zero-limit allocations,
measured snapshot-build improvement, successful crate verification, and recorded
solver status. Do not change cache eviction policy or concurrent TUI fixes.
