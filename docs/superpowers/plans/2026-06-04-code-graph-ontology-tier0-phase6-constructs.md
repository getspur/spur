# Ontology Tier-0 Phase 6 — `constructs` predicate (split from `calls`) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb` (§8 disambiguation).
**Grounding (spur-analyst, artifact c27a41d1):** of 25,983 resolved `calls`, **1,193 target an enum_variant** (`Err(x)`, `Action::NavigateTo`, `JsonValue::String`…) and **150 target a struct** (`MermaidId(x)`, `SessionId(x)`) — these are construction, not invocation. (`reads_field` is intentionally NOT in scope: the 4,705 calls→field are mostly `.into()`/`.join()` MISRESOLUTIONS, a separate problem.)

**Goal:** Split `constructs` out of `calls`: a `calls` edge that resolves to a type-like target (`Struct`/`EnumVariant`/`Class`) is reclassified to `RelationKind::Constructs`. `calls` keeps invocation-of-callables only.

**Architecture:** `RelationKind::Constructs` does NOT exist yet, so this needs (1) the enum variant, (2) serialization, (3) the reclassification at resolution. The reclassification point is exact: in `resolve_singleton_bare_target` (`crates/spur-graph/src/extract/tree_sitter.rs`, ~787), the `_ =>` arm is the sink for non-Method/non-Function resolved targets — i.e. Struct/EnumVariant/Class. Guard it on `relation == Calls` + target kind, and emit `Constructs` via a new `add_pending_edge_as`.

📌 **Golden artifacts WILL change** (corpus fixtures contain construction). Re-blessing the 4 goldens is **in scope and budgeted** below (the phase-4/5 lesson).

⚠️ **`incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it (separate epic handles it).

**Downstream note (for the reviewer, not the worker):** relabeling ~1.3k edges from `calls` to `constructs` means consumers filtering `relation='calls'` (call-graph views) no longer see construction edges. That is the intended semantic split; downstream view adaptation is out of scope.

---

### Task constructs: reclassify calls→{Struct,EnumVariant,Class} as constructs

**Task ID:** `task-constructs`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/schema.rs` (add `RelationKind::Constructs`)
- Modify: `crates/spur-graph/src/store/parquet.rs` (serialize `constructs` in `relation_to_str`/`relation_from_str`)
- Modify: any OTHER exhaustive `match` on `RelationKind` the compiler flags (e.g. `store/build.rs` `relation_to_str`) — add the `Constructs` arm
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the `add_pending_edge_as` helper + the `_`-arm reclassification)
- Create: `crates/spur-graph/tests/constructs_edges.rs`
- Modify: `crates/spur-graph/queries/README.md` (add a `constructs` matrix row)
- Regenerate (bless, expected): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `struct Foo(u32); fn f() { let _ = Foo(1); }` → a `Constructs` edge `f`→`Foo` (NOT `Calls`).
- [ ] `enum E { V(u32) } fn g() { let _ = E::V(1); }` → a `Constructs` edge to `V`.
- [ ] A plain function call (`fn h() {} fn c() { h(); }`) stays `Calls`.
- [ ] No `Constructs` edge has a target whose kind ∉ {Struct, EnumVariant, Class}.
- [ ] Goldens re-blessed; full `-p spur-graph` suite green except the flaky `incremental_ingest`; `gate_contract` + clippy clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the files above. OUT: the `reads_field`/`calls→field` problem, the analyst POC, other crates, the resolver's range logic (phase 5, already landed). Do NOT change how `calls` resolves callables — only relabel the already-resolved Struct/EnumVariant/Class case.

**Implementation:**

- [ ] **Step 1: Add the enum variant + serialization.**
  - `schema.rs`: add `Constructs` to `RelationKind` (after `Calls`).
  - `parquet.rs`: `RelationKind::Constructs => "constructs"` in `relation_to_str`; `"constructs" => Ok(RelationKind::Constructs)` in `relation_from_str`.
  - Build (`SPUR_REMOTE=1 scripts/spur-cargo build -p spur-graph`) and add a `Constructs` arm to every OTHER exhaustive `RelationKind` match the compiler reports (likely `store/build.rs::relation_to_str`). Mirror the `calls` string `"constructs"`.

- [ ] **Step 2: Failing test.** Create `crates/spur-graph/tests/constructs_edges.rs` (model the `build_facts` harness on `tests/rust_implements_edge.rs`):

