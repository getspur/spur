# Tier-2 Pillar 1 — Import resolution: candidate hygiene + path resolution Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. **4-task strict-chain DAG.**

**Source:** Tier-2 design spec `docs/superpowers/specs/2026-06-05-code-graph-tier2-fusion-design.ipynb`
(commit 5da24d07), §2 Pillar 1. Re-grounded on the live v12 + JS-extractor artifact (`9b1fef99`,
50,439 nodes) by a 3-phase review (code-explore + spur-analyst + first-principles) that **decomposed the
9,686 unresolved imports** — a decomposition the first draft of this plan omitted, and which materially
re-sequences the work.

**The defect (exact, grounded):** the extractor records an import by its **bare final segment**. The edge
queries capture `@import.name` = the last identifier, so `use crate::extract::schema::RelationKind`
becomes `target_label = "RelationKind"` and the disambiguating path is discarded
(`crates/spur-graph/queries/{rust,python,typescript}/spur-edges.scm`). `import_resolution_candidates`
(`extract/tree_sitter.rs:1189`) matches only that bare name against `symbol_index`, filtered by
`is_import_resolution_candidate_kind` (tree_sitter.rs:1225). That kind-filter **deliberately keeps
`Impl`, `EnumVariant`, and `Constant`** in the candidate set, which silently inflates candidate counts
and suppresses recall.

**Live decomposition of the 9,686 unresolved imports (the number that drives sequencing):**
imports already resolve **4,961** edges today via the bare-name closed-world singleton policy. The 9,686
unresolved split into three populations with *different* fixes:

| Bucket | Sites | Names | Cause | Fix |
|---|---|---|---|---|
| **A** — no importable workspace type by that bare name (external, or collides only with `enum_variant`/`constant`: `Path`, `Value`, `Command`, `Result`, `fs`, `Write`, `Error`…) | 6,403 | 943 | genuinely external; path leaves the workspace | **Pillar 2** — correctly stays unresolved here |
| **B** — exactly **one** importable workspace type, in **one** file, dropped only because `impl`/`enum_variant`/`constant` shadows push the candidate set above 1 (`SessionId` ×216, `SpurEvent` ×107, `BrainSessionId` ×81, `RelationKind` ×48…) | **2,255** | 253 | over-broad candidate kind-filter | **P1.0 — candidate hygiene (NO path needed)** |
| **C** — a real workspace type defined in **2+ files** (`McpCallbackServer` across 12 files, `ReactTrace`, `Tier`, `Plan`…) | **1,028** | 173 | bare name is genuinely ambiguous | **P1.1 + P1.2 — path resolution** |

**Two corrections this re-sequencing encodes vs. the first draft:**
1. The path-only target is **~1,028 sites / 173 names (bucket C)**, *not* 3,939. The earlier "3,939
   ambiguous, structurally unreachable without the path" conflated B+C and inflated the path target ~4×.
2. The **larger** recoverable population — bucket B, ~2,255 sites — needs **no path, no schema change, no
   extractor change**: a resolver-only candidate-set hygiene fix recovers it under the *same* closed-world
   singleton policy that already resolves the 4,961 imports today. The cheapest change recovers the most
   recall, so it ships **first**.

**The boundary this plan respects:** Tier-1 precision is closed. Both candidate hygiene (P1.0) and path
resolution (P1.2) only *narrow* what an import can bind to, so they **strictly raise recall without a new
phantom class**; the v9 language-family gate still applies to every bind. P1.0 is precision-consistent
with the established import-singleton policy; the path (P1.2) makes bucket B *strictly more precise* when
it lands (an external `use other::SessionId` carries a path that fails the workspace module match → stays
unresolved) and supersedes P1.0's bare bind — nothing is wasted.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.
🧪 **Two-corpus precision check** (used throughout Tier-1) is the safety net: after each task, gated bind
arms stay **0 cross-crate** and all arms stay **0 cross-language** on both the SPUR and `anywidget` graphs.

**Out of scope (whole plan):** external-symbol modeling (Pillar 2) and the 6,403 bucket-A imports,
import-licensed cross-crate recall (Pillar 3/Frontier B), and receiver-type inference.

---

### Task P1.0 — Import candidate-set hygiene (resolver-only, no path) — bucket B

**Task ID:** `task-p1-import-candidate-hygiene`

