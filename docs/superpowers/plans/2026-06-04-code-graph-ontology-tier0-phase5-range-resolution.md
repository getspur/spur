# Ontology Tier-0 Phase 5 — Range-Constrained Resolution (implements/extends) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-design.ipynb` (§6 domain/range).
**Grounding:** empirically confirmed (spur-analyst + code_read_symbol). In the live artifact `c27a41d1`, `extends` is 1/58 correct and `implements` 251/333 correct; the rest resolve to **wrong-kind** local symbols. Root cause proven by reading source: `pub trait Adapter: Send + Sync` captures `extends→Send`/`Sync`, but std `Send`/`Sync` have no worktree definition, so the resolver binds `Send` to `SubmitDecision::Send` (an enum_variant in spur-tui) and `Sync` to a markdown section. Same for `impl Default for X` → enum_variant `Default`.

**Goal:** Make the resolver **range-aware** for the relational predicates so `implements`/`extends` only resolve to type-like targets; out-of-range names (std/marker/derive traits like `Send`/`Sync`/`Default`) are left **unresolved** (an honest `target_label` with no wrong node) instead of bound to a same-named enum_variant/section.

**Range (spec §6):** `implements → {Trait, Interface}`; `extends → {Trait, Interface, Class}`.

**Architecture — exact defect location:** `resolve_bare_pending_edge` in `crates/spur-graph/src/extract/tree_sitter.rs` (676–785) has kind-filtered candidate blocks for `Calls` (via `callable_symbol_candidates`) and `Imports` (via `import_resolution_candidates`), but `implements`/`extends` fall through to the **generic, kind-blind** singleton-by-label path (770–784) + `resolve_singleton_bare_target`'s `_ =>` catch-all (818), which accepts any kind. The fix adds a relational block mirroring the Calls/Imports ones.

**Scope:** `implements`/`extends` ONLY (the confirmed-broken predicates). Do NOT touch `calls` resolution (it has its own kind filter; its separate field-misresolution via the generic fallback is a documented follow-up, out of scope here).

⚠️ **KNOWN PRE-EXISTING:** `tests/incremental_ingest.rs` is **flaky** (cross-thread `PoisonError` + a pointer assertion), unrelated. Do NOT fix it; treat its failures as acceptable noise (run the rest).

📌 **Golden artifacts WILL change** (some implements/extends edges move resolved→unresolved). Re-blessing the 4 corpus goldens is **in scope and expected** — budgeted below so it does NOT trip a scope-drift split.

---

### Task range: range-constrain implements/extends resolution

**Task ID:** `task-range`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (the relational block + 2 helpers)
- Create: `crates/spur-graph/tests/range_resolution_edges.rs`
- Regenerate (via bless, expected): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`
- Modify (only if a resolver assertion legitimately changes): `crates/spur-graph/tests/resolver.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] An `extends`/`implements` edge NEVER resolves to a target whose kind ∉ {Trait, Interface, Class} (for extends) / {Trait, Interface} (for implements). Out-of-range names are left unresolved (`target_node_id = None`, `target_label` retained).
- [ ] A supertrait/trait that DOES exist locally still resolves (e.g. `trait Derived: Base {}` → resolved extends→Base when `Base` is a local trait).
- [ ] `trait Marker: Send {}` with a local `enum { Send }` does NOT produce a resolved extends edge to the enum variant.
- [ ] Golden artifacts re-blessed; full `-p spur-graph` suite green except the flaky `incremental_ingest`.
- [ ] `gate_contract` passes; clippy `-D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the files above. OUT: `schema.rs`, `parquet.rs`, the `calls`/`imports` resolution paths, the markdown/mcp extract paths, other crates, the analyst POC. Do NOT change `resolve_singleton_bare_target`'s existing arms — only add the new pre-emptive relational block in `resolve_bare_pending_edge`.

**Implementation:**

- [ ] **Step 1: Failing test.** Create `crates/spur-graph/tests/range_resolution_edges.rs` (model the `build_facts` harness on `tests/rust_implements_edge.rs`). `GraphEdge` has `.relation`, `.target_node_id: Option<NodeId>`, `.target_label`; `GraphNode` has `.node_id`, `.kind: NodeKind`, `.label`:

```rust
use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

// INVARIANT: no implements/extends edge resolves to an out-of-range kind.
#[test]
fn relational_edges_never_resolve_out_of_range() {
    // `Send` here is a local enum variant — the std supertrait name collides with it.
    let facts = build("enum Marker { Send }\ntrait Base {}\ntrait Derived: Base + Send {}\nstruct S;\nimpl Default for S {}\n");
    let kind_of = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.kind);
    for e in &facts.edges {
        if matches!(e.relation, RelationKind::Implements | RelationKind::Extends) {
            if let Some(tid) = e.target_node_id {
                assert!(
                    matches!(kind_of(tid), Some(NodeKind::Trait) | Some(NodeKind::Interface) | Some(NodeKind::Class)),
                    "{:?} edge resolved to out-of-range kind {:?} (target_label={:?})",
                    e.relation, kind_of(tid), e.target_label
                );
            }
        }
    }
    // Specifically: extends→Send must NOT bind the enum variant.
    assert!(
        !facts.edges.iter().any(|e| e.relation == RelationKind::Extends
            && e.target_node_id.is_some()
            && kind_of(e.target_node_id.unwrap()) == Some(NodeKind::EnumVariant)),
        "extends bound a std marker name to a local enum variant"
    );
}

