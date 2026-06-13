# spur-graph Jupyter Notebook (.ipynb) Extraction Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-10-spur-graph-jupyter-notebook-support-design.md`
**Base branch:** `spur/context-provider-rebased` (the integrated context-provider slices 1–2, rebased on current main)

**Goal:** Make `spur-graph` discover and index `.ipynb` files, extracting per-cell symbols (Python/JS code cells, markdown headings) into the code graph so they surface in the analyst index and `code_*`/`knowledge_context_pack`.

**Architecture:** `.ipynb` is a JSON container with no tree-sitter grammar of its own, so it is handled **programmatically** and special-cased exactly like `Language::Markdown` is today: a new `Language::JupyterNotebook` is registered for the `ipynb` extension, `BytesExtractor` skips single-grammar parser setup for it, and `extract_graph_facts` routes it to a new `extract/notebook.rs` that parses the JSON, resolves each cell's language via a fallback chain, and delegates each cell's source to the per-language extraction the crate already has. Each cell's symbols become `Contains` children of the `.ipynb` file node.

**Tech Stack:** Rust, `serde_json` (already a `spur-graph` dep), existing `tree_sitter` per-language configs, `FactBuilder`/`extract_file_from_tree` machinery.

**Scope correction (verified against current code):** spur-graph's `Language` enum has only `Rust`, `Python`, `TypeScript`, `Tsx`, `Javascript`, `Markdown`, `C`, `Cpp`, `Lua`, `Shell`. There is **no Go/Julia/R grammar**, so the spec's mapping table for `go`/`julia`/`r` is aspirational — this plan maps cell languages only to existing variants (Python, JS→Tsx grammar, Rust) and skips everything else with a warning, matching the spec's `(fallback) → skip cell with warning` row.

**Build/test command:** always `scripts/spur-cargo test -p spur-graph`, never bare cargo. Force remote from the sandbox if clippy is needed: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph`.

---

### Task 1: Register `Language::JupyterNotebook`

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-graph/src/extract/languages.rs` (enum + all `match self` arms + `from_path`/registry)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `Language::from_path(Path::new("x.ipynb")) == Some(Language::JupyterNotebook)`
- [ ] `JupyterNotebook` is handled in every `match self` in `impl Language` (no non-exhaustive-match compile error): `tree_sitter_language`, `config`, `builtin_method_names`, `label`
- [ ] Registry descriptor present with `extensions: &["ipynb"]`
- [ ] `scripts/spur-cargo test -p spur-graph languages` passes

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `languages.rs` only
- OUT of scope: `notebook.rs` (task-3), `tree_sitter.rs` dispatch (task-4), the gate test in `tree_sitter.rs` (task-4 owns it)
- If a `match self` elsewhere (outside languages.rs) fails to compile from the new variant, that is expected and is handled by task-4 — note it but do not edit those files; emit `scope_drift` only if a non-task-4 file is implicated.

**Implementation:**

- [ ] **Step 1: Failing test** (in `languages.rs` `#[cfg(test)] mod tests`, or extend existing):

```rust
#[test]
fn ipynb_path_resolves_to_jupyter_notebook() {
    assert_eq!(
        Language::from_path(std::path::Path::new("analysis.ipynb")),
        Some(Language::JupyterNotebook)
    );
}
```

- [ ] **Step 2:** `scripts/spur-cargo test -p spur-graph ipynb_path_resolves` → FAIL (no variant)

- [ ] **Step 3: Implement.** Add `JupyterNotebook` to `enum Language` (after `Markdown`). Then:
  - `tree_sitter_language`: container has no grammar, but the method must return something. Return the markdown grammar as an inert placeholder (never used — task-4 bypasses parsing for this variant): `Self::JupyterNotebook => tree_sitter_md::LANGUAGE.into(),`
  - `config`: `Self::JupyterNotebook => jupyter_notebook_config(),` — add a `pub(crate) fn jupyter_notebook_config() -> LanguageConfig` that mirrors `markdown_config()`'s shape but with empty `queries`, empty `definition_kind_map`, `relation_kind_map: None`, `preserve_bare_import_path: false`, `is_method: None`, and `language: tree_sitter_md::LANGUAGE.into()` (placeholder, unused).
  - `builtin_method_names`: `Self::JupyterNotebook => &[],`
  - `label`: `Self::JupyterNotebook => "jupyter_notebook",`
  - Add a `LanguageDescriptor` to `language_registry()`:

