# Ontology Tier-0 P2 — Per-relation algebra metadata Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-spec-live-evidence.ipynb` (§5 Piece 4, P2).

**Grounding (read the schema):** `RelationKind` (`crates/spur-graph/src/schema.rs:282`) is a plain enum
with no associated algebra; `GraphEdge` (`schema.rs:203`) carries only `directed: bool`. There is
nowhere a consumer can ask "what is `calls`'s inverse?", "is `contains` transitive?", or "is
`implements` many-to-many?". This metadata is the input Tier 3 (inference: `called_by` views,
transitive reachability) and Tier 4 (governance) consume, so its absence blocks the upper ladder.

**Goal:** Declare per-relation **algebra metadata** as first-class, queryable data on `RelationKind`:
its **inverse** (a virtual reverse-edge label such as `called_by`/`contained_by`; inverses are NOT new
`RelationKind` variants), **cardinality**, and **transitivity**. Pure declaration + accessor + tests —
no change to extraction, resolution, or the persisted artifact schema.

**Scope:** `schema.rs` ONLY (plus its test module). Behavior-neutral: this adds a lookup table and an
accessor; it does NOT add edge columns, change serialization, or alter any edge's data.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task p2: per-relation algebra metadata

**Task ID:** `task-p2-relation-algebra`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/schema.rs` (new `RelationMetadata` struct, `RelationCardinality`
  enum, and `impl RelationKind { pub fn metadata(self) -> RelationMetadata }`; plus a test module case)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A `pub fn metadata(self) -> RelationMetadata` on `RelationKind` returns, for every variant:
      `inverse_label: Option<&'static str>`, `cardinality: RelationCardinality`, `transitive: bool`.
- [ ] The match is **exhaustive** over all `RelationKind` variants (compiler-enforced — no wildcard
      arm, so a future variant forces a deliberate metadata decision).
- [ ] Concrete, correct values at least for: `Calls.inverse_label == Some("called_by")`;
      `Contains.transitive == true` and `Contains.inverse_label == Some("contained_by")`;
      `Implements.cardinality == ManyToMany`; `Imports.inverse_label == Some("imported_by")`;
      `Extends.transitive == true`.
- [ ] A unit test asserts the above key facts and that `metadata()` is total (calling it on every
      variant does not panic and yields the expected shape).
- [ ] No change to `GraphEdge` fields, `parquet.rs` serialization, the resolver, or any golden
      artifact. `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib` green; clippy `-D warnings`
      clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: `schema.rs` additions (struct + enum + accessor + test). OUT: persisting the
metadata into the Parquet/DuckDB artifact, exposing it via the MCP/analyst layer, the resolver,
`GraphEdge` shape, other crates. Those are Tier-3 follow-ups. Do NOT add new `RelationKind` variants
for inverses — inverses are labels, not variants.

**Implementation:**

- [ ] **Step 1: Types.** In `schema.rs`, near `RelationKind`, add:

```rust
/// Cardinality of a predicate: how many objects one subject may relate to (and back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationCardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}

/// Tier-0 algebra of a predicate. Inverses are virtual reverse-edge labels (e.g. `called_by`),
/// NOT distinct `RelationKind` variants. Consumed by Tier-3 inference (inverse/transitive views).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationMetadata {
    pub inverse_label: Option<&'static str>,
    pub cardinality: RelationCardinality,
    pub transitive: bool,
}
```

- [ ] **Step 2: Accessor.** Add `impl RelationKind { pub fn metadata(self) -> RelationMetadata { match self { ... } } }`
  with an **exhaustive** arm per variant. Suggested values (worker may refine with justification in the
  diff): `Imports`{inv:`imported_by`,ManyToMany,false}; `Calls`{inv:`called_by`,ManyToMany,false};
  `Constructs`{inv:`constructed_by`,ManyToMany,false}; `Contains`{inv:`contained_by`,OneToMany,**true**};
  `Implements`{inv:`implemented_by`,**ManyToMany**,false}; `Defines`{inv:`defined_in`,OneToMany,false};
  `References`{inv:`referenced_by`,ManyToMany,false}; `Extends`{inv:`extended_by`,ManyToMany,**true**};
  `Links`{inv:`linked_from`,ManyToMany,false}; `Touches`{inv:`touched_by`,ManyToMany,false}.

- [ ] **Step 3: Failing-first test** in the `schema.rs` test module:

```rust
#[test]
fn relation_metadata_declares_algebra() {
    use RelationKind::*;
    assert_eq!(Calls.metadata().inverse_label, Some("called_by"));
    assert!(Contains.metadata().transitive);
    assert_eq!(Contains.metadata().inverse_label, Some("contained_by"));
    assert_eq!(Implements.metadata().cardinality, RelationCardinality::ManyToMany);
    assert_eq!(Imports.metadata().inverse_label, Some("imported_by"));
    assert!(Extends.metadata().transitive);
    // totality: every variant returns metadata without panic
    for r in [Imports, Calls, Constructs, Contains, Implements, Defines, References, Extends, Links, Touches] {
        let _ = r.metadata();
    }
}
```

  (Write the test first so it fails to compile/assert before Step 2 is complete.)

- [ ] **Step 4: Gate + commit.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib relation_metadata
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/schema.rs
git commit -m "feat(spur-graph): declare per-relation algebra metadata (inverse/cardinality/transitivity)"
```

  Report the final metadata table and confirm the match is exhaustive (no wildcard arm).

## Self-Review
- **Spec coverage:** §5 Piece 4 P2 — predicate algebra becomes first-class, unblocking Tier 3-4.
- **Placeholder scan:** concrete struct/enum/accessor/test; values enumerated.
- **Type consistency:** additive to `schema.rs`; exhaustive match is compiler-enforced; no `GraphEdge`
  or serialization change.
- **DAG:** single task.
- **Risk:** minimal — pure additive declaration, behavior-neutral, no artifact/golden impact.
