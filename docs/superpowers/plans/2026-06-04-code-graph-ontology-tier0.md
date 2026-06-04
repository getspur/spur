# Code-Graph Ontology Tier-0 — Bind Orphan Predicates (Rust) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`
**Design epic:** (spec committed `e4c3fcbd`)

**Goal:** Close the spec's headline gap — `RelationKind::Implements` and `RelationKind::Extends` are declared but emit zero facts — by binding both predicates from Rust source, end-to-end (tree-sitter capture → classifier → resolved edge), with tests.

**Architecture:** Purely additive to the existing extraction pipeline. The predicate enum and parquet serialization **already handle all 10 `RelationKind` variants** (`relation_to_str`/`relation_from_str` in `store/parquet.rs:2765-2794`), so no schema or serialization change is needed. Each task (a) adds a capture pattern to `queries/rust/spur-edges.scm`, (b) adds a `match` arm to the capture classifier in `src/extract/languages.rs` that pushes a `PendingEdge` with the new relation, and (c) adds a query-level test plus an integration test. The new arms are byte-for-byte parallel to the existing `"import"` arm.

**Tech Stack:** Rust 2021, `tree-sitter` / `tree-sitter-rust`, the `spur-graph` crate. Build/test through `scripts/spur-cargo` (remote-default), never bare `cargo`.

**Scope guard for the whole epic:** Rust only. C++/TypeScript/Python realizations, the `PredicateSig` domain/range table, the disambiguation splits (`calls`→constructs/reads_field), and the gate test are **explicitly out of scope** — they are follow-up epics in the spec's §13–14. Do not touch `schema.rs` (the enum variants already exist).

---

### Task 1: Bind the `implements` predicate (Rust)

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-graph/queries/rust/spur-edges.scm` (append the `@implements` capture patterns)
- Modify: `crates/spur-graph/src/extract/languages.rs` (add an `"implements"` arm to the capture-dispatch `match`, alongside the existing `"import"` / `"reference.name"` arms near line 445)
- Create: `crates/spur-graph/tests/rust_implements_edge.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `impl Trait for Type` produces a `RelationKind::Implements` edge whose `target_label` is the trait name.
- [ ] Inherent impls (`impl Type { .. }`) produce **no** `implements` edge.
- [ ] Both new tests pass; the existing `rust_macro_token_tree_query.rs` tests and the `languages.rs` contract gate test still pass.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings` is clean.

**Suggested Worker:** codex (single crate, mechanical, additive).

**Scope Boundary:**
- IN scope: the three files above.
- OUT of scope: `schema.rs` (enum unchanged), any non-Rust `.scm`, the resolver, `parquet.rs`, `build.rs`.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Scope Drift Checkpoint:**
- If the query won't compile against the grammar without touching other languages' configs → emit `risk`.
- If binding the edge appears to require resolver changes → emit `scope_drift` (it should not; the existing `import` path resolves `target_name` the same way).

**Implementation:**

- [ ] **Step 1: Write the failing query-level test.**

Create `crates/spur-graph/tests/rust_implements_edge.rs`. The first test compiles the real `spur-edges.scm` and asserts the capture fires — modeled exactly on `crates/spur-graph/tests/rust_macro_token_tree_query.rs` (reuse its `capture_texts` helper verbatim):

```rust
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/rust/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();
    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == capture_name {
            names.push(capture.node.utf8_text(source.as_bytes()).expect("text").to_owned());
        }
    }
    names
}

#[test]
fn rust_spur_edges_query_captures_implemented_trait_name() {
    let source = "struct Button;\ntrait Drawable { fn draw(&self); }\nimpl Drawable for Button { fn draw(&self) {} }\n";
    let names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");
    assert!(names.contains(&"Drawable".to_owned()), "got {names:?}");
}

#[test]
fn rust_inherent_impl_does_not_capture_implements() {
    let source = "struct Button;\nimpl Button { fn new() -> Self { Button } }\n";
    let names = capture_texts(SPUR_EDGES_QUERY, source, "implements.name");
    assert!(names.is_empty(), "inherent impl must not emit implements; got {names:?}");
}
```

- [ ] **Step 2: Run the test; verify it FAILS.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test rust_implements_edge`
Expected: FAIL — `Query::new` succeeds but `implements.name` is never captured (the capture does not exist yet), so `rust_spur_edges_query_captures_implemented_trait_name` fails the `assert!`.

- [ ] **Step 3: Add the capture to `queries/rust/spur-edges.scm`.**

Append (the `trait:` field of `impl_item` is absent for inherent impls, so this never matches `impl Type {}`). Verify node names against the tree-sitter-rust grammar (`node-types.json`) and adjust until Step 4 passes:

```scheme
; `impl Trait for Type` — implements edge (Subject = enclosing impl, Object = Trait).
; Inherent impls have no `trait:` field and never match.
(impl_item
  trait: (type_identifier) @implements.name) @implements

(impl_item
  trait: (generic_type
    type: (type_identifier) @implements.name)) @implements

(impl_item
  trait: (scoped_type_identifier
    name: (type_identifier) @implements.name)) @implements
```

