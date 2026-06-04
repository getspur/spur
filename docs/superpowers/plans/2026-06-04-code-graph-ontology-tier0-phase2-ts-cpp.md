# Ontology Tier-0 Phase 2 — Bind implements/extends for TypeScript + C++ Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb`
**Phase 1 (Rust):** landed on main — `a9842865` (implements), `c2e017bb` (extends).

**Goal:** Fill the realization matrix for the next two languages: bind `implements`/`extends` from TypeScript and `extends` from C++, by adding tree-sitter captures only.

**Architecture — the key fact:** `emit_edges` in `src/extract/languages.rs` is **language-agnostic**. Its `"implements"` and `"extends"` match arms (landed in Phase 1) fire for ANY language whose `spur-edges.scm` emits `@implements`/`@extends` captures (with inner `@implements.name`/`@extends.name`). So this phase adds **`.scm` captures and tests only — NO Rust/`languages.rs`/`schema.rs` changes.** The TypeScript and C++ query files are independent, so the two binding tasks run in parallel.

**Ontology decision (recorded here, settles the spec §9 ambiguity):**
- **TypeScript** has a clean split: `implements_clause` → `implements`; `extends_clause` (class) and interface `extends` → `extends`.
- **C++** has NO syntactic interface concept — `base_class_clause` is inheritance. It maps to **`extends`**. `implements` is **`—` (not realizable)** for C++ at the syntactic Tier-0 level (distinguishing an abstract-base "interface" needs semantics, which is out of Tier-0 scope).

**Tech Stack:** Rust 2021, `tree-sitter-typescript`, `tree-sitter-cpp` (already workspace deps, used in `languages.rs`). Build/test through `scripts/spur-cargo`, never bare cargo.

**Epic scope guard:** Edit ONLY the two `spur-edges.scm` files, two new test files, and (in the final task) `queries/README.md`. Do NOT touch `schema.rs`, `languages.rs`, the resolver, `parquet.rs`, `Cargo.toml`, the Rust query files, or `rust_*_edge.rs`.

⚠️ **KNOWN PRE-EXISTING FAILURE — DO NOT CHASE (applies to every task):** the full `spur-graph` suite has a PRE-EXISTING, OUT-OF-SCOPE failure in `tests/incremental_ingest.rs` (2/3 fail on clean main: an incremental-pointer assertion and a poisoned-mutex panic), unrelated to this work. Do NOT run the whole-crate suite as an acceptance gate, do NOT try to fix it, do NOT emit a `scope_drift` signal about it. Only build/run the named test binaries below. Only emit `scope_drift` if you must edit a file outside this task's listed set.

---

### Task ts: Bind `implements` + `extends` for TypeScript

**Task ID:** `task-ts`

**Files:**
- Modify: `crates/spur-graph/queries/typescript/spur-edges.scm` (append `@implements`/`@extends` captures; this file is shared by TypeScript AND Tsx, so reference only nodes common to both — no JSX nodes)
- Create: `crates/spur-graph/tests/ts_inheritance_edges.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `class C implements I {}` → a `RelationKind::Implements` edge from `C` with `target_label` `I`.
- [ ] `class C extends B {}` → a `RelationKind::Extends` edge from `C` with `target_label` `B`.
- [ ] `class Plain {}` (no heritage) → neither edge.
- [ ] Tests pass; the `languages.rs` `gate_contract` test still passes.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the two files above. OUT: everything else (esp. `languages.rs` — the arms already exist; `jsx-edges.scm`; `schema.rs`; `Cargo.toml`).

**Implementation:**

- [ ] **Step 1: Failing query-level test.** Create `crates/spur-graph/tests/ts_inheritance_edges.rs`. Copy the `capture_texts` helper from `crates/spur-graph/tests/rust_implements_edge.rs`, but change the language to TypeScript:

```rust
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};
const SPUR_EDGES_QUERY: &str = include_str!("../queries/typescript/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
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
fn ts_captures_implemented_interface() {
    let src = "interface I { f(): void }\nclass C implements I { f() {} }\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "implements.name").contains(&"I".to_owned()));
}
#[test]
fn ts_captures_extended_base_class() {
    let src = "class B {}\nclass C extends B {}\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "extends.name").contains(&"B".to_owned()));
}
#[test]
fn ts_plain_class_has_no_heritage_edges() {
    let src = "class Plain {}\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "implements.name").is_empty());
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "extends.name").is_empty());
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test ts_inheritance_edges` → expect FAIL (captures don't exist yet).

- [ ] **Step 2: Add captures to `typescript/spur-edges.scm`.** Append the following candidate patterns and **refine node names against the tree-sitter-typescript grammar until Step 1 passes** (the class heritage node is `class_heritage`; clauses are `implements_clause` / `extends_clause`; the base in `extends_clause` may be an `(identifier)` or `(member_expression)`):

```scheme
; `class C implements I` — implements edge to each interface.
(class_declaration
  (class_heritage
    (implements_clause [(type_identifier) @implements.name
                        (generic_type (type_identifier) @implements.name)]))) @implements

