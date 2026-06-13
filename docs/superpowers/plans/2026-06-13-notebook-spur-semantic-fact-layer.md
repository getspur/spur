# Notebook Spur-Semantic Fact Layer + Symbol Tools Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-ai-sidebar-context-provider-design.md` (§6, §7 slice-3 portions, §11 slice 3)
**Companion spec:** `docs/superpowers/specs/2026-06-10-spur-graph-jupyter-notebook-support-design.md` (foundation — already landed)
**Design epic:** `bd-f1ab` (closed)

**Goal:** Extend the landed spur-graph `.ipynb` extractor from generic-symbol-only to a spur-semantic fact layer (cell container nodes, port I/O, declared DAG, frontend bindings, table references), then expose it live in the notebook daemon via a delta-driven in-memory index and two new MCP tools (`notebook_symbol_search`, `notebook_symbol_refs`).

**Architecture:** spur-graph stays a pure library (no new MCP server). It gains new graph-schema variants and programmatic fact emission inside `extract/notebook.rs`. The notebook daemon links spur-graph, maintains a per-notebook live fact index rebuilt on save/run boundaries (debounced, per-cell blake3 hash), and serves symbol queries from it. Drift and `scope:"used"` are **derived at query time** (trust invariant §10), not stored.

**Tech Stack:** Rust 2021, tree-sitter (python + typescript/javascript grammars already shipped), `serde_json`, `blake3` (already a spur-notebook dep), `tokio::sync::broadcast`, `rmcp`.

**Build/test:** Always `scripts/spur-cargo` (never bare cargo). spur-graph tests: `scripts/spur-cargo test -p spur-graph`. Daemon: `scripts/spur-cargo test -p spur-notebook`. Lint from sandbox: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -p spur-notebook -- -D warnings`.

---

## Schema decisions (shared context for all tasks — read first)

These are fixed for the whole epic so later tasks use consistent vocabulary:

| Fact (spec §6 table) | Graph encoding |
|---|---|
| Cell container node | `NodeKind::Cell`, one per nbformat cell, `Contains` edge file→cell; cell symbols re-parented under the cell node |
| Arrow port | `NodeKind::Port`, one per distinct port name |
| Declared produces / consumes | `RelationKind::Produces` / `RelationKind::Consumes`, cell→port, `bind_method: Some("declared")` |
| Actual produces / consumes (`spur.put`/`spur.get`) | same `Produces`/`Consumes`, cell→port, `bind_method: Some("actual")` |
| Declared source (`dag.source`) | `RelationKind::References`, cell→external(`ds://…`), `bind_method: Some("declared")` |
| Frontend binds / emits | `RelationKind::Binds` / `RelationKind::Emits`, cell→port |
| Table reference (table-fn call in source) | `RelationKind::References`, cell→external(`ds://…`), `bind_method: Some("actual")` |
| Drift | **derived at query time** in `notebook_symbol_refs` by comparing declared vs actual port edges; NOT a stored annotation (trust invariant: derived-not-stored) |

- Datasource (`ds://…`) targets are `NodeKind::External` nodes whose `label`/`target_label` is the `ds://` ref — the graph does not own the datasource catalog (the daemon does), so a lightweight external target is correct.
- Provenance (declared vs actual) is carried in the existing `GraphEdge.bind_method: Option<String>` field — **no new struct field is added**.
- `cell://` and `port://` ref strings used as node `label`/`stable_key` follow `crates/spur-notebook/src/context/refs.rs` formatting: `cell://<cell-id>`, `port://<name>` (no notebook prefix inside the single-notebook graph file; the daemon adds notebook scoping when it maps to `sym://`).

---

