# Artifact-level domain/range invariant test Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`.

**Why:** Tier-0's domain/range guards live in THREE resolution paths — the structural
resolver (`extract/tree_sitter.rs`), the cross-file rebind pass
(`store/build.rs::rebind_cross_file_edges`), and the qualified-`fqn` path. Each was fixed
separately, and each bug hid for days **because the unit tests only exercised `build_facts`,
not the assembled artifact** (`artifact_from_facts`, which runs `rebind_cross_file_edges`
internally). This task converts the manual DuckDB verification I ran after every rebuild into
a standing CI guard: assert the invariant over the **post-assembly artifact** across all four
code corpora, so any future resolver/rebind change that reintroduces an out-of-kind bind fails
the suite immediately.

**This is a regression guard, NOT a bug fix.** The production code is already correct on
`main` (live graph verified: 0 out-of-range across all languages, manifest `e4c3b9b8`). The
new test therefore **passes on the first run** — there is no failing-first step and you must
**NOT** change any production code to manufacture one.

**The invariant (mirror the resolver/rebind guards, union kind-sets):**
- `Extends`    → resolved target `symbol_kind` ∈ `{"trait","interface","class"}`
- `Implements` → resolved target `symbol_kind` ∈ `{"trait","interface"}`
- `Calls`      → resolved target `symbol_kind` ∈ `{"function","method"}`
  (Constructs is a *separate* relation, so a `Calls` edge never legitimately points at a
  struct/enum_variant/class — those are reclassified to `Constructs` upstream.)

Only edges with a **resolved** `target_stable_symbol_id = Some(_)` are checked; unresolved
edges (label kept, target `None`) are correct and skipped.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.

---

### Task artifact-range-invariant: assert domain/range over the assembled artifact

**Task ID:** `task-artifact-range-invariant`

**Files (all in scope):**
- Create: `crates/spur-graph/tests/artifact_range_invariants.rs` (new integration test)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A new integration test builds the assembled artifact (`artifact_from_facts`, i.e. AFTER
      `rebind_cross_file_edges`) for the `sample_corpus` (rust), `python_corpus`,
      `typescript_corpus`, and `cpp_corpus` fixtures and asserts: every `Extends`/`Implements`/
      `Calls` edge with a resolved `target_stable_symbol_id` points at a target whose
      `symbol_kind` is in the allowed set above.
- [ ] On a violation, the assertion message names the corpus, the offending edge's relation,
      the resolved target's `symbol_kind`, and the edge's `target_label` (so a future
      regression is diagnosable from the failure alone).
