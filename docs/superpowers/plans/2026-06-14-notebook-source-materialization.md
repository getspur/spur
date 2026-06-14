# Notebook Source Materialization (D1) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source investigation:** debugging session 2026-06-14 (knowledge_context_pack analyst failure → notebook byte-range root cause). See also the D2 resilience fix on branch `spur/worker/v2/codex/9274e99cb91dc99e/8e64b1f4-…` (boundary-skip guard in `lance_sections.rs`).

**Goal:** Make notebook (`.ipynb`) symbols carry their real source text so embeddings, `code_read_symbol`, and TUI mentions stop emitting garbage/skipping for notebook content.

**Architecture (approach C — decouple text from byte ranges):** Notebook extraction stamps **cell-relative** byte/line ranges onto `.ipynb`-file-path nodes; since the `.ipynb` is JSON-escaped + array-split, there is no faithful map back to file bytes, so every consumer that slices the file by a symbol's range produces garbage (or, pre-D2, aborts the embedding sidecar). Fix: add an optional `source_text` field to the symbol artifact that the extractor populates with the **decoded** cell/section/symbol text; consumers prefer `source_text` when present and fall back to file-slicing (with the D2 boundary guard) otherwise. `byte_range`/`line_range` remain as positional metadata. This phase is scoped to **spur-graph** (schema + extractor + embedding sidecar), which restores hybrid/semantic notebook search; the cross-crate consumers (`spur-mcp::code_read_symbol`, `spur-tui` mentions) are a follow-up phase that builds on the new field.

**Tech Stack:** Rust 2021, tree-sitter, Arrow/Parquet (`store/parquet.rs`), LanceDB (`store/lance_sections.rs`), `serde`.

**Product assumption (flag for reviewer):** this phase assumes notebook cell/markdown content **should** be hybrid-searchable. If the near-term goal is graph structure only (cells/ports/dataflow edges), stop after Task 3's fallback-skip and defer Tasks 2/4.

---

### Task 1: Add `source_text` to the symbol artifact + Parquet round-trip

**Task ID:** `task-1-schema`