## Task 1: Schema — notebook node & relation kinds

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-graph/src/schema.rs` (`NodeKind` enum + `discriminator()`; `RelationKind` enum + `metadata()`)
- Modify: `crates/spur-graph/src/extract/languages.rs` (`mod gate_contract` — the `relation_coverage_matches_declared_contract` expectation for `JupyterNotebook`, ~lines 2117-2119)
- Modify: `crates/spur-graph/src/store/build.rs` (`buckets_from_facts` node-kind match, ~lines 602-617)
- Modify: `crates/spur-graph/queries/README.md` (document new relations + the `spur-notebook-facts` query type that task-4 adds)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `NodeKind` has `Cell` and `Port` variants; `discriminator()` returns `"cell"` / `"port"`.
- [ ] `RelationKind` has `Produces`, `Consumes`, `Binds`, `Emits` variants; `metadata()` returns sensible inverse labels (`produced_by`, `consumed_by`, `bound_by`, `emitted_by`), all `ManyToMany`, all `transitive: false`.
- [ ] `buckets_from_facts` compiles (exhaustive match handles `Cell`/`Port`) and routes `Cell`/`Port` nodes into the symbol bucket so they land in `nodes.parquet`.
- [ ] The `gate_contract` tests (`every_registered_language_satisfies_query_contract`, `relation_coverage_matches_declared_contract`, `ipynb_path_resolves_to_jupyter_notebook`) all pass: `JupyterNotebook`'s declared relation set is expanded to `{contains, produces, consumes, binds, emits, references}`.
- [ ] New serde round-trip test asserts each new `NodeKind`/`RelationKind` variant serializes to its expected snake_case string and back.
- [ ] `scripts/spur-cargo test -p spur-graph` is green; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings` is clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the four files above; schema variants, their exhaustive-match arms, the gate-contract expectation, the analyst bucket arm, README docs, and round-trip tests.
- OUT of scope: any emission of these variants (that is task-2/3/4), `extract/notebook.rs`, the daemon crate.
- Do **not** emit a `scope_drift` signal. This is a single cohesive schema task; the four files are all the mechanical fan-out of adding enum variants. If a gate test in `languages.rs` references the new variants, updating it is IN scope.

**Implementation:**

- [ ] **Step 1: Write the failing serde round-trip test** in `crates/spur-graph/src/schema.rs` under the existing `#[cfg(test)] mod tests` (or add one):

```rust
#[test]
fn notebook_node_and_relation_kinds_roundtrip() {
    use serde_json::json;
    for (kind, disc) in [(NodeKind::Cell, "cell"), (NodeKind::Port, "port")] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(disc));
        assert_eq!(kind.discriminator(), disc);
        let back: NodeKind = serde_json::from_value(json!(disc)).unwrap();
        assert_eq!(back, kind);
    }
    for (rel, disc) in [
        (RelationKind::Produces, "produces"),
        (RelationKind::Consumes, "consumes"),
        (RelationKind::Binds, "binds"),
        (RelationKind::Emits, "emits"),
    ] {
        assert_eq!(serde_json::to_value(rel).unwrap(), json!(disc));
        let back: RelationKind = serde_json::from_value(json!(disc)).unwrap();
        assert_eq!(back, rel);
        // metadata() must not panic and must declare an inverse label.
        assert!(rel.metadata().inverse_label.is_some());
    }
}
```

- [ ] **Step 2: Run to verify it fails to compile** (`Cell`/`Port`/`Produces`… undefined):

Run: `scripts/spur-cargo test -p spur-graph notebook_node_and_relation_kinds_roundtrip`
Expected: FAIL (unknown variants).

- [ ] **Step 3: Add the variants.** In `schema.rs` `NodeKind` add `Cell,` and `Port,` after `McpTool`. Add to `discriminator()`:

```rust
Self::Cell => "cell",
Self::Port => "port",
```

In `RelationKind` add `Produces,`, `Consumes,`, `Binds,`, `Emits,` after `Touches`. Add to `metadata()`:

```rust
Self::Produces => RelationMetadata { inverse_label: Some("produced_by"), cardinality: ManyToMany, transitive: false },
Self::Consumes => RelationMetadata { inverse_label: Some("consumed_by"), cardinality: ManyToMany, transitive: false },
Self::Binds    => RelationMetadata { inverse_label: Some("bound_by"),    cardinality: ManyToMany, transitive: false },
Self::Emits    => RelationMetadata { inverse_label: Some("emitted_by"),  cardinality: ManyToMany, transitive: false },
```

- [ ] **Step 4: Fix the exhaustive matches the compiler now flags.**
  - `store/build.rs` `buckets_from_facts`: add `NodeKind::Cell | NodeKind::Port` to the same arm that handles `Function | Class | … | Section | McpTool` so they land in the symbol/nodes bucket.
  - `languages.rs` `gate_contract`: update the hardcoded `JupyterNotebook` relation expectation (currently `{"contains"}`) to `{"contains", "produces", "consumes", "binds", "emits", "references"}`. Read the surrounding assertion to match its exact set type/format.

- [ ] **Step 5: Document** in `queries/README.md`: a short subsection "Notebook semantic facts" listing the new relations and noting that task-4 adds `queries/python/spur-notebook-facts.scm` and `queries/typescript/spur-notebook-facts.scm` registered in `MANIFEST_QUERY_BYTES`.

- [ ] **Step 6: Run the full crate suite + clippy.**

