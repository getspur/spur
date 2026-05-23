# Parquet Exporter Spec — claude-code review

**Date:** 2026-05-21
**Reviewed:** `docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md` (commit 1efaa92a)
**Delegation:** `bc79d218-5885-429b-8346-54a430c5bccb` (claude-code, 1 attempt, ~$0.52 estimated)
**Framing:** First-principles thinking + MCTS-style branch exploration

> **Note:** The worker's full report ran past the artifact-store summary cap. Sections 2 (MCTS expansion) and most of section 3 (top critical findings) were truncated mid-content; verdict section 5 names every critical finding (C1–C5) so the picture is recoverable. The omitted regions are marked `[truncated by artifact store]` below.

---

## 1. First-principles audit of headline claims

**8–14× smaller on disk (42 MB → 3–6 MB).** Defensible. Today's `write_artifact` calls `serde_json::to_string_pretty` (json.rs:461), so the 42 MB number is *pretty-printed* JSON: ~2× bloat from whitespace. Strip whitespace and you're at ~20 MB. Dictionary encoding on `file_path` (1.5k distinct over 27.5k+47k+68k rows) plus `kind`/`confidence` (~10 values over 115k rows) plus ZSTD-3 on the remaining narrow ints/strings credibly gets to 3–6 MB. The 14× upper bound is the credit you get from pretty-printing, not from Parquet. **Honest range is 4–8×**; calling it "8–14×" is leaning on whitespace savings the team gave themselves.

**5–8× faster cold load (250–500 ms → 50–80 ms).** Plausible at the I/O layer (200 MB/s serde_json vs. ~80 MB/s Parquet decode at much smaller input), but the existing JSON load path also runs `deduplicate_symbols` and `validate_ranges` (schema.rs:3 [truncated by artifact store]

## 2. MCTS expansion of high-impact decisions

[fully truncated by artifact store]

## 3. Top critical-severity findings

[partially truncated by artifact store; the verdict section below names all five (C1–C5)]

…**Fix:** make `parquet`/`arrow-*` non-optional in `[dependencies]`. Drop the feature flag entirely.

## 4. Medium-severity findings

**M1. Row-group pruning is asymptotic, not current.** With `row_group_size = 65_536` and a fixture of 27.5k nodes / 47k edges / 68k unresolved, every table is a single row group. The "row-group pruning to ~one group" win in §4 is zero at today's scale; it kicks in around 3–5× current size. State this in §4 so reviewers don't expect query speedups today.

**M2. `confidence` and `confidence_score` redundancy (§6.2).** `confidence` is the discriminator (`SyntaxExact`/`Heuristic`/`Unknown`); `confidence_score` is the numeric. Storing both is intentional for round-trip, but flag it — a future hash-basis swap (FU-F) is the right moment to drop one.

**M3. Hash-stability snapshot test (1.2) doesn't cover field ordering of `GraphArtifactBodyForHash`.** The canonical JSON depends on serde's struct-field declaration order in `json.rs:466`. Any reorder of that struct silently breaks hashes across the fleet. **Fix:** add a `#[deny(non_snake_case)]`-style review comment to that struct, or assert the field order via a `serde_json::to_string` snapshot of a minimal artifact in addition to the hash hex.

**M4. The "one-time" `sweep_legacy_json()` runs on every `cache-init` (§9 PR3 last paragraph) but is described as "idempotent."** Idempotent ≠ free. If called on every build it does an extra directory walk. Mark it as a one-shot gated on a flag file (e.g., `.git/spur-graph/.legacy_swept`) and stop running after first success.

**M5. Sort key for `nodes.parquet` is `(file_path ASC, id ASC)` but `node_id` is assigned by `enumerate()` on that same sorted order (§6.1, §6.5).** That's fine — but means `node_id` is not stable across builds when a file is added/removed. Any external system that caches `node_id` between builds breaks. State this invariant explicitly: **`node_id` is per-artifact-only; never persist it outside a single `.parquet/` directory.** Otherwise DuckDB MCP queries that materialize a result set with `node_id` and revisit it next build will silently drift.

## 5. Verdict

**Ship with changes.** The core architectural decision (Parquet directory, in-memory canonical-JSON hash retained, `artifact_from_facts_incremental` untouched) is sound and the POC has already de-risked the read side. But ship requires: (1) split PR3 into reader-tolerance and writer-flip PRs per Decision E; (2) fix the rollback story per C1; (3) add `read_artifact_header` to the migration table per C2; (4) make Test 2.4 a hard CI gate per C3; (5) rewrite the RSS and speedup claims to separate consumer paths and quote honest ranges per C4 and §1; (6) drop the incoherent default-feature flag per C5/D. None of these are back-to-brainstorming items — they're spec edits and a PR-sequence reshuffle. The decision that will age worst is the symlink-into-git-internals story across multi-worktree setups (Decision B); not blocking, but watch it.