// POSITIVE: an in-range local supertrait still resolves.
#[test]
fn local_supertrait_still_resolves() {
    let facts = build("trait Base {}\ntrait Derived: Base {}\n");
    let base = facts.nodes.iter().find(|n| n.label == "Base" && n.kind == NodeKind::Trait).expect("Base trait");
    assert!(
        facts.edges.iter().any(|e| e.relation == RelationKind::Extends
            && e.target_node_id == Some(base.node_id)),
        "local supertrait Base should still resolve; edges: {:?}",
        facts.edges.iter().filter(|e| e.relation == RelationKind::Extends)
            .map(|e| (e.target_node_id, e.target_label.clone())).collect::<Vec<_>>()
    );
}
```

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test range_resolution_edges` → expect the invariant test to FAIL (today `Send`/`Default` bind to the enum variant).

- [ ] **Step 2: Add the relational block + helpers** in `tree_sitter.rs`. Insert this block in `resolve_bare_pending_edge` AFTER the `Imports` block (after line ~740) and BEFORE the generic `ambiguous_symbols_by_label` path (line ~742):

```rust
    if let Some(allowed) = relational_target_kinds(edge.relation) {
        // implements/extends must resolve to a type-like target. A std/marker
        // trait (Send/Sync/Default/…) has no worktree definition, so rather than
        // binding a same-named enum_variant/section we leave the edge unresolved.
        let candidates = relational_symbol_candidates(builder, edge, indexes, allowed);
        match candidates.as_slice() {
            [target] if *target != edge.source => {
                builder.add_pending_edge(edge, Some(*target));
                return;
            }
            cands if cands.len() > 1 => {
                *ambiguous_unresolved += 1;
                builder.add_pending_edge(edge, None);
                return;
            }
            _ => {
                builder.add_pending_edge(edge, None);
                return;
            }
        }
    }
```

Add these helpers near `callable_symbol_candidates` (model `relational_symbol_candidates` on it verbatim, swapping the kind filter):

```rust
fn relational_target_kinds(relation: RelationKind) -> Option<&'static [NodeKind]> {
    match relation {
        RelationKind::Implements => Some(&[NodeKind::Trait, NodeKind::Interface]),
        RelationKind::Extends => Some(&[NodeKind::Trait, NodeKind::Interface, NodeKind::Class]),
        _ => None,
    }
}

fn relational_symbol_candidates(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
    allowed: &[NodeKind],
) -> Vec<NodeId> {
    let mut candidates = builder
        .symbol_index
        .get(&edge.target_name)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .filter(|target| *target != edge.source)
        .filter(|target| {
            indexes
                .node_kind_by_id
                .get(target)
                .copied()
                .is_some_and(|kind| allowed.contains(&kind))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}
```

Run Step 1 again → expect PASS (both tests).

- [ ] **Step 3: Re-bless the golden artifacts (expected change).** Some implements/extends edges now move resolved→unresolved, so the 4 corpus goldens change. Regenerate and commit them:

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green without bless
```

Sanity-check the golden diff: it should ONLY flip some `implements`/`extends` edges from a resolved target to unresolved (target dropped, label kept) — NO `calls`/`contains`/`defines` edges should change. If anything else changed, STOP and emit `risk`.

- [ ] **Step 4: Broad regression gate** (green except flaky `incremental_ingest`, which you must NOT run):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test range_resolution_edges --test extractor \
  --test resolver --test rust_implements_edge --test rust_extends_edge --test ts_inheritance_edges \
  --test cpp_inheritance_edges --test python_inheritance_edges --test defines_edges
```

If a `resolver.rs` assertion legitimately changes because an implements/extends edge is now correctly unresolved, update that specific assertion (in scope) and note it. A change to a `calls`/`contains`/`defines` resolution is a real regression → STOP, emit `risk`.

- [ ] **Step 5: Lint + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs \
        crates/spur-graph/tests/range_resolution_edges.rs \
        crates/spur-graph/tests/fixtures/  # re-blessed goldens
# plus tests/resolver.rs if you updated an assertion
git commit -m "fix(spur-graph): range-constrain implements/extends resolution"
```

Report: which goldens changed, the resolved→unresolved edge delta, and any resolver.rs assertion updated.

## Self-Review

- **Spec coverage:** Implements §6 domain/range as resolver constraints — the practical fix for the confirmed misresolution (extends 57/58, implements 82/333 wrong-kind).
- **Placeholder scan:** concrete block + helpers (verbatim mirror of `callable_symbol_candidates`) + concrete tests.
- **Type consistency:** `NodeKind::{Trait,Interface,Class}`, `RelationKind::{Implements,Extends}`, `PendingResolutionIndexes.node_kind_by_id`, `builder.symbol_index` all already used in the same file; no schema change.
- **DAG:** single task.
- **Risk:** resolver change; bounded to implements/extends only, with the golden re-bless budgeted in-scope (no scope-drift), the diff-is-relational-only check, and a `risk` checkpoint for any calls/contains/defines change.