**Files:**
- Modify: `crates/spur-graph/src/schema.rs:157` (`GraphSymbolArtifact`)
- Modify: `crates/spur-graph/src/store/parquet.rs` (symbol column write/read)
- Modify: `crates/spur-graph/src/store/build.rs` (fact → artifact assembly; default `None`)
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (FactBuilder carries optional source text per node; default `None`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `GraphSymbolArtifact` gains `source_text: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (struct keeps `deny_unknown_fields` — the `default` makes old artifacts deserialize).
- [ ] Parquet writer emits a nullable UTF8 column; reader restores `Some`/`None` exactly.
- [ ] All existing round-trip tests still pass; new round-trip test for `source_text` passes.
- [ ] Non-notebook extraction is unchanged (field is `None`).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the 4 files above, additive field + serialization only.
- OUT of scope: changing any consumer behavior (Task 3), populating the field (Task 2), touching `spur-mcp`/`spur-tui`.
- **Scope-drift checkpoint:** if the additive Parquet column forces a format-version bump or touches >5 files, emit `scope_drift` before proceeding.

**Implementation:**
- [ ] **Step 1: Failing round-trip test** in `store/parquet.rs` tests:

```rust
#[test]
fn symbol_artifact_round_trips_source_text() {
    let mut sym = test_symbol("a::b", "function"); // existing helper
    sym.source_text = Some("def f():\n    return 1\n".to_owned());
    let artifact = test_artifact_with_symbols(vec![sym.clone()]);
    let bytes = write_artifact_to_parquet(&artifact);
    let read = read_artifact_from_parquet(&bytes);
    assert_eq!(read.symbols[0].source_text, sym.source_text);
    // And the None case survives:
    let mut none_sym = test_symbol("a::c", "function");
    none_sym.source_text = None;
    let rt = read_artifact_from_parquet(&write_artifact_to_parquet(
        &test_artifact_with_symbols(vec![none_sym]),
    ));
    assert_eq!(rt.symbols[0].source_text, None);
}
```

- [ ] **Step 2:** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph symbol_artifact_round_trips_source_text` → FAILS (field missing).
- [ ] **Step 3:** Add the field to `GraphSymbolArtifact`; add a nullable UTF8 column to the symbol Parquet schema in `store/parquet.rs` (mirror an existing `Option<String>` column such as `enclosing_scope` for the null-handling pattern); thread `source_text: None` through `store/build.rs` assembly and the `FactBuilder` node push in `extract/tree_sitter.rs`.
- [ ] **Step 4:** test passes; full `scripts/spur-cargo test -p spur-graph` green.
- [ ] **Step 5:** `git commit -m "feat(spur-graph): add source_text field to symbol artifact"`

---

### Task 2: Populate `source_text` in the notebook extractor

**Task ID:** `task-2-extractor`

**Files:**
- Modify: `crates/spur-graph/src/extract/notebook.rs` (`add_cell_node`, `intern_port`, `extract_cell` child path)
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (a `add_node_with_range_and_source(..., source_text: Option<String>)` variant, or an explicit setter on the just-pushed node)

**Depends on:** `task-1-schema`

**Acceptance Criteria:**
- [ ] Each notebook **cell** node's `source_text` = the decoded cell source (`cell_source_text`).
- [ ] Each child symbol/section extracted from a cell gets `source_text` = the substring of the **decoded cell source** for its (cell-relative) range — i.e. `cell_source.get(range).map(str::to_owned)` (valid because the range IS relative to that string).
- [ ] `port` nodes get `source_text = None` (no meaningful body).
- [ ] Non-notebook extraction still emits `None`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `extract/notebook.rs` and the minimal `tree_sitter.rs` builder hook.
- OUT of scope: consumer changes, schema/parquet (done in Task 1), `byte_range` correctness (intentionally unchanged — ranges stay positional).
- If you find the builder cannot attach per-node source without touching >2 files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Failing test** in `extract/notebook.rs` tests (model on `each_cell_gets_a_cell_container_node`):

```rust
#[test]
fn cell_and_children_carry_decoded_source_text() {
    let nb = json!({
        "cells": [{
            "cell_type": "code",
            "metadata": {"spur": {"code_type": "python"}},
            "source": ["def f():\n", "    return 1\n"]
        }],
        "metadata": {"kernelspec": {"name": "python3"}}
    });
    let facts = run_notebook_extraction(&nb.to_string()); // existing helper
    let cell = facts.symbols.iter().find(|s| s.symbol_kind == "cell").unwrap();
    assert_eq!(cell.source_text.as_deref(), Some("def f():\n    return 1\n"));
    let func = facts.symbols.iter().find(|s| s.entity_name == "f").unwrap();
    assert!(func.source_text.as_deref().unwrap().contains("def f()"));
    assert!(!func.source_text.as_deref().unwrap().contains("\\n")); // decoded, not JSON-escaped
}
```

- [ ] **Step 2:** run it → FAILS (`source_text` is `None`).
- [ ] **Step 3:** add a builder hook that sets `source_text` on the node just pushed; in `add_cell_node` pass `Some(source.to_owned())`; in `extract_cell`, when delegating to the markdown/code child extractors, give each child `cell_source.get(child_range).map(str::to_owned)`.
- [ ] **Step 4:** test passes; `scripts/spur-cargo test -p spur-graph` green.
- [ ] **Step 5:** `git commit -m "feat(spur-graph): populate notebook symbol source_text from decoded cells"`

---

### Task 3: Prefer `source_text` in the embedding sidecar (and keep the D2 boundary guard)

**Task ID:** `task-3-embeddings`

**Files:**
- Modify: `crates/spur-graph/src/store/lance_sections.rs` (`section_row`, `symbol_row`, `doc_text_for_symbol`, `first_source_line_for_symbol`)

**Depends on:** `task-1-schema`

**Acceptance Criteria:**
- [ ] When `symbol.source_text` is `Some`, `section_row`/`symbol_row` build `body_text`/`doc_text`/`embed_text` from it (NOT from a file slice).
- [ ] When `source_text` is `None`, behavior is unchanged: slice the file by range, and **skip the row with a warning if the range is not a valid UTF-8 boundary** (the D2 guard — re-establish it here if HEAD lacks it: `let Some(body) = source.get(start..end) else { tracing::warn!(...); return Ok(None) };` for sections, and the `is_none()` guard before slicing for symbols).
- [ ] A notebook section/symbol with `source_text` produces clean body text even though its `byte_range` is invalid against the `.ipynb`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `store/lance_sections.rs` only.
- OUT of scope: schema, extractor, `spur-mcp`/`spur-tui`.

**Implementation:**
- [ ] **Step 1: Failing test** (model on `section_row_batcher_skips_non_utf8_boundary_ranges`): a `section` symbol whose `byte_range` is `[0, mid_char]` (invalid vs the file) but whose `source_text = Some("## Heading\n\nBody")` yields a `SectionRow` whose `body_text == "## Heading\n\nBody"` (used, not skipped).
- [ ] **Step 2:** run → FAILS (current code ignores `source_text`, skips on the bad range).
- [ ] **Step 3:** at the top of `section_row`/`symbol_row`, branch: `if let Some(text) = symbol.source_text.as_deref() { /* build from text */ } else { /* existing file-slice path + D2 boundary guard */ }`.
- [ ] **Step 4:** test passes; `scripts/spur-cargo test -p spur-graph` green.
- [ ] **Step 5:** `git commit -m "feat(spur-graph): embed notebook symbols from stored source_text"`

---

### Task 4: Verify notebook content is embeddable end-to-end

**Task ID:** `task-4-verify`

**Files:**
- Test: `crates/spur-graph/src/store/lance_sections.rs` (or a new `tests/notebook_embedding.rs`)

**Depends on:** `task-2-extractor`, `task-3-embeddings`

**Acceptance Criteria:**
- [ ] An integration-style test builds symbol rows for a notebook fixture and asserts the emitted `SymbolRow`/`SectionRow` `embed_text`/`body_text` contains the decoded cell tokens (e.g. `def f`) and NOT raw JSON (`"cell_type"`, `\\n`).
- [ ] No regression: the existing boundary-skip tests still pass.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: test code + any trivial test helper.
- OUT of scope: production behavior changes (those are Tasks 1–3).

**Implementation:**
- [ ] **Step 1:** write the integration test using the `graph_artifact_for_path` helper (already added in the D2 work) with a notebook-derived symbol carrying `source_text`.
- [ ] **Step 2:** run → expect PASS (Tasks 2–3 make it pass); if it fails, the gap is a real bug in Task 2/3 — report it.
- [ ] **Step 3:** `git commit -m "test(spur-graph): notebook symbols embed decoded cell source"`

---

## Self-review

- **Coverage:** schema (T1) → populate (T2) → consume (T3) → verify (T4). The headline capability (hybrid/semantic notebook search) is restored within spur-graph. Cross-crate consumers (`code_read_symbol`, TUI mentions) are an explicit follow-up phase, noted in Architecture.
- **DAG:** `T1 → {T2, T3} → T4`. T2 and T3 are independent after T1 (different files), maximizing parallelism. No cycles.
- **Interfaces:** T2/T3/T4 all reference the `source_text: Option<String>` field defined in T1.
- **No placeholders:** every task has real failing-test code and concrete file targets.
- **Risk:** T1's Parquet column is the only schema-format touch; its scope-drift checkpoint bounds it.
