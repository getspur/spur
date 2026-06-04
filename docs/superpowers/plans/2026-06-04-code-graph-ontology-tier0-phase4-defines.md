# Ontology Tier-0 Phase 4 — Split `contains` → add `defines` Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb` (§5, §8, §13)
**Prior phases on main:** orphan predicates `implements`/`extends` bound across Rust/Python/TS/Tsx/C++.

**Goal:** Emit a semantic `defines` edge **alongside** the existing lexical `contains` edge, so "an enclosing definition declares this nested definition" is a first-class, queryable relationship. `contains` stays unchanged as the lexical spine (so `v_doc_tree` / `lance_sections`, which filter `relation = Contains`, are unaffected).

**Architecture:** The parent→child `Contains` edge for every definition is emitted in ONE place — `emit_definitions_with_parents` in `src/extract/languages.rs` (currently `builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None)`). This is language-agnostic (Rust/Python/TS/Tsx/C++ all route through it; Markdown uses a separate path and is intentionally excluded). The change adds, right after that line, a parallel `Defines` edge **only when the parent is a real enclosing definition** (not the file root):

```rust
builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None);
if parent.node_id != file_node_id {
    builder.add_edge(parent.node_id, Some(node_id), RelationKind::Defines, None);
}
```

This yields `defines` for module→fn, struct→field, enum→variant, impl→method, trait→method, class→member — and NOT for file→top-level item (the file is not a definition). `RelationKind::Defines` already exists and is already serialized (`store/parquet.rs` `relation_to_str`/`relation_from_str`), so **no schema/serialization change**.

⚠️ **KNOWN PRE-EXISTING FAILURE:** `tests/incremental_ingest.rs` fails 2/3 on clean main (unrelated, out-of-scope). Do NOT run it, do NOT fix it, do NOT `scope_drift` about it.

**Blast-radius note (important for this task):** unlike the captures-only phases, this adds edges to EVERY fixture with nested definitions, so some `build_facts`-based integration tests that assert an exact edge *set* may now legitimately include `defines` rows. The synthetic-graph tests in `src/traversal.rs` use a hand-built `artifact()` and are unaffected. Updating a genuinely-changed edge-set/count assertion in an existing spur-graph test IS in scope for this task (it is the same semantic change). Adding NEW unrelated behavior is not.

---

### Task defines: Emit `defines` parallel to `contains`

**Task ID:** `task-defines`

**Files:**
- Modify: `crates/spur-graph/src/extract/languages.rs` (the two-line addition in `emit_definitions_with_parents`)
- Create: `crates/spur-graph/tests/defines_edges.rs`
- Modify: `crates/spur-graph/queries/README.md` (add a `defines` row to the relation matrix)
- Modify (only if they legitimately change): existing `crates/spur-graph/tests/*.rs` / `src/**` unit tests that assert exact edge sets/counts now including `defines`.

**Depends on:** none

**Acceptance Criteria:**
- [ ] `struct S { f }` → a `Defines` edge `S`→`f` (in addition to the existing `Contains`).
- [ ] `enum E { V }` → `Defines` `E`→`V`; `mod m { fn g() {} }` → `Defines` `m`→`g`; `impl T for S { fn h() {} }` → `Defines` impl→`h`.
- [ ] A top-level item (parent is the file) gets `Contains` but NOT `Defines`.
- [ ] Existing `Contains` edges are unchanged (still emitted for the same pairs).
- [ ] `gate_contract` passes; clippy `-D warnings` clean; README matrix gains a `defines` row.

**Suggested Worker:** codex.

**Scope Boundary:** IN: `languages.rs` (the 2-line add only), the new test, README, and any existing spur-graph edge-assertion test that legitimately changes. OUT: `schema.rs` (Defines already exists), `parquet.rs` (serialization already exists), the resolver logic, other crates, the analyst POC. Do NOT change the existing `Contains` emission. Do NOT add finer parent-kind gating (the `parent != file_node` rule is the agreed v1).

**Scope Drift Checkpoint:** if more than ~3 existing test files need assertion updates, or a failure looks like a REAL regression (not just an added `defines` row), STOP and emit `risk` with the details rather than mass-editing.

**Implementation:**