; `class C extends B` — extends edge to the base class.
(class_declaration
  (class_heritage
    (extends_clause value: [(identifier) @extends.name
                            (member_expression property: (property_identifier) @extends.name)]))) @extends

; `interface I extends J` — extends edge between interfaces.
(interface_declaration
  (extends_type_clause [(type_identifier) @extends.name
                        (generic_type (type_identifier) @extends.name)])) @extends
```

Run Step 1 again → expect PASS. (If `abstract_class_declaration` is needed for an abstract-class case, add the mirrored pattern; the three tests above do not require it.)

- [ ] **Step 3: Integration test (edge emission).** Append to the same test file. Model the fixture root on `crates/spur-graph/tests/cpp_definition_query.rs`; the fixture file MUST end in `.ts` so the language is detected. `build_facts(root, None) -> (GraphFacts, _)`; `GraphEdge` has `.relation`, `.target_label`, `.source_node_id`; `GraphNode` has `.node_id`, `.label`:

```rust
use spur_graph::{build_facts, RelationKind};

#[test]
fn ts_class_emits_implements_and_extends_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.ts"),
        "interface I { f(): void }\nclass B {}\nclass C extends B implements I { f() {} }\n",
    ).expect("write fixture");
    let (facts, _) = build_facts(dir.path(), None).expect("build facts");
    let has = |rel, tgt| facts.edges.iter().any(|e| e.relation == rel && e.target_label.as_deref() == Some(tgt));
    assert!(has(RelationKind::Implements, "I"), "missing implements->I; edges: {:?}",
        facts.edges.iter().map(|e| (e.relation, e.target_label.clone())).collect::<Vec<_>>());
    assert!(has(RelationKind::Extends, "B"), "missing extends->B");
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test ts_inheritance_edges` → all PASS.

- [ ] **Step 4: Gate + lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib extract::languages::gate_contract
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/queries/typescript/spur-edges.scm crates/spur-graph/tests/ts_inheritance_edges.rs
git commit -m "feat(spur-graph): task-ts bind implements/extends for TypeScript"
```

---

### Task cpp: Bind `extends` for C++

**Task ID:** `task-cpp`

**Files:**
- Modify: `crates/spur-graph/queries/cpp/spur-edges.scm` (append `@extends` capture)
- Create: `crates/spur-graph/tests/cpp_inheritance_edges.rs`

**Depends on:** none (independent of `task-ts` — different files).

**Ontology note:** C++ `base_class_clause` → `extends`. C++ does NOT get an `implements` edge (no syntactic interface). Do not add an `@implements` capture for C++.

**Acceptance Criteria:**
- [ ] `struct D : Base {}` / `class D : public Base {}` → a `RelationKind::Extends` edge from `D` with `target_label` `Base`.
- [ ] A class with no base (`struct Plain {};`) → no `extends` edge.
- [ ] Tests pass; `gate_contract` still passes; clippy `-D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the two files above. OUT: everything else (esp. `languages.rs`, `schema.rs`, `Cargo.toml`, the TS files).

**Implementation:**

- [ ] **Step 1: Failing query-level test.** Create `crates/spur-graph/tests/cpp_inheritance_edges.rs` with the `capture_texts` helper using the C++ grammar:

```rust
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};
const SPUR_EDGES_QUERY: &str = include_str!("../queries/cpp/spur-edges.scm");