- [ ] **Step 4: Run the query-level test; verify it PASSES.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test rust_implements_edge`
Expected: both query-level tests PASS.

- [ ] **Step 5: Add the classifier arm in `src/extract/languages.rs`.**

In the capture-dispatch `match capture.name.as_str()` block (the one containing `"import"`, `"call"`, `"reference.name"`, ending in `_ => {}` near line 536), add this arm. It is identical in shape to the existing `"import"` arm (lines 446-460):

```rust
            "implements" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for trait_name in
                    contained_capture_text(capture, source, captures, "implements.name")
                {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: trait_name,
                        relation: RelationKind::Implements,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
```

- [ ] **Step 6: Add the integration test (edge emission) to the same test file.**

Append to `crates/spur-graph/tests/rust_implements_edge.rs`. Set up the fixture root following `crates/spur-graph/tests/cpp_definition_query.rs` / `tests/resolver.rs` (both call `build_facts(root, None)` on a fixture directory). `GraphEdge` carries `relation: RelationKind` and `target_label: Option<String>` (`schema.rs:203-219`):

```rust
use spur_graph::{build_facts, RelationKind};

#[test]
fn rust_impl_emits_implements_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "trait Drawable { fn draw(&self); }\nstruct Button;\nimpl Drawable for Button { fn draw(&self) {} }\n",
    )
    .expect("write fixture");

    let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");

    assert!(
        facts.edges.iter().any(|e| e.relation == RelationKind::Implements
            && e.target_label.as_deref() == Some("Drawable")),
        "expected an implements edge targeting Drawable; got {:?}",
        facts
            .edges
            .iter()
            .map(|e| (e.relation, e.target_label.clone()))
            .collect::<Vec<_>>()
    );
}
```

If `build_facts` on a bare directory yields no nodes (it may expect a specific root setup), copy the exact fixture-root construction from `tests/cpp_definition_query.rs` rather than inventing one.

- [ ] **Step 7: Run the full test file + the crate gate test; verify PASS.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test rust_implements_edge`
Then: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph` (ensures the `languages.rs` contract gate test and `rust_macro_token_tree_query.rs` still pass).
Expected: all PASS.

- [ ] **Step 8: Lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/queries/rust/spur-edges.scm \
        crates/spur-graph/src/extract/languages.rs \
        crates/spur-graph/tests/rust_implements_edge.rs
git commit -m "feat(spur-graph): task-1 bind implements predicate from Rust impl blocks"
```

---

### Task 2: Bind the `extends` predicate (Rust supertraits)

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-graph/queries/rust/spur-edges.scm` (append the `@extends` capture patterns)
- Modify: `crates/spur-graph/src/extract/languages.rs` (add an `"extends"` arm next to the `"implements"` arm from Task 1)
- Create: `crates/spur-graph/tests/rust_extends_edge.rs`
- Modify: `crates/spur-graph/queries/README.md` (start the relation coverage matrix — see Step 6)

**Depends on:** `task-1` (shares `spur-edges.scm` and `languages.rs`; sequential to avoid worktree conflicts).

**Acceptance Criteria:**
- [ ] `trait A: B` produces a `RelationKind::Extends` edge from `A` whose `target_label` is `B`.
- [ ] A trait with no supertrait bound (`trait A { .. }`) produces **no** `extends` edge.
- [ ] New tests pass; all Task-1 tests and the crate gate test still pass.
- [ ] `queries/README.md` documents `implements` and `extends` as `Y` for Rust in a new relation coverage matrix.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings` is clean.

**Suggested Worker:** codex.

**Scope Boundary:**
- IN scope: the four files above.
- OUT of scope: same exclusions as Task 1 (`schema.rs`, non-Rust `.scm`, resolver, `parquet.rs`).
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing query-level test.**

Create `crates/spur-graph/tests/rust_extends_edge.rs` with the same `capture_texts` helper as Task 1 (copy it verbatim; the two test files are independent):

```rust
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const SPUR_EDGES_QUERY: &str = include_str!("../queries/rust/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    let tree = parser.parse(source, None).expect("parse source");
    let query = Query::new(&language, query_source).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut names = Vec::new();
    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        if capture_names[capture.index as usize] == capture_name {
            names.push(capture.node.utf8_text(source.as_bytes()).expect("text").to_owned());
        }
    }
    names
}

#[test]
fn rust_spur_edges_query_captures_supertrait_name() {
    let source = "trait Base { fn b(&self); }\ntrait Derived: Base { fn d(&self); }\n";
    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");
    assert!(names.contains(&"Base".to_owned()), "got {names:?}");
}

#[test]
fn rust_trait_without_supertrait_does_not_capture_extends() {
    let source = "trait Lonely { fn x(&self); }\n";
    let names = capture_texts(SPUR_EDGES_QUERY, source, "extends.name");
    assert!(names.is_empty(), "got {names:?}");
}
```

