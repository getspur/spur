# Ontology Tier-0 P1a — Terminal `calls` range rejection Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Source spec:** `docs/superpowers/specs/2026-06-04-code-graph-ontology-tier0-spec-live-evidence.ipynb` (§4 Piece 3, P1a).

**Grounding (live artifact `a66543657`, spur-analyst):** of resolved `calls` edges, the object kind is
`function 11,660` + `method 8,286` (legit) but also **`field 4,709`**, `module 40`, `section 30`,
`constant 7`, `enum 2`, `macro 1` — all non-callable. These are `.into()`/`.join()`/field-access
**misresolutions**, not invocations.

**Root cause (read end-to-end):** `resolve_singleton_bare_target`
(`crates/spur-graph/src/extract/tree_sitter.rs:838-883`), reached for `calls` via the generic
singleton fallback (`resolve_bare_pending_edge:835`), has a `_ =>` arm that intercepts only
`Calls → {Struct,EnumVariant,Class}` (→ `Constructs`) and then **unconditionally** binds everything
else via `else → add_pending_edge(edge, Some(target))`. So a `calls` edge whose only same-named
singleton is a `field`/`module`/`section`/`constant`/`enum`/`macro` gets bound to it.

**Goal:** Make `calls` resolution **terminal on range**: a `calls` edge resolves only to a callable
(`Function`/`Method`, already handled in the earlier match arms) or reclassifies to `Constructs`
(`Struct`/`EnumVariant`/`Class`, already handled). Any other singleton kind is a misresolution and
must be left **unresolved** (`target_node_id = None`, label retained) — never bound.

📌 **Golden artifacts WILL change** (corpus fixtures contain `.into()`/field-shaped calls). Re-blessing
the 4 corpus goldens is **in scope and budgeted**.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task p1a: terminal calls range rejection

**Task ID:** `task-p1a-calls-terminal`

**Files (all in scope):**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` (one new arm in `resolve_singleton_bare_target`)
- Create: `crates/spur-graph/tests/calls_range_resolution_edges.rs`
- Regenerate (bless, expected): `crates/spur-graph/tests/fixtures/{sample_corpus,python_corpus,typescript_corpus,cpp_corpus}/expected_graph_index.json`
- Modify (only if an assertion legitimately changes): `crates/spur-graph/tests/resolver.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] No resolved `calls` edge has a target whose kind ∉ {Function, Method}. (Struct/EnumVariant/Class
      already reclassify to `Constructs`, so they are not `calls` targets either.)
- [ ] A `calls` whose only singleton is a `field`/`module`/`section`/`constant`/`enum`/`macro` is left
      unresolved (label kept, no node).
- [ ] A genuine call to a local `Function`/`Method` still resolves (no regression to the callable path).
- [ ] Goldens re-blessed; full `-p spur-graph` suite green except the flaky `incremental_ingest`;
      `gate_contract` + clippy `-D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the files above. OUT: the `implements`/`extends` range path (already landed,
P0/phase-5), `schema.rs`, `parquet.rs`, other crates, the analyst POC. Do NOT change the callable
candidate filter or the qualified/scope-match paths — only add the terminal rejection in the `_ =>`
arm of `resolve_singleton_bare_target`.

**Implementation:**

- [ ] **Step 1: Failing test.** Create `crates/spur-graph/tests/calls_range_resolution_edges.rs`
  (model the `build_facts` harness on `tests/range_resolution_edges.rs`). Assert the invariant: no
  `RelationKind::Calls` edge resolves to a target whose `NodeKind` ∉ {Function, Method}; and a
  field-shaped call (e.g. `struct S { f: u32 } fn g(s:S){ let _ = s.f; }` plus a same-named free
  identifier that would single-bind) does not produce a resolved `calls→field`. Include a positive
  case: a real local `fn h(){}` called from `fn c(){ h(); }` still resolves `calls→h`.

  Run `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test calls_range_resolution_edges` →
  expect the invariant to FAIL today.

- [ ] **Step 2: Add the terminal rejection** in `resolve_singleton_bare_target`'s `_ =>` arm, between
  the `Constructs` reclassification and the python-extends reclassification's `else`:

```rust
        _ => {
            if edge.relation == RelationKind::Calls
                && matches!(
                    indexes.node_kind_by_id.get(&target).copied(),
                    Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)
                )
            {
                builder.add_pending_edge_as(edge, Some(target), RelationKind::Constructs);
            } else if should_reclassify_python_extends_as_implements(edge, target, indexes) {
                builder.add_pending_edge_as(edge, Some(target), RelationKind::Implements);
            } else if edge.relation == RelationKind::Calls {
                // P1a: a `calls` edge only resolves to a callable (Function/Method, handled in the
                // earlier arms) or a constructible type (Constructs, handled above). Any other
                // singleton kind (field/module/section/constant/enum/macro) is a misresolution —
                // leave it unresolved rather than binding a non-callable target.
                builder.add_pending_edge(edge, None);
            } else {
                builder.add_pending_edge(edge, Some(target));
            }
        }
```

  Run Step 1 again → expect PASS.

- [ ] **Step 3: Re-bless goldens (expected).** Some `calls→field`/etc. edges move resolved→unresolved:

```bash
SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph --test extractor
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test extractor   # confirm green
```

  Sanity-check the diff: it should ONLY flip some `calls` edges (whose target is a non-callable kind)
  from resolved to unresolved — NO `contains`/`defines`/`implements`/`extends`/`constructs`/`imports`
  edge changes, and no edge's TARGET changes to a different node. If anything else changed, STOP and
  emit `risk`.

- [ ] **Step 4: Broad gate + commit** (green except flaky `incremental_ingest`, do NOT run it):

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --lib
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test calls_range_resolution_edges --test extractor \
  --test resolver --test constructs_edges --test range_resolution_edges --test defines_edges
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
git add crates/spur-graph/src/extract/tree_sitter.rs \
        crates/spur-graph/tests/calls_range_resolution_edges.rs crates/spur-graph/tests/fixtures/
# plus tests/resolver.rs if an assertion legitimately changed
git commit -m "fix(spur-graph): terminally reject non-callable calls targets"
```

  Report: the resolved→unresolved `calls` delta by target kind, which goldens changed, and any
  `resolver.rs` assertion updated. A change to a non-`calls` relation is a regression → STOP, `risk`.

## Self-Review
- **Spec coverage:** §4 P1a — `calls` domain/range made terminal.
- **Placeholder scan:** one concrete arm + concrete tests; golden re-bless budgeted in-scope.
- **Type consistency:** `NodeKind`, `RelationKind::Calls`, `add_pending_edge(edge, None)` all already
  used in the same function.
- **DAG:** single task.
- **Risk:** resolver change bounded to the `_ =>` arm's Calls case; diff-is-calls-only check + `risk`
  off-ramp for any other-relation change.
