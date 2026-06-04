# Ontology Tier-0 Phase 3 — Bind `extends` for Python Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`
**Prior phases on main:** Rust `a9842865`/`c2e017bb`; TS/C++ `d22c689b`/`f94ec8a6`/`58ff8317`.

**Goal:** Bind `extends` for Python class inheritance, captures-only (the language-agnostic `emit_edges` `"extends"` arm already exists).

**Ontology decision:** Python has **no `implements` keyword** — `class C(Base):` is inheritance, which maps to **`extends`** (exactly like C++). Python `implements` stays **`TODO`** (a future Protocol/ABC-base heuristic is a Tier-3 inference, not syntactic Tier-0; do NOT attempt it here).

**Architecture:** `src/extract/languages.rs::emit_edges` is language-agnostic; its `"extends"` arm fires for any `@extends`/`@extends.name` capture. This adds a capture to `python/spur-edges.scm` + a test + one matrix cell. **No Rust changes.**

⚠️ **KNOWN PRE-EXISTING FAILURE — DO NOT CHASE:** the full `spur-graph` suite has a PRE-EXISTING, OUT-OF-SCOPE failure in `tests/incremental_ingest.rs` (2/3 fail on clean main), unrelated to this work. Do NOT run the whole-crate suite, do NOT fix it, do NOT emit `scope_drift` about it.

---

### Task py: Bind `extends` for Python + matrix cell

**Task ID:** `task-py`

**Files:**
- Modify: `crates/spur-graph/queries/python/spur-edges.scm` (append the `@extends` capture)
- Create: `crates/spur-graph/tests/python_inheritance_edges.rs`
- Modify: `crates/spur-graph/queries/README.md` (one matrix cell)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `class Derived(Base):` → a `RelationKind::Extends` edge from `Derived` with `target_label` `Base`.
- [ ] Multiple inheritance `class C(A, B):` → two extends edges (`A` and `B`).
- [ ] A class with no base (`class Plain:`) → no extends edge; `metaclass=`/keyword args are NOT captured as bases.
- [ ] Tests pass; `gate_contract` still passes; `clippy -D warnings` clean.
- [ ] README matrix `extends` row: Python cell `TODO` → `Y`.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the three files above. OUT: `languages.rs` (arm exists), `schema.rs`, `Cargo.toml`, other languages' files, `rust_*`/`ts_*`/`cpp_*` tests. Do NOT add an `@implements` capture for Python.

**Implementation:**

- [ ] **Step 1: Failing query-level test.** Create `crates/spur-graph/tests/python_inheritance_edges.rs`. Copy the `capture_texts` helper from `crates/spur-graph/tests/rust_implements_edge.rs`, changing the language to `tree_sitter_python::LANGUAGE`:

```rust
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};
const SPUR_EDGES_QUERY: &str = include_str!("../queries/python/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();
    while let Some((m, idx)) = captures.next() {
        let cap = m.captures[*idx];
        if capture_names[cap.index as usize] == capture_name {
            names.push(cap.node.utf8_text(source.as_bytes()).expect("text").to_owned());
        }
    }
    names
}

#[test]
fn py_captures_base_class() {
    let src = "class Base:\n    pass\nclass Derived(Base):\n    pass\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "extends.name").contains(&"Base".to_owned()));
}
#[test]
fn py_captures_multiple_bases() {
    let src = "class A:\n    pass\nclass B:\n    pass\nclass C(A, B):\n    pass\n";
    let names = capture_texts(SPUR_EDGES_QUERY, src, "extends.name");
    assert!(names.contains(&"A".to_owned()) && names.contains(&"B".to_owned()), "got {names:?}");
}
#[test]
fn py_plain_class_and_keyword_args_have_no_extends() {
    let plain = capture_texts(SPUR_EDGES_QUERY, "class Plain:\n    pass\n", "extends.name");
    assert!(plain.is_empty(), "got {plain:?}");
    // metaclass= is a keyword_argument, not a base — must not be captured.
    let kw = capture_texts(SPUR_EDGES_QUERY, "class M(metaclass=Meta):\n    pass\n", "extends.name");
    assert!(!kw.contains(&"Meta".to_owned()), "metaclass kwarg captured as base: {kw:?}");
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test python_inheritance_edges` → expect FAIL.

- [ ] **Step 2: Add the capture to `python/spur-edges.scm`.** Append; refine node names against the tree-sitter-python grammar until Step 1 passes (`class_definition` has a `superclasses: (argument_list …)` field; bases are `(identifier)` or dotted `(attribute attribute: (identifier))`; keyword args are `keyword_argument` and must be excluded):

```scheme
; `class C(Base):` inheritance. Python has no `implements` keyword, so a base
; class maps to `extends`. keyword_argument (e.g. metaclass=) is not matched.
(class_definition
  superclasses: (argument_list
    [(identifier) @extends.name
     (attribute attribute: (identifier) @extends.name)])) @extends
```

Run Step 1 again → expect PASS.

- [ ] **Step 3: Integration test.** Append; fixture file MUST end in `.py`. `build_facts(root, None) -> (GraphFacts, _)`; `GraphEdge` has `.relation`, `.target_label`, `.source_node_id`; `GraphNode` has `.node_id`, `.label`:

```rust
use spur_graph::{build_facts, RelationKind};

#[test]
fn py_derived_emits_extends_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.py"),
        "class Base:\n    pass\nclass Derived(Base):\n    pass\n").expect("write fixture");
    let (facts, _) = build_facts(dir.path(), None).expect("build facts");
    assert!(
        facts.edges.iter().any(|e| e.relation == RelationKind::Extends && e.target_label.as_deref() == Some("Base")),
        "missing extends->Base; edges: {:?}",
        facts.edges.iter().map(|e| (e.relation, e.target_label.clone())).collect::<Vec<_>>()
    );
}
```

Run → all PASS.

- [ ] **Step 4: Update the README matrix.** In `crates/spur-graph/queries/README.md`, change ONLY the `extends` row's Python cell from `TODO` to `Y`. The row becomes:

```markdown
| extends | Y | Y | Y | Y | Y | — |
```

(Leave the `implements` row unchanged — Python `implements` stays `TODO`: a Protocol/ABC heuristic is deferred Tier-3 work, not syntactic Tier-0.)

- [ ] **Step 5: Gate + lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib extract::languages::gate_contract
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/queries/python/spur-edges.scm \
        crates/spur-graph/tests/python_inheritance_edges.rs \
        crates/spur-graph/queries/README.md
git commit -m "feat(spur-graph): task-py bind extends for Python class inheritance"
```

## Self-Review

- **Spec coverage:** Advances §9 matrix Python column for `extends`; mirrors the C++ ontology decision (no syntactic `implements`).
- **Placeholder scan:** No TBD/TODO-in-code; the capture is a concrete candidate the TDD loop refines.
- **Type consistency:** No Rust types touched; `RelationKind::Extends` + the `emit_edges` arm already on main; `build_facts`/`GraphEdge` signatures match prior phases.
- **DAG:** single task, no deps.
- **beads compatibility:** unique ID, empty `depends_on`, verifiable acceptance criteria, scope boundary.