**The fix:** narrow `import_resolution_candidates` so a unique workspace type stops being falsely
"ambiguous":
- **Collapse `impl X` onto the same-name type defined in the same file** — an `impl` block is never an
  independent import target; it is the same type as its `struct`/`enum`/`trait`. (Live proof: `SessionId`
  = `{struct, impl}` in one file; `RelationKind` = `{enum, impl}` in one file — each one type, dropped
  only by the impl shadow.)
- **Give type-like kinds precedence over `enum_variant`/`constant` shadows:** when ≥1 type-like candidate
  (`module`/`function`/`struct`/`enum`/`trait`/`class`/`interface`/`type_alias`/`macro`) exists for the
  bare name, exclude `enum_variant`/`constant` candidates (they only shadow). Keep `enum_variant`/
  `constant` as candidates **only when no type-like candidate exists**, so a legitimate
  `use my_enum::Variant` / `use mod::CONST` still resolves.
- After narrowing: exactly one candidate → resolve as today; >1 → ambiguous unresolved (defer to P1.2's
  path); 0 → unresolved.

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — narrow `import_resolution_candidates`
  (line 1189) and/or `is_import_resolution_candidate_kind` (line 1225) per the rule above; add unit +
  behavioral tests.
- Modify: `crates/spur-graph/src/store/build.rs` — bump `RESOLVER_VERSION` (line 29) →
  `"2026-06-05-import-candidate-hygiene-v13"`.
- Regenerate (bless — expect additive import-edge churn only): `crates/spur-graph/tests/fixtures/*/expected_graph_index.json`.

**Depends on:** none