```rust
use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

#[test]
fn tuple_struct_and_enum_variant_construction_is_constructs() {
    let facts = build(
        "struct Foo(u32);\nenum E { V(u32) }\nfn f() { let _ = Foo(1); }\nfn g() { let _ = E::V(1); }\n",
    );
    let has = |rel, tgt| facts.edges.iter().any(|e| e.relation == rel && e.target_label.as_deref() == Some(tgt));
    assert!(has(RelationKind::Constructs, "Foo"), "Foo(1) should be constructs; edges: {:?}",
        facts.edges.iter().map(|e| (e.relation, e.target_label.clone())).collect::<Vec<_>>());
    assert!(has(RelationKind::Constructs, "V"), "E::V(1) should be constructs");
    // No invocation got mislabeled and no constructs targets a non-type kind.
    let kind_of = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.kind);
    for e in &facts.edges {
        if e.relation == RelationKind::Constructs {
            if let Some(t) = e.target_node_id {
                assert!(matches!(kind_of(t), Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)),
                    "constructs to non-type kind {:?}", kind_of(t));
            }
        }
    }
}

#[test]
fn plain_function_call_stays_calls() {
    let facts = build("fn h() {}\nfn c() { h(); }\n");
    assert!(facts.edges.iter().any(|e| e.relation == RelationKind::Calls && e.target_label.as_deref() == Some("h")));
    assert!(!facts.edges.iter().any(|e| e.relation == RelationKind::Constructs && e.target_label.as_deref() == Some("h")));
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test constructs_edges` → expect FAIL (today `Foo(1)`/`E::V(1)` are `Calls`).

- [ ] **Step 3: Reclassify at resolution** in `tree_sitter.rs`. Add the helper near `add_pending_edge`:

```rust
    fn add_pending_edge_as(
        &mut self,
        edge: &PendingEdge,
        target: Option<NodeId>,
        relation: RelationKind,
    ) {
        let metadata = metadata_for_pending_edge(edge, target, None);
        self.add_edge_with_metadata(
            edge.source,
            target,
            relation,
            Some(edge.target_name.clone()),
            edge.edge_kind,
            metadata,
        );
    }
```

In `resolve_singleton_bare_target`, change the final `_ =>` arm so a resolved `Calls` edge to a type-like target becomes `Constructs`:

```rust
        _ => {
            if edge.relation == RelationKind::Calls
                && matches!(
                    indexes.node_kind_by_id.get(&target).copied(),
                    Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)
                )
            {
                builder.add_pending_edge_as(edge, Some(target), RelationKind::Constructs);
            } else {
                builder.add_pending_edge(edge, Some(target));
            }
        }
```

Run Step 2 again → expect PASS.

- [ ] **Step 4: Re-bless goldens (expected).** Some calls→struct/enum_variant/class flip to constructs:

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

Sanity-check the golden diff: it should ONLY flip some `calls` edges (whose target is a struct/enum_variant/class) to `constructs` — NO `contains`/`defines`/`implements`/`extends`/`imports` edges change, and no edge's TARGET changes. If anything else changed, STOP and emit `risk`.

- [ ] **Step 5: README matrix.** Add a `constructs` row (it fires wherever construction resolves to a type — all code languages):

```markdown
| constructs | Y | Y | Y | Y | Y | — |
```

- [ ] **Step 6: Broad gate + commit** (green except flaky `incremental_ingest`, which you must NOT run):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test constructs_edges --test extractor \
  --test resolver --test rust_implements_edge --test rust_extends_edge --test defines_edges \
  --test range_resolution_edges --test ts_inheritance_edges --test cpp_inheritance_edges --test python_inheritance_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/schema.rs crates/spur-graph/src/store/parquet.rs \
        crates/spur-graph/src/extract/tree_sitter.rs crates/spur-graph/tests/constructs_edges.rs \
        crates/spur-graph/queries/README.md crates/spur-graph/tests/fixtures/
# plus store/build.rs if the compiler required a Constructs arm there
git commit -m "feat(spur-graph): split constructs out of calls for type construction"
```

If a `resolver.rs` assertion legitimately changes (a calls edge is now constructs), update it (in scope) and note it. A change to contains/defines/implements/extends is a regression → STOP, emit `risk`.

Report: the calls→constructs delta, which goldens changed, and any other RelationKind match arm you had to add.

## Self-Review
- **Spec coverage:** §8 `calls → constructs` split (the type-construction half; reads_field deferred, documented).
- **Type consistency:** new `RelationKind::Constructs` threaded through serialization (compiler-enforced exhaustive matches); `add_pending_edge_as` mirrors `add_pending_edge_with_bind_method`; `NodeKind::{Struct,EnumVariant,Class}` + `indexes.node_kind_by_id` already used in the same function.
- **DAG:** single task.
- **Risk:** schema + resolver change; bounded by the type-kind guard, golden re-bless in-scope, and the diff-is-calls→constructs-only check.
