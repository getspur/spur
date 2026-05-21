# Parquet Exporter Spec v2 — claude-code review

**Date:** 2026-05-22
**Reviewed:** `docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md` (commit b46000f1 — v2)
**Delegation:** `e06b732c-cc3f-45ed-b445-c1819bf262a2` (claude-code, ~$0.50 estimated)
**Framing:** First-principles + MCTS exploration; regression check on v1 F1–F13 + fresh attack on v2.

> **Note:** The worker's full report ran past the artifact-store summary cap. Sections 2 (MCTS expansion) and most of section 3 (critical findings) were truncated mid-content. Verdict section 5 names every critical finding (C1, C2, C3, N1) so the picture is recoverable. The omitted regions are marked `[truncated by artifact store]` below.

---

## 1. Regression check on v1 findings F1–F13

| Finding | Fix in v2 | Verdict | Justification |
|---|---|---|---|
| F1 (8–14× → honest range) | §4 table rewritten | **yes** | 4–8× plus pretty-print disclosure is accurate. |
| F2 (5–8× cold load) | §4 / §1 split by consumer path | **yes** | Decomposed into 3–6× + validation overhead disclosure. |
| F3 (RSS conflation; OOM specific to DuckDB) | §4 row 3 | **yes** | OOM correctly scoped to DuckDB UNNEST. |
| F4 (PR3 monolith → safe rollback) | §9 PR3a/PR3b split, §13 per-PR rollback | **partial** | PR3a additivity is real, but PR3b still flips writer + deletes `write_artifact` + renames `json.rs` in one commit — see C2 below. |
| F5 (`read_artifact_header` missing) | §7 `read_artifact_header_parquet` + `manifest.json` | **yes** | Header fast path is now first-class. |
| F6 (`sweep_legacy_json` incoherent w/ rollback) | §9 — sweep deferred to FU-G | **yes** | R[truncated by artifact store]

[F7–F13 entries truncated. Verdict implies they were treated as resolved.]

## 2. MCTS expansion of v2's highest-impact NEW decisions

[fully truncated by artifact store]

## 3. Top critical-severity findings on v2

[partially truncated; final paragraph visible below — assumed to be C3 fix detail]

…new bug surface in `spur-mcp/code_graph.rs` and `spur-tui/registry.rs`, both of which today cache by canonical path), or there's a hidden second cache key path. **Fix:** specify two cache-key derivations explicitly — `LegacyJson` derives from file mtime + path; `Parquet` derives from `manifest.json.graph_content_hash`. Lock the shared lookup signature so business logic *never* matches on the variant.

## 4. Medium-severity findings on v2

**M1. Family 3.5 (`≤ 250 ms` cold first-query) and 3.6 (`≤ 500 MB` peak RSS) are pulled from thin air.** No reference measurement, no derivation. They could be 100× too tight or too loose. **Fix:** run the POC's measured numbers (commit 754b07a8) and set thresholds at e.g. 1.5× POC median; record provenance in §12.

**M2. `confidence_score: f32` widened to `Float64` (§6.2) breaks `PartialEq` byte-equivalence on round-trip.** `GraphEdgeArtifact` derives `PartialEq` (schema.rs:161), not `Eq` — because of `f32`. Widening to `f64` then narrowing back is exact for finite values but Family 1.1 must use NaN/inf-aware comparison. **Fix:** spell this out in 1.1; alternatively keep `Float32` in Parquet.

**M3. `edges_by_dst.parquet` writer can drift from `edges.parquet` on partial-write failure.** No atomic-rename or two-phase commit specified. If the process dies between the two writes, the directory has an inconsistent edges/edges_by_dst pair. **Fix:** write to `*.tmp` then atomic rename the whole directory; or add a `manifest.json.complete = true` sentinel checked by `read_artifact_parquet`.

**M4. `enclosing_scope` marked DICTIONARY nullable (§6.1) — likely high-cardinality.** Dictionary encoding helps only when distinct count is low. Enclosing scopes are typically per-symbol distinct (module path + line). **Fix:** measure cardinality in the pre-PR2 bench; switch to PLAIN if dict ratio > 0.5.

**M5. `read_artifact_parquet` ignores `edges_by_dst.parquet` (§7 docstring) but Family 2.1 reads it.** Fine, but the writer must guarantee it; add a Family 2.5 assertion that `edges_by_dst` and `edges` contain identical row multisets.

## 5. Verdict

**Ship v2 with 3 more edits.** (1) Fix C1 — the `node_id` enumeration rule directly contradicts the round-trip contract and breaks every JOIN with `file_manifests`; this is a spec-level error, not an implementation detail. (2) Fix C2 — split PR3b into writer-flip and cleanup PRs so the documented rollback story matches reality. (3) Fix C3 — make the resolver-to-cache-key path explicit so the new `ArtifactLocation` enum does not metastasize into call-site `match`es. M1–M5 are tighten-and-go. None of these need back-to-brainstorming; the architectural shape (Parquet directory, in-memory canonical-JSON hash retained, dual edge sort, hard CI gates) is sound. The decision that will age worst is N1 if not corrected — DuckDB queries silently producing wrong joins is the exact "silent drift" §11.5 warns against, except it would be the writer drifting from itself.