```rust
LanguageDescriptor {
    matcher: |path| matches_extension(path, &["ipynb"]),
    factory: jupyter_notebook_config,
    language: Language::JupyterNotebook,
    label: "jupyter_notebook",
    extensions: &["ipynb"],
},
```
  (Use whatever the registry's existing extension-matcher helper is — copy the exact `matcher` form used by the `Markdown` descriptor at the registry entry near `extensions: &["md"]`.)

- [ ] **Step 4:** `scripts/spur-cargo test -p spur-graph languages` → PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/extract/languages.rs
git commit -m "feat(spur-graph): NB1 register Language::JupyterNotebook for .ipynb"
```

---

### Task 2: Cell language resolution (pure fallback chain)

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-graph/src/extract/notebook.rs`
- Modify: `crates/spur-graph/src/extract/mod.rs` (add `pub(crate) mod notebook;`)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `resolve_cell_language` implements the fallback chain: `cell.metadata.spur.code_type` → `cell.metadata.kernelspec.name` → `root.metadata.kernelspec.name` → `root.metadata.language_info.name`
- [ ] Maps `python`/`python3` → `Language::Python`; `javascript` → `Language::Javascript`; `rust`/`evcxr` → `Language::Rust`; everything else (`go`, `gonb`, `julia`, `r`, `ir`, unknown) → `None`
- [ ] Markdown cells are detected separately by `cell_type == "markdown"` (helper `cell_is_markdown`)
- [ ] Tests pass

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `notebook.rs` (new — this task adds only the resolution helpers + tests), `mod.rs` module line
- OUT of scope: `extract_notebook_file` (task-3 adds it to the same file — do not implement extraction yet), `tree_sitter.rs`, `languages.rs`
- If task-3's later additions seem to need a different signature, that's fine — keep `resolve_cell_language` a standalone pure fn.

**Implementation:**

- [ ] **Step 1: Failing tests** (`notebook.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_type_takes_precedence() {
        let cell = json!({"cell_type":"code","metadata":{"spur":{"code_type":"javascript"}}});
        let root = json!({"metadata":{"kernelspec":{"name":"python3"}}});
        assert_eq!(resolve_cell_language(&cell, &root), Some(Language::Javascript));
    }

    #[test]
    fn falls_back_to_notebook_kernelspec_then_language_info() {
        let cell = json!({"cell_type":"code","metadata":{}});
        let root = json!({"metadata":{"kernelspec":{"name":"python3"}}});
        assert_eq!(resolve_cell_language(&cell, &root), Some(Language::Python));

        let root2 = json!({"metadata":{"language_info":{"name":"rust"}}});
        assert_eq!(resolve_cell_language(&cell, &root2), Some(Language::Rust));
    }

    #[test]
    fn unknown_and_unsupported_languages_resolve_to_none() {
        let root = json!({"metadata":{}});
        for kernel in ["go", "gonb", "julia", "r", "ir", "haskell"] {
            let cell = json!({"cell_type":"code","metadata":{"spur":{"code_type":kernel}}});
            assert_eq!(resolve_cell_language(&cell, &root), None, "{kernel}");
        }
    }

    #[test]
    fn detects_markdown_cells() {
        assert!(cell_is_markdown(&json!({"cell_type":"markdown"})));
        assert!(!cell_is_markdown(&json!({"cell_type":"code"})));
    }
}
```

- [ ] **Step 2:** `scripts/spur-cargo test -p spur-graph notebook` → FAIL

- [ ] **Step 3: Implement** (`notebook.rs`):

```rust
use serde_json::Value;

use crate::extract::languages::Language;

/// Map a Jupyter/SPUR language token to a spur-graph grammar, or None if no
/// grammar exists for it (go/julia/r are not yet supported — see plan scope note).
fn language_for_token(token: &str) -> Option<Language> {
    match token.to_ascii_lowercase().as_str() {
        "python" | "python3" => Some(Language::Python),
        "javascript" => Some(Language::Javascript),
        "rust" | "evcxr" => Some(Language::Rust),
        _ => None,
    }
}

pub(crate) fn cell_is_markdown(cell: &Value) -> bool {
    cell.get("cell_type").and_then(Value::as_str) == Some("markdown")
}

/// Resolve a code cell's language via the fallback chain:
/// cell spur.code_type -> cell kernelspec -> notebook kernelspec -> notebook language_info.
pub(crate) fn resolve_cell_language(cell: &Value, root: &Value) -> Option<Language> {
    let cell_meta = cell.get("metadata");
    let candidates = [
        cell_meta
            .and_then(|m| m.get("spur"))
            .and_then(|s| s.get("code_type"))
            .and_then(Value::as_str),
        cell_meta
            .and_then(|m| m.get("kernelspec"))
            .and_then(|k| k.get("name"))
            .and_then(Value::as_str),
        root.get("metadata")
            .and_then(|m| m.get("kernelspec"))
            .and_then(|k| k.get("name"))
            .and_then(Value::as_str),
        root.get("metadata")
            .and_then(|m| m.get("language_info"))
            .and_then(|l| l.get("name"))
            .and_then(Value::as_str),
    ];
    candidates.into_iter().flatten().find_map(language_for_token)
}
```

`mod.rs`: add `pub(crate) mod notebook;`.

- [ ] **Step 4:** `scripts/spur-cargo test -p spur-graph notebook` → PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/extract/notebook.rs crates/spur-graph/src/extract/mod.rs
git commit -m "feat(spur-graph): NB2 add cell language resolution fallback chain"
```

---

### Task 3: `extract_notebook_file` — programmatic per-cell extraction

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-graph/src/extract/notebook.rs` (add `extract_notebook_file` + a per-cell delegation helper + a fixture test)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] `extract_notebook_file` parses `.ipynb` JSON, adds a file node for the notebook, and for each cell emits symbols as `Contains` children of that file node
- [ ] Python code cells extract `def`/`class` symbols; JS code cells extract function/class symbols; markdown cells extract section headings
- [ ] Unknown-language code cells are **skipped with a `tracing::warn!`**, not a panic
- [ ] Malformed JSON returns an `anyhow::Error` (no panic)
- [ ] A fixture test builds a 2-cell notebook (one Python `def`, one markdown heading) and asserts both appear as nodes contained under the `.ipynb` file node
- [ ] `scripts/spur-cargo test -p spur-graph notebook` passes

**Suggested Worker:** claude-code-acp

> Routing note: this is the one task that requires judgment about reusing the `FactBuilder`/`extract_file_from_tree` machinery per cell rather than a mechanical edit — routed to claude-code-acp despite the epic's codex default. The brain confirms routing at dispatch.

**Scope Boundary:**
- IN scope: `notebook.rs` only
- OUT of scope: `tree_sitter.rs` (task-4 wires dispatch), `languages.rs`. You may **call** existing `pub(crate)` items from `tree_sitter.rs` (e.g. `extract_file_from_tree`, `FactBuilder`, `add_file_node`) but must not modify them. If you find they are not `pub(crate)`-visible to `notebook.rs`, emit `scope_drift` (task-4 would need to widen visibility) rather than editing them here.

**Scope Drift Checkpoint:**
- If reusing `extract_file_from_tree` per cell requires changing its signature or visibility → emit `scope_drift` immediately (do not refactor it inside this task).
- If cell-relative vs notebook-relative byte offsets force a change to `FactBuilder` span handling → emit `risk`.

**Implementation:**

- [ ] **Step 1: Failing fixture test** (`notebook.rs` tests):

```rust
#[test]
fn extracts_python_def_and_markdown_heading_as_contained_children() {
    let nb = json!({
        "nbformat": 4, "nbformat_minor": 5,
        "metadata": {"kernelspec": {"name": "python3"}},
        "cells": [
            {"cell_type": "code", "metadata": {}, "source": ["def load_df():\n", "    return 1\n"]},
            {"cell_type": "markdown", "metadata": {}, "source": ["# Analysis\n"]}
        ]
    });
    let bytes = serde_json::to_vec(&nb).unwrap();
    let facts = run_notebook_extraction(std::path::Path::new("nb.ipynb"), &bytes)
        .expect("notebook extracts");
    // helper asserts: a node named "load_df" exists, a section node "Analysis" exists,
    // and both are Contains-children of the nb.ipynb file node.
    assert!(facts.has_symbol("load_df"));
    assert!(facts.has_section("Analysis"));
    assert!(facts.all_symbols_contained_by_file("nb.ipynb"));
}
```

  (`run_notebook_extraction` + the `has_symbol`/`has_section`/`all_symbols_contained_by_file` assertions are test helpers you write in the `notebook.rs` test module, constructing a `FactBuilder` rooted at a tempdir and inspecting the produced `GraphFacts`. Mirror how existing `tree_sitter.rs` tests inspect `builder` output — e.g. the tests near `builder.add_edge(file, Some(source), RelationKind::Contains, None)`.)

- [ ] **Step 2:** `scripts/spur-cargo test -p spur-graph notebook` → FAIL

- [ ] **Step 3: Implement** `extract_notebook_file(builder, path, bytes)`:
  - `let nb: Value = serde_json::from_slice(bytes).context("parse .ipynb JSON")?;`
  - Compute `relative_path` + `let file_node = builder.add_file_node(&relative_path, FileId(builder.next_file_id()), …)` following the exact opening of `extract_markdown_file` (`markdown.rs:15+`).
  - Iterate `nb.get("cells").and_then(Value::as_array)`:
    - Join `cell["source"]` (array-of-strings or string) into one `String` via a `cell_source_text` helper.
    - If `cell_is_markdown(cell)` → run the markdown extraction on the cell text (reuse the markdown grammar path) parented to `file_node`.
    - Else `match resolve_cell_language(cell, &nb)`:
      - `Some(lang)` → build a per-cell `BytesExtractor::for_language(lang)` (or call `extract_file_from_tree` with that lang's config + a freshly parsed tree of the cell source), emitting nodes parented under `file_node` via `Contains`.
      - `None` → `tracing::warn!(?path, "skipping notebook cell: unsupported language");` and continue.
  - Symbol byte ranges are cell-relative (per spec); do not attempt to remap to whole-file offsets in v1.
  - Return `anyhow::Result<()>`.
  - **Reuse, do not reimplement:** the actual symbol emission per cell must call the existing per-language extraction (`extract_file_from_tree` in `tree_sitter.rs`) so cell symbols and intra-cell edges come from the real grammar. If a thin wrapper is needed to parse a cell string into a tree for a given `Language`, add it **inside notebook.rs** using `tree_sitter::Parser` + `lang.tree_sitter_language()`.

- [ ] **Step 4:** `scripts/spur-cargo test -p spur-graph notebook` → PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/extract/notebook.rs
git commit -m "feat(spur-graph): NB3 extract per-cell symbols from .ipynb as contained children"
```

---

### Task 4: Wire dispatch + gate + docs

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (`BytesExtractor::new` notebook special-case, `extract_graph_facts` routing, `symbol_query_policy`, and the gate test `every_registered_language_satisfies_query_contract` / `symbol_query_policy_documents_shared_and_dedicated_sources`)
- Modify: `crates/spur-graph/queries/README.md` (add a JupyterNotebook coverage row)

**Depends on:** task-3

**Acceptance Criteria:**
- [ ] `BytesExtractor::new` does not fail for `Language::JupyterNotebook` (skips single-grammar `set_language`/`compile_queries`, mirroring how Markdown's inline parser is conditionally built)
- [ ] `extract_graph_facts` routes `Language::JupyterNotebook` to `notebook::extract_notebook_file` (a branch beside the existing `if self.language == Language::Markdown`)
- [ ] `symbol_query_policy` returns a non-panicking policy for `JupyterNotebook` (no dedicated `.scm`; container emits no direct symbols)
- [ ] The registered-language gate test passes with `JupyterNotebook` treated as a container language (no `@definition.*` query contract required)
- [ ] `queries/README.md` documents JupyterNotebook coverage as `contains` only
- [ ] An end-to-end test runs `BytesExtractor::for_language(Language::JupyterNotebook)` + `extract_graph_facts` on a 2-cell fixture and asserts symbols appear (or reuses task-3's fixture through the public path)
- [ ] `scripts/spur-cargo test -p spur-graph` is green (whole crate)

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two files above
- OUT of scope: `notebook.rs` (consume task-3's `extract_notebook_file` as-is), `languages.rs`
- If `extract_notebook_file` needs a different signature than task-3 provided, emit `scope_drift` rather than editing `notebook.rs`.

**Implementation:**

- [ ] **Step 1: Failing test** — extend the gate test region in `tree_sitter.rs`. First run the existing gate to see it fail on the unhandled variant:

Run: `scripts/spur-cargo test -p spur-graph every_registered_language_satisfies_query_contract`
Expected: FAIL (JupyterNotebook not handled / panics in `symbol_query_policy`)

- [ ] **Step 2: Implement dispatch.** In `BytesExtractor::new` (`tree_sitter.rs:203`), special-case before `set_language`:

```rust
if language == Language::JupyterNotebook {
    // Container format: no single grammar. Parsing + per-cell sub-extraction
    // happens in extract_notebook_file; build an inert extractor.
    return Ok(Self {
        language,
        config,
        parser: Parser::new(),
        queries: CompiledQueries::empty(), // or the existing "no queries" constructor
        markdown_inline_parser: None,
    });
}
```
  (Use whatever the crate's existing empty/no-op `CompiledQueries` constructor is; if none exists, compile against the placeholder markdown grammar from task-1's `jupyter_notebook_config` — it is never used because the next change bypasses `self.parser`.)

  In `extract_graph_facts` (`tree_sitter.rs:247`), add a branch **before** the `self.parser.parse(...)` call so JSON is never fed to a code grammar:

```rust
if self.language == Language::JupyterNotebook {
    return crate::extract::notebook::extract_notebook_file(builder, path, bytes)
        .map_err(|err| ExtractError::Extraction(err.to_string()));
}
```

  In `symbol_query_policy` (`tree_sitter.rs:91`), add `Language::JupyterNotebook => SymbolQueryPolicy::Dedicated(...)` only if a non-empty `.scm` is required; otherwise extend the `ReuseTags`/container arm so the match is exhaustive without claiming query coverage. Pick whichever keeps the gate test honest (container emits no direct symbols).

- [ ] **Step 3: Update the gate test** so `JupyterNotebook` is asserted as a container language (extension `ipynb`, `contains` relation only, no `@definition.*` requirement), per the spec's Gate Contract.

- [ ] **Step 4: Run** `scripts/spur-cargo test -p spur-graph` → whole crate PASS. Update `queries/README.md` with the coverage row.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/queries/README.md
git commit -m "feat(spur-graph): NB4 route .ipynb extraction and satisfy language gate"
```

---

## Dependency DAG

```
task-1 ──> task-2 ──> task-3 ──> task-4
```

A deliberately linear chain: each task depends on the prior's types/functions (registry → resolution → extraction → dispatch). No parallelism is available because all four touch the same small extraction surface and would otherwise collide on `notebook.rs` / `languages.rs` / `tree_sitter.rs`.

## Out of Scope (follow-up)

- **Go/Julia/R cell grammars** — not in spur-graph's `Language` enum; cells in those languages are skipped with a warning. Adding those grammars is a separate change.
- **Cell-level container nodes** as an explicit `NodeKind::Section` tier between file and symbols — the spec lists this as future work; v1 parents cell symbols directly under the file node via `Contains`.
- **Output extraction / incremental re-extraction** — spec future work.
- **The context-provider spur-semantic fact layer (CellFacts) + `notebook_symbol_*` tools (slice 3)** — this plan unblocks that; it ships as its own epic afterward.

## Test Plan

- task-1: `Language::from_path("x.ipynb")` resolves; crate compiles with exhaustive matches.
- task-2: fallback-chain precedence, supported→variant mapping, unsupported→None, markdown detection.
- task-3: 2-cell fixture (Python def + markdown heading) → both contained under the file node; unknown-language cell skipped (warn, no panic); malformed JSON → Err.
- task-4: gate test treats JupyterNotebook as container; whole-crate `-p spur-graph` green; end-to-end extraction through `BytesExtractor` public path.

## Risks

**Per-cell `FactBuilder` reuse (task-3).** Reusing `extract_file_from_tree` per cell is the one non-mechanical step; if it requires signature/visibility changes the worker must signal rather than refactor across task boundaries. This is why task-3 is routed to claude-code-acp and carries explicit scope-drift/risk checkpoints.

**Placeholder grammar for the container.** `JupyterNotebook` carries an inert markdown grammar so `match` arms stay exhaustive; the dispatch bypass in task-4 ensures it is never actually used to parse JSON. If any code path parses an `.ipynb` through `self.parser` it would silently produce garbage — task-4's `extract_graph_facts` branch ordering (before `parse`) is the guard, and the end-to-end test is the check.

## Acceptance Criteria (epic)

- `.ipynb` files are discovered by extension and indexed by `spur-graph`.
- Python and JS code cells contribute symbols; markdown cells contribute headings; unknown-language cells are skipped without crashing.
- The language gate test passes with `JupyterNotebook` as a registered container language.
- Whole-crate `scripts/spur-cargo test -p spur-graph` is green.
- Context-provider slice 3 is unblocked (can extract against notebook symbols).