**Acceptance Criteria:**
- [ ] An import whose bare name has **exactly one** type-like workspace definition (after collapsing
      `impl` onto its same-file type and dropping `enum_variant`/`constant` shadows) now **resolves**;
      bucket-A names (no type-like def — `Path`, `Value`, `Command`, `Result`, `fs`) stay **unresolved**;
      bucket-C names (2+ files) stay **ambiguous/unresolved** (P1.2's job).
- [ ] `use my_enum::Variant` (no same-named type) still resolves to the `enum_variant` — the type-like
      precedence is a *fallback*, not a removal of enum-variant/constant import support.
- [ ] **Precision preserved (two-corpus):** all arms 0 cross-language and gated arms 0 cross-crate on SPUR
      **and** `anywidget` — narrowing candidates can never add a cross-boundary bind.
- [ ] **Unit test** `import_candidate_hygiene_collapses_impl_shadow`: a `struct X` + `impl X` in one file,
      imported by bare name elsewhere → candidate set is unique → resolves to the struct.
- [ ] **Behavioral `build_facts` test** `import_resolves_unique_type_despite_enum_variant_shadow`: a type
      `Foo` defined once, plus an unrelated `SomeEnum::Foo` variant in another file, imported by bare name
      → resolves to the type `Foo`, not dropped as ambiguous.
- [ ] `RESOLVER_VERSION` bumped to v13; goldens re-blessed; the **only** golden change is newly-resolved
      import edges (no non-import edge changes; no resolved target flips). If any non-import edge or any
      previously-resolved target changes, STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** count of imports newly resolved on SPUR (expect **on the order of ~2,200 sites /
      ~250 names**); two-corpus precision check; v13 confirmation.

**Suggested Worker:** codex. **Scope Boundary:** IN: the import candidate-set narrowing +
`RESOLVER_VERSION` + tests + bless. OUT: any extractor/schema/`.scm` change (P1.1), path resolution
(P1.2), re-export (P1.3), non-import relations, other crates.

---

### Task P1.1 — Capture the full import path (extractor + schema) — depends on P1.0

**Task ID:** `task-p1-import-path-capture`

**Files:**
- Modify: `crates/spur-graph/queries/{rust,python,typescript}/spur-edges.scm` — additionally capture the
  full scoped path of each import as `@import.path` (the `scoped_identifier`/`scoped_use_list` parent for
  Rust; the dotted `dotted_name`/`relative_import` for Python; the module `string_fragment` + specifier
  for TS/JS). Keep `@import.name` (bare segment) unchanged for back-compat. JavaScript reuses the TS
  queries (TSX grammar), so the TS query change covers `.js/.jsx/.mjs/.cjs` automatically.
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — add `import_path: Option<String>` to
  `PendingEdge` (line ~26); populate it from the `@import.path` capture during edge extraction; thread it
  onto the emitted edge. (No resolution behavior change in this task.)
- Modify: `crates/spur-graph/src/extract/schema.rs` (+ store/build serialization) — add an optional
  `import_path` field to the persisted edge record so the path survives into the artifact.
- Modify: `crates/spur-graph/src/store/build.rs` — bump `EXTRACTOR_VERSION` (line 27) **and**
  `SCHEMA_VERSION` (line 26, new field).
- Regenerate (bless — expect churn on import edges only): `crates/spur-graph/tests/fixtures/*/expected_graph_index.json`.

**Depends on:** `task-p1-import-candidate-hygiene` (sequencing/merge-safety: both touch
`tree_sitter.rs` + `build.rs` version lines; no semantic dependency, but the strict chain avoids worktree
conflicts on the version constants).

**Acceptance Criteria:**
- [ ] Every `imports` edge in the goldens carries `import_path` = the full source path text
      (e.g. `crate::extract::schema::RelationKind`, `./helpers.mjs`, `os.path`), while `target_label`
      stays the bare segment. Non-import edges are unchanged.
- [ ] `PendingEdge.import_path` is `Some(_)` for imports with a scoped/dotted/relative path and `None`
      for bare single-segment imports; resolution behavior is **unchanged** vs. P1.0 (same resolved/
      unresolved counts — this task only adds a field).
- [ ] Unit test `import_path_capture_records_full_path` (per language: a Rust `use a::b::C`, a Python
      `from a.b import C`, a TS `import { C } from "./m"`) asserts the captured `import_path`.
- [ ] `SCHEMA_VERSION` + `EXTRACTOR_VERSION` bumped; goldens re-blessed; the **only** golden change is the
      additive `import_path` field on import edges (resolved targets unchanged vs. P1.0). If a non-import
      edge or any resolved target changes, STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** per-language capture confirmed; golden delta is import_path-only; version bumps.

**Suggested Worker:** codex. **Scope Boundary:** IN: path capture + `PendingEdge`/schema field + version
bumps + tests + bless. OUT: any resolver change (P1.2), re-export (P1.3), other relations.

---

### Task P1.2 — Module-path resolver (consume the path) — depends on P1.1 — bucket C

**Task ID:** `task-p1-module-path-resolver`

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — a new `module_path_resolution_candidates`
  (or extend `import_resolution_candidates`, line 1189) that, when `edge.import_path` is `Some`, resolves
  the path against the workspace module/file tree and returns the **unique** path-named symbol; falls back
  to P1.0's narrowed bare-name behavior when `import_path` is `None`. Stamp resolved path-binds with
  `bind_method = "import_path"`.
- Modify: `crates/spur-graph/src/store/build.rs` — bump `RESOLVER_VERSION` (line 29) →
  `"2026-06-05-import-path-resolver-v14"`; add `"import_path"` to the `resolution_is_stamped` skip set in
  `rebind_cross_file_edges`.
- Regenerate (bless — expect newly-resolved imports): fixtures.

**Depends on:** `task-p1-import-path-capture`

**Acceptance Criteria:**
- [ ] Path segments handled: Rust `crate::`/`super::`/`self::`/crate-name roots; Python dotted +
      relative (`.`,`..`) modules; JS/TS relative (`./`,`../`) + bare module specifiers. The resolver maps
      a path to the file/module that defines the final segment and binds to that unique symbol.
- [ ] A **bucket-C** import (bare name ambiguous across 2+ files) whose **path** names exactly one
      workspace symbol now **resolves** (`bind_method="import_path"`); an import whose path **leaves the
      workspace** stays **unresolved** (bucket A — Pillar 2's job, not a phantom). **Precision guard:** the
      workspace-module-match + v9 language gate are exactly what keep bucket-A names (`Path`, `Value`,
      `Result` colliding with workspace `enum_variant`s) from phantom-binding to std/external symbols —
      call this out in the report.
- [ ] Bucket B is re-derived through the path at **higher precision** than P1.0's bare bind (path-qualified
      in-workspace imports keep resolving; any bare bind that was actually external now correctly drops).
- [ ] **Precision preserved (two-corpus):** gated arms 0 cross-crate, all arms 0 cross-language, on SPUR
      **and** `anywidget`.
- [ ] Unit + behavioral tests: a fixture with the same bare type name defined in two modules, imported by
      full path in a third — asserts the import binds to the path-named one, not dropped as ambiguous.
- [ ] `RESOLVER_VERSION` bumped to v14; rebind skip entry added; goldens re-blessed (only import edges gain
      targets); clippy clean; suite green except flaky `incremental_ingest`.
- [ ] **Report:** count of bucket-C imports newly resolved per corpus (**expect ~1,000 path-only on SPUR**,
      plus bucket B re-derived); two-corpus precision check; v14 bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: path-consuming resolver + `import_path` bind_method
+ rebind skip + `RESOLVER_VERSION` + tests + bless. OUT: re-export chains (P1.3), external symbols
(Pillar 2), non-import relations, the extractor (done in P1.1), the candidate filter (done in P1.0).

---

### Task P1.3 — Re-export following — depends on P1.2

**Task ID:** `task-p1-reexport-following`

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — when a path resolves to a re-export
  (`pub use a::B` in Rust; `export { B } from "./m"` / `export * from` in TS/JS; `from .m import B` re-export
  in Python `__init__`), follow the chain to the original definition. **Bounded** depth + **cycle-guarded**
  (visited set); on ambiguity or depth-exceeded, leave unresolved (never guess).
- Modify: `crates/spur-graph/src/store/build.rs` — bump `RESOLVER_VERSION` →
  `"2026-06-05-reexport-following-v15"`.
- Regenerate (bless): fixtures.

**Depends on:** `task-p1-module-path-resolver`

**Acceptance Criteria:**
- [ ] A re-exported import (`use crate::prelude::Foo` where `prelude` does `pub use crate::real::Foo`)
      resolves to the **original** `Foo` definition, with a documented bounded depth (e.g. ≤8) and a
      cycle guard that leaves pathological chains unresolved rather than looping.
- [ ] Glob re-exports (`pub use a::*` / `export * from`) resolve a bare imported name to the unique
      re-exported symbol when unambiguous; ambiguous globs stay unresolved.
- [ ] **Precision preserved (two-corpus)** as in P1.2; no cross-language / unlicensed cross-crate bind.
- [ ] Unit + behavioral tests for: a single re-export hop, a 2-hop chain, a cycle (must terminate
      unresolved), and an ambiguous glob (must stay unresolved).
- [ ] `RESOLVER_VERSION` bumped to v15; goldens re-blessed; clippy clean; suite green except flaky test.
- [ ] **Report:** re-export hops resolved per corpus; cycle/ambiguity handling proven; v15 bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: bounded cycle-guarded re-export following +
`RESOLVER_VERSION` + tests + bless. OUT: external symbols (Pillar 2), import-licensed cross-crate recall
(Pillar 3/B), non-import relations.

---

## Self-Review
- **Coverage & ordering (cost↑ as recall↓):** P1.0 (resolver-only, ~2,255 sites, cheapest) → P1.1 (path
  capture; schema+extractor) → P1.2 (path resolver, ~1,028 bucket-C sites + bucket-B at higher precision)
  → P1.3 (re-export). The cheapest change recovers the most recall and ships first; the expensive
  schema/extractor bump (P1.1) is deferred to where the path is genuinely load-bearing (bucket C).
- **DAG:** strict chain P1.0 → P1.1 → P1.2 → P1.3. P1.0→P1.1 is a sequencing/merge-safety edge (shared
  `build.rs` version lines + `tree_sitter.rs`); P1.1→P1.2→P1.3 are true semantic dependencies (resolver
  cannot consume a path before it exists; re-export builds on path resolution).
- **Risk:** every task only *narrows* candidates, so recall rises with no new phantom class. P1.0 is
  precision-consistent with the 4,961 imports already bound by the closed-world singleton policy; the path
  (P1.2) makes bucket B strictly more precise and the workspace-module-match guards bucket A from
  phantom-binding. The v9 language gate + two-corpus precision check are the standing guardrails;
  re-export following is bounded + cycle-guarded. Version-bump discipline (RESOLVER in P1.0/P1.2/P1.3,
  SCHEMA+EXTRACTOR in P1.1) forces clean re-derives.
- **Boundary:** strictly Pillar 1; bucket A (6,403 external imports) is Pillar 2 and Frontier B is Pillar 3
  — separate plans.