- [ ] **Step 1: Failing integration test.** Create `crates/spur-graph/tests/defines_edges.rs`. Model the `build_facts` fixture harness on `crates/spur-graph/tests/cpp_definition_query.rs` / `rust_implements_edge.rs`. `GraphEdge` has `.relation`, `.target_label`, `.source_node_id`; `GraphNode` has `.node_id`, `.label`:

```rust
use spur_graph::{build_facts, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

// (If `spur_graph::extract::GraphFacts` is not the right public path, return the
// tuple's .0 inline as the other *_edge tests do; the type is what build_facts returns.)

#[test]
fn struct_defines_field_and_enum_defines_variant() {
    let facts = build("struct S { f: i32 }\nenum E { V }\n");
    let label = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.label.as_str());
    let defines = |s: &str, t: &str| facts.edges.iter().any(|e|
        e.relation == RelationKind::Defines && label(e.source_node_id) == Some(s)
        && e.target_label.as_deref() == Some(t));
    assert!(defines("S", "f"), "S defines f; got {:?}",
        facts.edges.iter().filter(|e| e.relation == RelationKind::Defines)
            .map(|e| (label(e.source_node_id), e.target_label.clone())).collect::<Vec<_>>());
    assert!(defines("E", "V"), "E defines V");
    // Contains is still emitted (unchanged spine).
    assert!(facts.edges.iter().any(|e| e.relation == RelationKind::Contains
        && e.target_label.as_deref() == Some("f")), "Contains S->f still present");
}

#[test]
fn module_defines_fn_but_file_does_not_define_module() {
    let facts = build("mod m { fn g() {} }\n");
    let label = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.label.as_str());
    assert!(facts.edges.iter().any(|e| e.relation == RelationKind::Defines
        && label(e.source_node_id) == Some("m") && e.target_label.as_deref() == Some("g")),
        "m defines g");
    // The file is not a definition: `m` must have NO inbound Defines edge.
    assert!(!facts.edges.iter().any(|e| e.relation == RelationKind::Defines
        && e.target_label.as_deref() == Some("m")), "file must not `define` the module");
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test defines_edges` → expect FAIL (no `Defines` edges yet).

- [ ] **Step 2: Add the emission** in `emit_definitions_with_parents` (`src/extract/languages.rs`), immediately after the existing `RelationKind::Contains` `add_edge` call:

```rust
        builder.add_edge(parent.node_id, Some(node_id), RelationKind::Contains, None);
        // `defines`: a real enclosing definition declares this nested definition.
        // The file root is not a definition, so file→top-level item stays contains-only.
        if parent.node_id != file_node_id {
            builder.add_edge(parent.node_id, Some(node_id), RelationKind::Defines, None);
        }
```

Run Step 1 again → expect PASS.

- [ ] **Step 3: Broad regression check (within the allowed gate).** Run, and ensure green (the ONLY acceptable failures are the 2 pre-existing `incremental_ingest` tests, which you must NOT run):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test defines_edges --test resolver \
  --test rust_implements_edge --test rust_extends_edge --test ts_inheritance_edges \
  --test cpp_inheritance_edges --test python_inheritance_edges
```

If any of these fail because an assertion now legitimately includes a `defines` edge (an added row in an exact-set or a count that grew by the number of definition-nestings), update that specific assertion to include `defines` and note it. If a failure looks like a real regression (a `contains`/`calls`/resolution edge changed or disappeared), STOP and emit `risk`.

- [ ] **Step 4: README matrix.** Add a `defines` row to the `## Relation Coverage Matrix` (it is builder-emitted for all code languages; Markdown uses a separate path):

```markdown
| defines | Y | Y | Y | Y | Y | — |
```

- [ ] **Step 5: Lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/languages.rs crates/spur-graph/tests/defines_edges.rs \
        crates/spur-graph/queries/README.md
# plus any existing test files whose edge assertions you updated
git commit -m "feat(spur-graph): task-defines emit defines edge parallel to contains"
```

## Self-Review

- **Spec coverage:** Implements §5/§13 `contains → +defines` split; `contains` stays the lexical spine (open-question #1 lean: "emit both").
- **Placeholder scan:** concrete two-line emission + concrete tests.
- **Type consistency:** `RelationKind::Defines` exists (`schema.rs`) and is serialized (`parquet.rs`); `build_facts`/`GraphEdge` match prior phases. No schema change.
- **DAG:** single task.
- **Risk:** core-builder change; bounded by the `parent != file_node` rule, the broad regression gate, and the `risk`-signal checkpoint for real regressions.
