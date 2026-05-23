# Parquet Exporter Spec v2 — codex review

**Date:** 2026-05-22
**Reviewed:** `docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md` (commit b46000f1 — v2)
**Delegation:** `e11f4f90-00c8-45a5-a464-824fd45f243e` (codex, ~$0.30 estimated)
**Framing:** First-principles + MCTS exploration; regression check on v1 F1–F13 + fresh attack on v2.

> **Note:** The worker's full report ran past the artifact-store summary cap. The regression-check table (after F1) and most of section 2 (MCTS expansion) were truncated mid-content. Verdict and findings sections are recoverable below. The omitted regions are marked `[truncated by artifact store]`.

---

I'll use the required code-exploration workflow first, then read the spec and prior review files in full before checking the Rust schema and artifact I/O paths.

Using `code-explore` because this review depends on existing Rust symbols and call paths, not just the design text. I'll keep the final report to the requested sections only.

The graph MCP failed before returning symbols because the current graph artifact contains `edge_kind` while the MCP loader expected the older edge schema. I'm falling back to narrow file reads and will treat that failure as relevant evidence only where it bears on the spec's migration risk.

The current schema confirms the v2 column-name/nullability fixes are mostly aligned with Rust structs. I'm now checking the call paths the spec names, especially places that currently assume `.json` files or `is_file()`.

## Regression Check On V1 Findings F1–F13

| Finding | v2 fix | Justification |
|---|---:|---|
| F1 overstated size/load wins | yes | §4 now u[truncated by artifact store]

[F2–F13 truncated. Verdict implies most were resolved or partial.]

## MCTS expansion of v2's highest-impact NEW decisions

[fully truncated by artifact store; tail visible below describes the perf-CI MCTS branch]

…e_artifact`, so "≤ current write_artifact" needs a preserved baseline path. N=10 medians on shared CI plus DuckDB CLI/getrusage will be noisy unless isolated.
Back-propagated verdict: keep gates, but make baselines concrete before PR3b and separate correctness CI from perf CI.

## Top Critical-Severity Findings On V2

1. **§6 creates an invalid endpoint ID domain.** Existing `GraphEdgeArtifact.source_stable_symbol_id` can be a file `stable_file_id` for `Contains` edges, but `nodes.parquet` mirrors only `GraphSymbolArtifact`, and `src_id` is defined as `stable_symbol_id -> node_id`. Also `file_manifests.node_ids` are existing extractor `NodeId(u64)` values, not enumerate-over-sorted-symbol IDs.
Fix: define one endpoint table covering files + symbols, or add file IDs to `files.parquet` and make edge endpoint resolution total. Keep `file_manifests.node_ids` byte-equivalent or rename the new Parquet-only IDs.

2. **PR3b does not specify atomic Parquet directory publication.** Current JSON cache writes a temp file then renames; a multi-file directory can be observed half-written or treated as valid because `canonical.exists()` succeeds.
Fix: write to a sibling temp directory, write/validate all footers, write manifest last, fsync, then atomically rename to `<hash>.parquet/`. Resolver must require a complete valid manifest.

3. **§7 resolver safety is underspecified.** `ArtifactLocation { LegacyJson, Parquet }` is fine internally, but v2 does not define precedence when pointer, legacy worktree JSON, explicit env path, and `CURRENT` disagree.
Fix: add a resolver contract table and PR3a tests for every current caller path in `spur-cli`, `spur-tui`, and `spur-mcp`.

## Top Medium-Severity Findings On V2

1. **§12 Family 3 gates are not yet measurable as written.** Relative "current JSON" baselines disappear during PR3b.
Fix: capture PR3a baselines or retain test-only legacy bench helpers until PR3b lands.

2. **`edges_by_dst.parquet` may be the wrong permanent artifact choice.** It adds +50% resolved-edge data before proving inbound lookup dominates.
Fix: include single-file + lazy DuckDB dst-sort in the pre-PR2 benchmark.

3. **§11.5 relies on agent discipline for `node_id`.** That will age badly.
Fix: MCP-facing views should expose `stable_symbol_id` by default and hide `node_id` behind internal views or require `(artifact_hash, node_id)` pairs.

4. **§7 says `read_artifact_header_parquet` returns counts/schema/commit oid via `GraphIndexHeader`, but the current type only has `graph_index_version` and `content_hash_blake3`.**
Fix: introduce a distinct `GraphArtifactManifest`/metadata type or expand the header contract explicitly.

## Verdict

Ship with 3 more edits: fix the endpoint/node-id domain, specify atomic Parquet directory publication, and make resolver precedence/test coverage explicit. `edges_by_dst` does not need back-to-brainstorming, but it must be benchmarked against lazy DuckDB materialization before becoming mandatory artifact surface.