Run: `scripts/spur-cargo test -p spur-graph`
Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-graph/src/schema.rs crates/spur-graph/src/extract/languages.rs crates/spur-graph/src/store/build.rs crates/spur-graph/queries/README.md
git commit -m "feat(spur-graph): task-1 add notebook cell/port nodes and dataflow relations"
```

---

## Task 2: Cell container nodes + re-parent symbols + public facts entry

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-graph/src/extract/notebook.rs` (`extract_notebook_file`, `extract_cell`; add a per-cell `NodeKind::Cell` node; re-parent cell symbols under it)
- Modify: `crates/spur-graph/src/extract/mod.rs` or `src/lib.rs` (expose a `pub fn extract_notebook_facts(root: &Path, path: &Path, bytes: &[u8]) -> anyhow::Result<GraphFacts>` library entry the daemon can call on in-memory bytes)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] Each nbformat cell produces exactly one `NodeKind::Cell` node, labeled with its `cell://<cell-id>` ref, `Contains` edge from the file node to the cell node.
- [ ] Symbols extracted from a cell are `Contains`-linked from that cell's node (not directly from the file node).
- [ ] The existing containment test is updated from "file contains every symbol" to "file → cell → symbol" transitive containment (e.g. `all_non_file_nodes_reachable_from_file_via_contains`).
- [ ] A new fixture asserts: a 2-code-cell notebook yields 2 `Cell` nodes whose labels are the cells' `cell://` refs.
- [ ] `extract_notebook_facts(root, path, bytes)` is public, returns the same `GraphFacts` the batch path produces for that notebook (assert batch-vs-direct agreement in a test).
- [ ] `scripts/spur-cargo test -p spur-graph` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `extract/notebook.rs` and one re-export site for the public entry.
- OUT of scope: port/dag/frontend/table facts (task-3, task-4), the daemon crate, schema variants (task-1 owns those).
- Do **not** emit a `scope_drift` signal. Cell-node introduction + symbol re-parenting + the public entry are one cohesive change to the notebook extractor.

**Implementation:**

- [ ] **Step 1: Write the failing fixture test** in `notebook.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn each_cell_gets_a_cell_container_node() {
    let nb = serde_json::to_vec(&serde_json::json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [
            {"cell_type":"code","id":"a3f1","source":["def load():\n"," pass\n"],"metadata":{},"outputs":[],"execution_count":null},
            {"cell_type":"code","id":"b2c9","source":["x = load()\n"],"metadata":{},"outputs":[],"execution_count":null}
        ]
    })).unwrap();
    let mut builder = FactBuilder::new(Path::new("/nb"));
    extract_notebook_file(&mut builder, Path::new("/nb/app.ipynb"), &nb).unwrap();
    let facts = builder.into_facts();
    let cell_nodes: Vec<_> = facts.nodes.iter().filter(|n| n.kind == NodeKind::Cell).collect();
    assert_eq!(cell_nodes.len(), 2);
    let labels: Vec<&str> = cell_nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(labels.contains(&"cell://a3f1"));
    assert!(labels.contains(&"cell://b2c9"));
}
```

- [ ] **Step 2: Run to verify it fails.**

Run: `scripts/spur-cargo test -p spur-graph each_cell_gets_a_cell_container_node`
Expected: FAIL (0 cell nodes).

- [ ] **Step 3: Implement.** In `extract_cell`, before extracting symbols, add the cell node and pass its `NodeId` down so symbol containment edges target it:

```rust
let cell_label = format!("cell://{cell_id}");
let cell_node = builder.add_node_with_range(
    relative_path,
    cell_label.clone(),
    cell_label,                 // fqn == cell ref
    NodeKind::Cell,
    file_id,
    /* range covering the cell source */ source_range,
);
builder.add_edge(file_node, Some(cell_node), RelationKind::Contains, None);
```

Then change the symbol-extraction calls so symbols are `Contains`-linked from `cell_node` rather than `file_node`. `extract_notebook_file` must thread each cell's id into `extract_cell` (read `cell["id"]`; fall back to a stable index-based id `format!("cell-{idx}")` when absent — document this fallback).

- [ ] **Step 4: Add the public library entry.** In `extract/mod.rs` (or `lib.rs`), expose:

```rust
pub fn extract_notebook_facts(
    root: &std::path::Path,
    path: &std::path::Path,
    bytes: &[u8],
) -> anyhow::Result<crate::extract::GraphFacts> {
    let mut builder = crate::extract::tree_sitter::FactBuilder::new(root);
    crate::extract::notebook::extract_notebook_file(&mut builder, path, bytes)?;
    Ok(builder.into_facts())
}
```

(`into_facts` is currently `#[cfg(test)]` — promote it to `pub(crate)`/`pub` as needed for this entry; keep it minimal.)