- [ ] **Step 2: Run; verify FAIL.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test rust_extends_edge`
Expected: FAIL — `extends.name` is never captured yet.

- [ ] **Step 3: Add the capture to `queries/rust/spur-edges.scm`.**

Append (the `bounds:` field of `trait_item` is the supertrait list; absent when there is no bound). Verify the field/node names against the grammar and adjust until Step 4 passes:

```scheme
; `trait A: B` supertrait bound — extends edge (Subject = enclosing trait, Object = supertrait).
(trait_item
  bounds: (trait_bounds
    (type_identifier) @extends.name)) @extends

(trait_item
  bounds: (trait_bounds
    (scoped_type_identifier
      name: (type_identifier) @extends.name))) @extends
```

- [ ] **Step 4: Run the query-level test; verify PASS.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test rust_extends_edge`
Expected: both query-level tests PASS.

- [ ] **Step 5: Add the classifier arm in `src/extract/languages.rs`.**

Next to the `"implements"` arm, add:

```rust
            "extends" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for super_name in
                    contained_capture_text(capture, source, captures, "extends.name")
                {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: super_name,
                        relation: RelationKind::Extends,
                        edge_kind: None,
                        origin: crate::extract::tree_sitter::CallOrigin::Expression,
                        receiver_text: None,
                        scope_text: None,
                    });
                }
            }
```

- [ ] **Step 6: Add the integration test + README matrix.**

Append the edge-emission test to `crates/spur-graph/tests/rust_extends_edge.rs` (model the fixture root on `tests/cpp_definition_query.rs`, as in Task 1):

```rust
use spur_graph::{build_facts, RelationKind};

#[test]
fn rust_supertrait_emits_extends_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.rs"),
        "trait Base { fn b(&self); }\ntrait Derived: Base { fn d(&self); }\n",
    )
    .expect("write fixture");

    let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");

    assert!(
        facts.edges.iter().any(|e| e.relation == RelationKind::Extends
            && e.target_label.as_deref() == Some("Base")),
        "expected an extends edge targeting Base; got {:?}",
        facts
            .edges
            .iter()
            .map(|e| (e.relation, e.target_label.clone()))
            .collect::<Vec<_>>()
    );
}
```

Then add a relation coverage matrix to `crates/spur-graph/queries/README.md` (new section, mirroring the existing node-kind coverage matrix), recording the two predicates this epic binds:

```markdown
## Relation Coverage Matrix

Predicate realization per language family (`Y` realized · `—` not realizable · `TODO` gap).
This table is the relation-level analogue of the Definition Coverage Matrix and the
seed of the Tier-0 ontology realization contract
(`docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`).

| Predicate | Rust | Python | TypeScript | Tsx | Cpp | Markdown |
|---|---|---|---|---|---|---|
| imports | Y | Y | Y | Y | Y | Y(links) |
| calls | Y | Y | Y | Y | Y | — |
| contains | Y | Y | Y | Y | Y | Y |
| references (HOF) | Y | TODO | Y | Y | TODO | — |
| links | — | — | — | — | — | Y |
| implements | Y | TODO | TODO | TODO | TODO | — |
| extends | Y | TODO | TODO | TODO | TODO | — |
```

- [ ] **Step 7: Run the full crate test suite; verify PASS.**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph`
Expected: all PASS (new extends tests, Task-1 implements tests, the gate test, and the macro-query tests).

- [ ] **Step 8: Lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/queries/rust/spur-edges.scm \
        crates/spur-graph/src/extract/languages.rs \
        crates/spur-graph/tests/rust_extends_edge.rs \
        crates/spur-graph/queries/README.md
git commit -m "feat(spur-graph): task-2 bind extends predicate from Rust supertrait bounds"
```

---

## Dependency DAG

```
task-1 (implements)  ──▶  task-2 (extends)
```

Sequential by necessity: both tasks edit `queries/rust/spur-edges.scm` and
`src/extract/languages.rs`; running them in parallel worktrees would conflict.
No other dependencies.

## Self-Review

- **Spec coverage:** Implements §1 (orphan predicates emit 0) and §9 (per-language realization, Rust column) for two of the four orphan predicates; starts §13 deliverable 5 (README relation matrix). `Uses`/`Defines`, disambiguation splits, other languages, and the gate test are deferred to follow-up epics by explicit scope guard.
- **Placeholder scan:** No TBD/TODO-in-code. Every code block is concrete; the `.scm` captures are real candidates the TDD loop refines against the grammar.
- **Type consistency:** `RelationKind::{Implements,Extends}` exist in `schema.rs:282`; `PendingEdge` fields match the existing `"import"` arm; `GraphEdge.relation`/`target_label` match `schema.rs:203`; `build_facts(root, None) -> (GraphFacts, _)` matches existing tests.
- **DAG validation:** Two-node chain, acyclic.
- **beads compatibility:** Both tasks have unique IDs, explicit `depends_on`, verifiable acceptance criteria (specific edges emitted), and scope boundaries.