- [ ] The test **passes on current `main`** with no production change.
- [ ] No production source changed, no golden fixture changed, `RESOLVER_VERSION` NOT bumped.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test artifact_range_invariants`
      green; `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings` clean.

**Suggested Worker:** codex.

**Scope Boundary:** IN: the one new test file only. OUT: any `src/**` change, `RESOLVER_VERSION`,
golden fixtures, `schema.rs`, other crates. If you believe a production change is needed to make
the test pass, STOP and emit a `risk` signal — the test passing on unmodified `main` is the whole
point (it certifies the guards already hold).

**Implementation:**

- [ ] **Step 1: New test file.** Create `crates/spur-graph/tests/artifact_range_invariants.rs`.
  Reuse the established pattern from `tests/extractor.rs` (lines 16-57, 384-438): the corpus
  roots are trivial `CARGO_MANIFEST_DIR/tests/fixtures/<corpus>` paths; replicate the four you
  need locally (do NOT import from `extractor.rs` — integration test files are separate crates).

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use spur_graph::build_facts;
use spur_graph::store::build::artifact_from_facts;
use spur_graph::RelationKind;

fn corpus_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Allowed resolved-target `symbol_kind`s per guarded relation. Mirrors the resolver +
/// `rebind_cross_file_edges` union kind-sets. `None` = relation not range-checked here.
fn allowed_target_kinds(relation: RelationKind) -> Option<&'static [&'static str]> {
    match relation {
        RelationKind::Extends => Some(&["trait", "interface", "class"]),
        RelationKind::Implements => Some(&["trait", "interface"]),
        RelationKind::Calls => Some(&["function", "method"]),
        _ => None,
    }
}

#[test]
fn assembled_artifact_has_no_out_of_range_resolved_edges() {
    // (corpus dir, language label for diagnostics)
    let corpora = [
        ("sample_corpus", "rust"),
        ("python_corpus", "python"),
        ("typescript_corpus", "typescript"),
        ("cpp_corpus", "cpp"),
    ];

    let mut violations: Vec<String> = Vec::new();

    for (dir, lang) in corpora {
        let root = corpus_root(dir);
        let facts = build_facts(&root, None)
            .unwrap_or_else(|e| panic!("extract {lang} corpus: {e:?}"))
            .0;
        // artifact_from_facts runs rebind_cross_file_edges internally — this is the
        // assembled (post-rebind) artifact the CLI actually persists.
        let artifact = artifact_from_facts(&facts, &root)
            .unwrap_or_else(|e| panic!("assemble {lang} artifact: {e:?}"));

        let kind_by_id: HashMap<&str, &str> = artifact
            .symbols
            .iter()
            .map(|s| (s.stable_symbol_id.as_str(), s.symbol_kind.as_str()))
            .collect();

        for edge in &artifact.edges {
            let Some(allowed) = allowed_target_kinds(edge.relation) else {
                continue;
            };
            let Some(target_id) = edge.target_stable_symbol_id.as_deref() else {
                continue; // unresolved edges are correct; skip
            };
            let Some(target_kind) = kind_by_id.get(target_id).copied() else {
                continue; // dangling id (shouldn't happen); not this test's concern
            };
            if !allowed.contains(&target_kind) {
                violations.push(format!(
                    "[{lang}] {:?} edge resolves to out-of-range kind {target_kind:?} \
                     (target_label={:?}, allowed={allowed:?})",
                    edge.relation, edge.target_label
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "assembled artifact contains out-of-range resolved edges:\n{}",
        violations.join("\n")
    );
}
```

  **Field-name caveat:** the snippet uses `artifact.symbols[].{stable_symbol_id, symbol_kind}`,
  `artifact.edges[].{relation, target_stable_symbol_id, target_label}` — verified against
  `tests/extractor.rs:312-318, 345-350`. If a field name differs in the artifact type you
  import, fix the accessor to match the real struct (use `code_read_symbol` / a quick
  `cargo` build error to confirm); do NOT change the struct.

- [ ] **Step 2: Run + gate.**

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph --test artifact_range_invariants
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-graph -- -D warnings
```

  Expect the test to **pass** (guards already hold on `main`). If it FAILS, do not patch the
  test to go green — the failure means a real out-of-range bind exists; STOP and emit `risk`
  with the violation lines.

- [ ] **Step 3 (optional self-check, then REVERT).** To prove the guard actually bites, you may
  temporarily revert the v5 qualified-`fqn` arm in `src/extract/tree_sitter.rs` (the
  `else { self.add_pending_edge(&edge, None) }` back to an unconditional `fqn` bind), confirm
  the new test FAILS, then `git checkout` that file to restore. This is a throwaway local check —
  **no production change may appear in the commit.**

- [ ] **Step 4: Commit** (only the new test file):

```bash
git add crates/spur-graph/tests/artifact_range_invariants.rs
git commit -m "test(spur-graph): assert domain/range invariant over assembled artifact"
```

  Report: confirmation the test passes on unmodified `main`, the per-corpus resolved-edge counts
  it checked (e.g. how many Extends/Implements/Calls edges were asserted), and that clippy is clean.

## Self-Review
- **Coverage:** closes the test-surface gap that let all three domain/range bypasses hide —
  asserts the invariant on the *assembled* artifact (post-rebind), not just `build_facts`.
- **Placeholder scan:** concrete, self-contained test; reuses the proven `build_facts` +
  `artifact_from_facts` corpus pattern from `extractor.rs`.
- **Type consistency:** `RelationKind`, `artifact.symbols`/`edges` field accessors verified
  against `extractor.rs`.
- **DAG:** single task.
- **Risk:** test-only, additive, behavior-neutral — no `src/**`, no golden, no `RESOLVER_VERSION`.
  Passes on unmodified `main` by design; `risk` off-ramp if it doesn't.