- [ ] **Step 5: Update the containment test** to expect file→cell→symbol and add a batch-vs-direct agreement test using `extract_notebook_facts`.

- [ ] **Step 6: Run + clippy + commit.**

Run: `scripts/spur-cargo test -p spur-graph`
Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings`

```bash
git add crates/spur-graph/src/extract/notebook.rs crates/spur-graph/src/extract/mod.rs
git commit -m "feat(spur-graph): task-2 cell container nodes and public notebook facts entry"
```

---

## Task 3: Declared facts from cell metadata (dag + frontend)

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-graph/src/extract/notebook.rs` (read `cell.metadata.spur.dag` and `cell.metadata.spur.frontend` from the cell JSON; emit Port nodes + declared edges)

**Depends on:** task-2

**Acceptance Criteria:**
- [ ] For a cell with `metadata.spur.dag.produces=[{port:"sales"}]`, emit a `NodeKind::Port` node `port://sales` and a `Produces` edge cell→port with `bind_method: Some("declared")`.
- [ ] `dag.consumes=["raw"]` → `Consumes` edge cell→`port://raw`, declared.
- [ ] `dag.source={kind, port}` → `References` edge cell→`external(ds://<kind>/<port>)`, declared. (Use the same `ds://` shape the daemon catalog uses; document the exact format chosen.)
- [ ] `metadata.spur.frontend.binds=["risk"]` → `Binds` edge cell→`port://risk`; `frontend.emits=["horizon"]` → `Emits` edge cell→`port://horizon`.
- [ ] Port nodes are de-duplicated by name (one `port://sales` node even if produced by one cell and consumed by another).
- [ ] Fixtures cover declared dag (produces/consumes/source) and frontend (binds/emits).
- [ ] `scripts/spur-cargo test -p spur-graph` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `extract/notebook.rs` metadata reading + declared-edge emission only.
- OUT of scope: source-text parsing / tree-sitter spur-fact queries (`spur.put`/`spur.get`, table-fn calls) — that is task-4. Schema variants (task-1). Daemon crate.
- Do **not** emit a `scope_drift` signal. Reading two metadata sub-objects and emitting their edges is one cohesive unit.

**Implementation:**

- [ ] **Step 1: Write the failing fixture** in `notebook.rs` tests:

```rust
#[test]
fn declared_dag_and_frontend_facts_emitted() {
    let nb = serde_json::to_vec(&serde_json::json!({
        "nbformat":4,"nbformat_minor":5,"metadata":{},
        "cells":[{
            "cell_type":"code","id":"a3f1","source":["pass\n"],"outputs":[],"execution_count":null,
            "metadata":{"spur":{"version":7,
                "dag":{"produces":[{"port":"sales","repr":"arrow"}],"consumes":["raw"],
                        "source":{"kind":"csv","port":"raw"}},
                "frontend":{"binds":["risk"],"emits":["horizon"]}}}
        }]
    })).unwrap();
    let mut b = FactBuilder::new(Path::new("/nb"));
    extract_notebook_file(&mut b, Path::new("/nb/app.ipynb"), &nb).unwrap();
    let f = b.into_facts();
    let has = |rel, label: &str, bm: Option<&str>| f.edges_to_labels().iter().any(|e|
        e.relation == rel && e.target_label.as_deref() == Some(label)
        && e.bind_method.as_deref() == bm);
    assert!(has(RelationKind::Produces, "port://sales", Some("declared")));
    assert!(has(RelationKind::Consumes, "port://raw", Some("declared")));
    assert!(has(RelationKind::Binds, "port://risk", None));
    assert!(has(RelationKind::Emits, "port://horizon", None));
}
```

> Note: `edges_to_labels()` is illustrative — use whatever the existing tests use to inspect emitted edges/`GraphFacts`; if no helper exists, iterate `f.edges` and resolve targets via `f.nodes`. Match the existing test idiom in this file.

- [ ] **Step 2: Run to verify it fails.**

- [ ] **Step 3: Implement.** After the cell node is created (task-2), read the cell's `metadata.spur` JSON object:
  - `dag.produces[].port` → `intern_port(builder, name)` (a helper that returns/creates the deduped `port://<name>` node) + `Produces` edge (declared).
  - `dag.consumes[]` (strings) → `Consumes` edges (declared).
  - `dag.source` `{kind, port}` → `References` edge to `external(format!("ds://{kind}/{port}"))` (declared). Use `add_edge` with `target_label` set (unresolved external — no target node id, mirroring how unresolved externals are emitted elsewhere) or create a `NodeKind::External` node; match the existing external-emission pattern in `tree_sitter.rs`.
  - `frontend.binds[]` → `Binds` edges; `frontend.emits[]` → `Emits` edges.
  - Provenance: set `bind_method: Some("declared")` for produces/consumes/source via `add_edge_with_kind`-style construction (set the field on the pushed edge). If no public setter exists, extend the builder minimally to accept `bind_method` for these edges, or set it on the `GraphEdge`/`PendingEdge` directly.