fn capture_texts(query_source: &str, source: &str, capture_name: &str) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
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
fn cpp_captures_base_class() {
    let src = "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override {} };\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "extends.name").contains(&"Base".to_owned()));
}
#[test]
fn cpp_class_without_base_has_no_extends() {
    let src = "struct Plain { int x; };\n";
    assert!(capture_texts(SPUR_EDGES_QUERY, src, "extends.name").is_empty());
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test cpp_inheritance_edges` → expect FAIL.

- [ ] **Step 2: Add the capture to `cpp/spur-edges.scm`.** If the file does not yet exist with content, append; refine against the tree-sitter-cpp grammar until Step 1 passes (the inheritance node is `base_class_clause`, child of `class_specifier`/`struct_specifier`; the base name is a `(type_identifier)`, possibly under `(qualified_identifier)` / `(template_type)`):

```scheme
; C++ base classes — `class D : public Base` / `struct D : Base`. C++ has no
; syntactic interface, so inheritance maps to `extends` (not `implements`).
(class_specifier
  (base_class_clause
    [(type_identifier) @extends.name
     (qualified_identifier name: (type_identifier) @extends.name)])) @extends

(struct_specifier
  (base_class_clause
    [(type_identifier) @extends.name
     (qualified_identifier name: (type_identifier) @extends.name)])) @extends
```

Run Step 1 again → expect PASS.

- [ ] **Step 3: Integration test.** Append; fixture file MUST end in a C++ extension (`.cpp` or `.hpp`). Model on `tests/cpp_definition_query.rs`:

```rust
use spur_graph::{build_facts, RelationKind};

#[test]
fn cpp_derived_emits_extends_edge() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("lib.cpp"),
        "struct Base { virtual void f(); };\nstruct Derived : Base { void f() override {} };\n",
    ).expect("write fixture");
    let (facts, _) = build_facts(dir.path(), None).expect("build facts");
    assert!(
        facts.edges.iter().any(|e| e.relation == RelationKind::Extends && e.target_label.as_deref() == Some("Base")),
        "missing extends->Base; edges: {:?}",
        facts.edges.iter().map(|e| (e.relation, e.target_label.clone())).collect::<Vec<_>>()
    );
}
```

Run → all PASS.

- [ ] **Step 4: Gate + lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib extract::languages::gate_contract
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/queries/cpp/spur-edges.scm crates/spur-graph/tests/cpp_inheritance_edges.rs
git commit -m "feat(spur-graph): task-cpp bind extends for C++ base classes"
```

---

### Task readme: Update the relation coverage matrix

**Task ID:** `task-readme`

**Files:**
- Modify: `crates/spur-graph/queries/README.md` (the `## Relation Coverage Matrix` section added in Phase 1)

**Depends on:** `task-ts`, `task-cpp` (records what they implemented).

**Acceptance Criteria:**
- [ ] The matrix rows for `implements` and `extends` reflect: TypeScript `Y`/`Y`, Tsx `Y`/`Y`, Cpp `—`/`Y`.

**Suggested Worker:** codex.

**Scope Boundary:** IN: `queries/README.md` only. OUT: everything else.

**Implementation:**

- [ ] **Step 1: Edit the matrix.** In the `## Relation Coverage Matrix` table, set the `implements` and `extends` rows to:

```markdown
| implements | Y | TODO | Y | Y | — | — |
| extends | Y | TODO | Y | Y | Y | — |
```

(Columns are: Predicate | Rust | Python | TypeScript | Tsx | Cpp | Markdown. C++ `implements` is `—` because C++ has no syntactic interface; its `base_class_clause` is `extends`.)

- [ ] **Step 2: Commit.**

```bash
git add crates/spur-graph/queries/README.md
git commit -m "docs(spur-graph): task-readme mark TS/C++ implements-extends in relation matrix"
```

---

## Dependency DAG

```
task-ts  ─┐
          ├─▶ task-readme
task-cpp ─┘
```

`task-ts` and `task-cpp` edit disjoint files and run in parallel. `task-readme` edits `README.md` (untouched by the other two) and runs after both, recording their realization.

## Self-Review

- **Spec coverage:** Advances §9 realization matrix (TS, C++ columns) for `implements`/`extends`; resolves the §9 C++ ambiguity by mapping `base_class_clause` → `extends` and marking C++ `implements` = `—`.
- **Placeholder scan:** No TBD/TODO-in-code; captures are concrete candidates the TDD loop refines.
- **Type consistency:** No Rust types touched — `RelationKind::{Implements,Extends}` and the `emit_edges` arms already exist on main; tasks add `.scm` captures + tests only. `build_facts`/`GraphEdge`/`GraphNode` signatures match Phase-1 usage.
- **DAG validation:** Diamond (two roots → one sink), acyclic; max parallelism for the two independent language files.
- **beads compatibility:** Each task has a unique ID, explicit `depends_on`, verifiable acceptance criteria (specific edges emitted), and a scope boundary.
