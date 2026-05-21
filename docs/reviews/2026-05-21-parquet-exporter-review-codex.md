# Parquet Exporter Spec — codex review

**Date:** 2026-05-21
**Reviewed:** `docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md` (commit 1efaa92a)
**Delegation:** `9eaf1d25-9760-43d3-abde-f8db9ae25f46` (codex, 1 attempt, ~$0.42 estimated)
**Framing:** First-principles thinking + MCTS-style branch exploration

> **Note:** The worker's full report ran past the artifact-store summary cap. Sections 1 (after the opening), 2 (MCTS expansion), and critical finding #1 were truncated mid-content. Verdict and findings 2–4 are intact; finding #1 is recoverable from the head of section 1 (schema mismatch around `GraphEdgeArtifact.edge_kind` / spec §6.2). Omitted regions are marked `[truncated by artifact store]` below.

---

Using `code-explore` because this review requires verified codebase claims before critique. I'll read the spec end to end first, then use the graph tools to validate the specific functions, data shapes, and caller migration surface.

The first half of the spec confirms the high-risk areas: Parquet is being used both as a storage cutover and as a future query substrate, while the hash remains tied to canonical JSON. I'm checking the current symbol graph now so the review can pin risks to real APIs and call surfaces instead of inferred names.

The actual migration surface is broader than the spec table implies: `write_artifact` has direct callers in cache code, CLI, MCP tests, TUI tests, and graph tests; `load_artifact` is used by MCP, TUI, CLI, and artifact compatibility tests. I'm reading only the line ranges the graph returned so the findings stay tied to concrete behavior.

One concrete mismatch is already clear: the current `GraphEdgeArtifact` has an `edge_kind` field, but §6.2/§6 [truncated by artifact store]

## Top critical-severity findings

[finding #1 truncated — recoverable as: spec §6.2 omits `GraphEdgeArtifact.edge_kind: Option<GraphEdgeKind>` and renames `relation`→`kind`, both of which break round-trip]

…ions/registry.rs`, `crates/spur-tui/src/mentions/code_graph/source.rs`, `crates/spur-mcp/src/server/handlers/code_graph.rs`, `crates/spur-cli/src/commands/graph.rs`.
Fix: specify and implement a shared resolver for file-or-directory artifact locations before cutover. Cache directories by `manifest.json` metadata/hash, not directory length.

3. **Rollback story is false if PR3 deletes legacy JSON.**
Risk: §9 invokes `sweep_legacy_json()` and §13 says revert PR3 gives no data loss. Those are incompatible. After a bad cutover, existing JSON caches may be gone and old binaries may fail.
Lives in: §9 PR3 and §13 rollback.
Fix: do not sweep JSON in PR3. Keep read-only legacy JSON import for one release, then delete in a later PR after Parquet has field data.

4. **The chosen edge sort optimizes the wrong "callers" query.**
Risk: §6.2 claims `WHERE src_id = X` answers callers. It answers callees. Inbound callers and reverse dependency queries still full-scan because `dst_id` is not the leading sort key. This will age badly as graph size grows.
Lives in: §6.2 and §11.4.
Fix: correct the claim and add `edges_by_dst.parquet`, or explicitly defer inbound pruning as FU-D/FU-new.

## Top medium-severity findings

1. **The headline performance gates are hypotheses, not acceptance criteria.**
Risk: Family 3 has no thresholds, so PR3 can ship slower Rust incremental loads while still "passing."
Lives in: §1, §4, §11.2, §12 Family 3.
Fix: require measured before/after numbers for Rust `load_artifact`, `read_artifact_parquet`, incremental build, and DuckDB query startup before PR3 merge.

2. **64K row groups are too coarse for current SPUR.**
Risk: 47K resolved edges fit in one row group, so current `edges.parquet` gets zero row-group pruning. At 10x scale it gets only coarse pruning.
Lives in: §6 all files.
Fix: benchmark 16K/32K/64K row groups against DuckDB and Rust reads before locking it.

3. **Schema nullability drifts from Rust structs.**
Risk: §6.5 marks `content_oid` nullable, but `GraphFileManifestEntry.content_oid` is a `String`; §6.1 marks `entity_name` nullable, but `GraphSymbolArtifact.entity_name` is a `String`. Drift invites lossy readers.
Lives in: §6.1, §6.5.
Fix: mirror Rust nullability exactly unless a migration rule is specified.

4. **`read_artifact_header` equivalent is underspecified.**
Risk: TUI uses header-only reads for cache validation. Parquet needs a manifest fast path, not a full artifact decode.
Lives in: §7, PR3, `CodeGraphMentionSource`.
Fix: add `read_artifact_header`/metadata behavior for Parquet via `manifest.json`.

## Verdict

Ship with changes: fix the lossy Parquet schema, replace file-path assumptions with a shared artifact-location resolver, remove PR3 legacy JSON sweeping, correct/add inbound edge pruning, and turn the headline performance numbers into measured PR3 gates. The core Parquet direction is sound; the current spec is not safe to execute as written.