- [ ] **Step 4: Run + clippy + commit.**

```bash
git add crates/spur-graph/src/extract/notebook.rs
git commit -m "feat(spur-graph): task-3 declared dag and frontend port facts"
```

---

## Task 4: Actual facts from source + spur-fact query patterns (python + javascript)

**Task ID:** `task-4`

**Files:**
- Create: `crates/spur-graph/queries/python/spur-notebook-facts.scm`
- Create: `crates/spur-graph/queries/typescript/spur-notebook-facts.scm`
- Modify: `crates/spur-graph/src/store/build.rs` (`MANIFEST_QUERY_BYTES`: register both new query files)
- Modify: `crates/spur-graph/src/extract/notebook.rs` (run the spur-fact query over each cell's parsed tree; emit actual port edges + table-reference edges)

**Depends on:** task-3

**Acceptance Criteria:**
- [ ] Python cell `spur.put("sales", df)` → `Produces` edge cell→`port://sales`, `bind_method: Some("actual")`. `spur.get("raw")` → `Consumes` edge, actual.
- [ ] JavaScript/TS cell `spur.get("sales")` (Deno app cell) → `Consumes` edge, actual.
- [ ] A table-function call matching a `ds://` invoke name in source → `References` edge cell→`external(ds://…)`, `bind_method: Some("actual")`. (For this slice, recognize a call whose function name matches `<datasource>_<table>` pattern; the daemon supplies the authoritative catalog match in task-6 — here, emit the raw call name as `external` target `ds://<call_name>` so the daemon can reconcile.)
- [ ] Non-literal port arg (`spur.put(name_var, df)`) emits an `external` marker `opaque_put` rather than a port edge (spec §14 risk) — covered by a fixture.
- [ ] Cross-slot match works at the data plane: a Python `spur.put("x")` and a JS `spur.get("x")` both reference the same `port://x` node (fixture with a python cell + a javascript cell).
- [ ] Both `.scm` files are registered in `MANIFEST_QUERY_BYTES` (under `language: "python"` and `language: "typescript"`), so `current_manifest_version()` accounts for them.
- [ ] `scripts/spur-cargo test -p spur-graph` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two `.scm` files, their manifest registration, and the source-fact extraction in `notebook.rs`.
- OUT of scope: drift computation (derived in the daemon, task-6), the daemon crate, schema variants.
- Do **not** emit a `scope_drift` signal. The query files + their wiring + the extraction call are one cohesive feature.

**Implementation:**

- [ ] **Step 1: Write the query patterns.** `queries/python/spur-notebook-facts.scm` (capture `spur.put`/`spur.get` calls with a string-literal first arg):

```scheme
; spur.put("name", value) — actual produce
(call
  function: (attribute object: (identifier) @_obj attribute: (identifier) @_method)
  arguments: (argument_list . (string) @port.name)
  (#eq? @_obj "spur")
  (#eq? @_method "put")) @port.produce

; spur.get("name") — actual consume
(call
  function: (attribute object: (identifier) @_obj2 attribute: (identifier) @_method2)
  arguments: (argument_list . (string) @port.get.name)
  (#eq? @_obj2 "spur")
  (#eq? @_method2 "get")) @port.consume

; bare table-function call: name(...) — candidate ds reference
(call function: (identifier) @table.call) @table.ref
```

`queries/typescript/spur-notebook-facts.scm` (mirror for JS/TS — `spur.put(...)`, `spur.get(...)`):

```scheme
(call_expression
  function: (member_expression object: (identifier) @_obj property: (property_identifier) @_method)
  arguments: (arguments . (string) @port.name)
  (#eq? @_obj "spur")
  (#eq? @_method "put")) @port.produce

(call_expression
  function: (member_expression object: (identifier) @_obj2 property: (property_identifier) @_method2)
  arguments: (arguments . (string) @port.get.name)
  (#eq? @_obj2 "spur")
  (#eq? @_method2 "get")) @port.consume

(call_expression function: (identifier) @table.call) @table.ref
```

> Adjust node-type names to the exact grammar in use (verify against the python / tsx grammars already vendored; the existing `queries/python/spur-edges.scm` is the reference for capture syntax and predicate style). The capture names above are the contract task-4's extractor code reads.

- [ ] **Step 2: Register in `MANIFEST_QUERY_BYTES`** (`store/build.rs`):

```rust
ManifestQueryBytes { language: "python",     query: "spur-notebook-facts", bytes: include_bytes!("../../queries/python/spur-notebook-facts.scm") },
ManifestQueryBytes { language: "typescript", query: "spur-notebook-facts", bytes: include_bytes!("../../queries/typescript/spur-notebook-facts.scm") },
```

- [ ] **Step 3: Write the failing fixture** in `notebook.rs` tests (python put/get + cross-slot js get + opaque):

```rust
#[test]
fn actual_port_facts_and_cross_slot_match() {
    let nb = serde_json::to_vec(&serde_json::json!({
        "nbformat":4,"nbformat_minor":5,"metadata":{"kernelspec":{"name":"python3"}},
        "cells":[
          {"cell_type":"code","id":"py","source":["spur.put(\"x\", df)\nspur.put(dyn_name, df)\n"],
           "outputs":[],"execution_count":null,"metadata":{}},
          {"cell_type":"code","id":"js","source":["const v = spur.get(\"x\");\n"],
           "outputs":[],"execution_count":null,"metadata":{"spur":{"code_type":"javascript"}}}
        ]
    })).unwrap();
    let mut b = FactBuilder::new(Path::new("/nb"));
    extract_notebook_file(&mut b, Path::new("/nb/app.ipynb"), &nb).unwrap();
    let f = b.into_facts();
    // one shared port node x; produce(actual) from py, consume(actual) from js
    assert_eq!(f.nodes.iter().filter(|n| n.kind==NodeKind::Port && n.label=="port://x").count(), 1);
    assert!(f.edges.iter().any(|e| e.relation==RelationKind::Produces && e.bind_method.as_deref()==Some("actual")));
    assert!(f.edges.iter().any(|e| e.relation==RelationKind::Consumes && e.bind_method.as_deref()==Some("actual")));
    // dynamic put → opaque marker, not a port edge
    assert!(f.edges.iter().any(|e| e.target_label.as_deref()==Some("opaque_put")));
}
```

- [ ] **Step 4: Implement** the source-fact pass in `extract_cell`: after parsing the cell tree, run the `spur-notebook-facts` query for the cell's language (python or javascript/typescript only; other languages skip this pass), iterate matches, and for `port.produce`/`port.consume` captures read the string-literal port name (strip quotes) → intern the shared `port://<name>` node + actual edge. When the port-arg capture is absent (non-literal), emit the `opaque_put` external marker. For `table.ref`, emit a `References` edge to `external(ds://<call_name>)`, actual. Reuse the `intern_port` helper from task-3.

- [ ] **Step 5: Run + clippy + commit.**

```bash
git add crates/spur-graph/queries/python/spur-notebook-facts.scm crates/spur-graph/queries/typescript/spur-notebook-facts.scm crates/spur-graph/src/store/build.rs crates/spur-graph/src/extract/notebook.rs
git commit -m "feat(spur-graph): task-4 actual port and table facts from cell source"
```

---

## Task 5: Daemon live fact index

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/Cargo.toml` (add `spur-graph = { workspace = true }`)
- Create: `crates/spur-notebook/src/context/symbol_index.rs` (the live per-notebook fact index + delta-driven updater)
- Modify: `crates/spur-notebook/src/context/mod.rs` (`pub mod symbol_index;`)
- Modify: the daemon startup path in `crates/spur-notebook/src/mcp/mod.rs` (spawn the updater task; store the index handle on `ServerDeps` or a shared `Arc` reachable from tools)

**Depends on:** task-4

**Acceptance Criteria:**
- [ ] An in-memory `SymbolIndex` keyed by notebook path holds the latest `GraphFacts` plus a per-cell `blake3` content-hash map.
- [ ] A background task subscribes via `state.subscribe_notebook_deltas()`, filters structural deltas (`Loaded | CellWritten | CellInserted | CellDeleted`), debounces ~150ms, and re-extracts the affected notebook by serializing its snapshot `NotebookRoot` to bytes (`serde_json::to_vec`) and calling `spur_graph::extract_notebook_facts`.
- [ ] Per-cell hash short-circuit: cells whose `blake3(source)` is unchanged are not re-extracted (assert via a test that an unrelated delta doesn't recompute an unchanged cell, or that the hash map is consulted).
- [ ] The index handle is reachable from MCP tool `call()` (added to `ServerDeps`, default `None` for standalone/test entry points, mirroring `daemon`/`state`).
- [ ] A unit test loads a 1-cell notebook into `State`, publishes a `Loaded`/`CellWritten` delta, waits for debounce, and asserts the index now contains the cell's symbols/ports.
- [ ] `scripts/spur-cargo test -p spur-notebook` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the Cargo dep, the new `symbol_index.rs`, its `mod.rs` registration, and the minimal `ServerDeps`/startup wiring to own and feed the index.
- OUT of scope: the two MCP query tools (task-6), spur-graph internals (tasks 1-4), the frontend.
- Do **not** emit a `scope_drift` signal. Linking the extractor + the index + the updater task + the deps field is one cohesive daemon feature. Adding the field to `ServerDeps` and its test constructors is expected mechanical fan-out, IN scope.

**Implementation:**

- [ ] **Step 1: Add the dependency.** In `crates/spur-notebook/Cargo.toml` `[dependencies]`: `spur-graph = { workspace = true }`. (spur-notebook already pulls duckdb via its existing introspection feature, so no new heavy transitive cost; if the default test build must stay light, gate behind a `symbol-index` feature and enable it in the daemon target — prefer unconditional unless the build time regresses.)

- [ ] **Step 2: Write the failing test** in `symbol_index.rs`:

```rust
#[tokio::test]
async fn index_updates_on_structural_delta() {
    let state = std::sync::Arc::new(jute::state::State::new());
    let index = SymbolIndex::shared();
    SymbolIndex::spawn_updater(index.clone(), state.clone());
    let path = std::path::PathBuf::from("/tmp/idx.ipynb");
    state.notebook_for_path(&path).load(&path, /* NotebookRoot w/ one code cell defining `load` */ test_root());
    // allow debounce + extraction
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let facts = index.facts_for(&path).expect("indexed");
    assert!(facts.nodes.iter().any(|n| n.label.contains("load")));
}
```

- [ ] **Step 3: Implement `SymbolIndex`:**

```rust
#[derive(Default)]
pub struct SymbolIndex {
    inner: dashmap::DashMap<PathBuf, IndexedNotebook>,
}
struct IndexedNotebook { facts: spur_graph::extract::GraphFacts, cell_hashes: HashMap<String, [u8;32]> }

impl SymbolIndex {
    pub fn shared() -> Arc<Self> { Arc::new(Self::default()) }
    pub fn facts_for(&self, path: &Path) -> Option<spur_graph::extract::GraphFacts> { /* clone */ }
    pub fn reindex(&self, path: &Path, root: &NotebookRoot) { /* hash cells; if any changed, serialize + extract_notebook_facts; store */ }
    pub fn spawn_updater(self: Arc<Self>, state: Arc<State>) {
        let mut rx = state.subscribe_notebook_deltas();
        tokio::spawn(async move {
            // debounce per path; on structural delta, snapshot state.notebook_for_path(path) and reindex
        });
    }
}
```

Use the structural-delta filter mirroring `dag/engine.rs::is_structural_delta`. Debounce with a `tokio::time::sleep` keyed per path (a simple `HashMap<PathBuf, Instant>` + 150ms window, or coalesce by draining the receiver). Hash each cell source with `blake3::hash(source.as_bytes()).into()`; skip extraction when all hashes match the stored map.

- [ ] **Step 4: Wire into startup + `ServerDeps`.** Add `pub symbol_index: Option<Arc<SymbolIndex>>` to `ServerDeps` (default `None` in the standalone/test constructors). In `start_daemon_server`, create `SymbolIndex::shared()`, call `spawn_updater`, and store it on the deps used by the server.

- [ ] **Step 5: Run + clippy + commit.**

```bash
git add crates/spur-notebook/Cargo.toml crates/spur-notebook/src/context/symbol_index.rs crates/spur-notebook/src/context/mod.rs crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): task-5 live notebook fact index via spur-graph extractor"
```

---

## Task 6: notebook_symbol_search / notebook_symbol_refs tools + scope:"used" upgrade

**Task ID:** `task-6`

**Files:**
- Create: `crates/spur-notebook/src/mcp/tools/notebook_symbol_search.rs`
- Create: `crates/spur-notebook/src/mcp/tools/notebook_symbol_refs.rs`
- Modify: `crates/spur-notebook/src/mcp/tools/mod.rs` (module decls + `tools()` list)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arms + server-instructions sentence naming the symbol tools)
- Modify: `crates/spur-notebook/src/context/catalog.rs` (`scope:"used"` table match: prefer the live index's table-reference facts when available, falling back to the literal string search)

**Depends on:** task-5

**Acceptance Criteria:**
- [ ] `notebook_symbol_search({notebook_path, query, kind?})` returns `{matches:[{ref:"sym://…", name, kind, lang, cell:"cell://…"}], next_queries}` served from the live index.
- [ ] `notebook_symbol_refs({notebook_path, ref})` accepts a `sym://` ref and returns `{defined_in, used_by:[cell refs], ports_touched, drift:[…], next_queries}` where `drift` is **derived** by comparing declared vs actual `Produces`/`Consumes` edges (declared port without matching actual, and inverse).
- [ ] Both tools are registered (module decls, `tools()` vec, dispatch arms) and `tools_include_direct_notebook_file_tools()`-style registry test updated.
- [ ] Server instructions mention `notebook_symbol_search`/`notebook_symbol_refs` as the slice-3 navigation step.
- [ ] `catalog.rs` `scope:"used"` uses index table facts when the index has the notebook; the existing literal-search test still passes (fallback path).
- [ ] `sym://` ↔ `graph://symbol/<id>` mapping is emitted where the batch index covers the file (best-effort; documented if the batch id is unavailable for loose notebooks).
- [ ] `scripts/spur-cargo test -p spur-notebook` green; clippy clean.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two tool files, their registration/dispatch, the server-instructions sentence, and the catalog `scope:"used"` upgrade.
- OUT of scope: the index implementation (task-5), spur-graph internals, the frontend.
- Do **not** emit a `scope_drift` signal. Two sibling query tools + their registration + the small catalog upgrade are one cohesive surface; updating the registry test is IN scope.

**Implementation:**

- [ ] **Step 1: Write the failing tool tests** modeled on `notebook_dag_status.rs`'s `mod tests` (build a `State`, load a notebook with declared+actual facts via the index, call `super::call`). Assert the search returns a `sym://` match and `notebook_symbol_refs` reports drift when a cell declares `produces:["sales"]` but its source has no `spur.put("sales")`.

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement** following the `notebook_context_pack.rs` `call()` template (deps→state/daemon→`current_path`→read index). Parse args as in `notebook_lineage.rs`. For `notebook_symbol_search`, filter `index.facts_for(path)` nodes by `kind`/substring `query`, format `sym://<cell-id>/<name>` refs (parse via `crate::context::refs::Ref`). For `notebook_symbol_refs`, parse the `sym://` ref (already supported in `refs.rs`), gather declared vs actual edges for the symbol's cell, compute `drift` as the set-difference, and return `ports_touched` from the cell's port edges. Always include `next_queries`.

- [ ] **Step 4: Register** in `mcp/tools/mod.rs` (`pub mod …; tools()` vec) and `mcp/mod.rs` (two dispatch arms before the `name =>` catch-all; extend the instructions string; update the registry assertion test).

- [ ] **Step 5: Upgrade `catalog.rs` `scope:"used"`** to consult the index's table-reference edges first, falling back to the literal `str::contains` search when the index lacks the notebook. Keep the existing slice-1 test green.

- [ ] **Step 6: Run + clippy + commit.**

```bash
git add crates/spur-notebook/src/mcp/tools/notebook_symbol_search.rs crates/spur-notebook/src/mcp/tools/notebook_symbol_refs.rs crates/spur-notebook/src/mcp/tools/mod.rs crates/spur-notebook/src/mcp/mod.rs crates/spur-notebook/src/context/catalog.rs
git commit -m "feat(spur-notebook): task-6 notebook_symbol tools and scope:used fact upgrade"
```

---

## Dependency DAG

```
task-1 (schema) ──> task-2 (cell nodes) ──> task-3 (declared facts) ──┐
                                       └────> task-4 (actual facts) ───┴─> task-5 (live index) ──> task-6 (symbol tools)
```

task-3 and task-4 both depend only on task-2 (and task-1 transitively) and may dispatch in parallel; task-5 depends on both. With a single worker they serialize naturally.

## Self-Review notes

- **Spec coverage:** §6 fact table → tasks 1-4; §6 "two execution modes / live" → task-5; §6 "Tools (slice 3)" → task-6; §4 "scope:used slice-3 upgrade" → task-6; §10 "drift derived, not stored" → task-6 (deliberate refinement vs the spec's "annotation on the cell node"; recorded in Schema decisions).
- **Type consistency:** `port://<name>`, `cell://<id>`, `ds://<kind>/<port>`, `sym://<cell-id>/<name>` used consistently; `bind_method` carries `"declared"`/`"actual"`; provenance never adds a struct field.
- **DAG:** acyclic, one parallel fork (task-3 ∥ task-4).
- **No standalone spur-graph MCP server** (spec §13 non-goal) — honored: spur-graph stays a library; tools are daemon-hosted.
