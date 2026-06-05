# Tier-2 Pillar 1 — Import-path resolution (the keystone) Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. **3-task DAG.**

**Source:** Tier-2 design spec `docs/superpowers/specs/2026-06-05-code-graph-tier2-fusion-design.ipynb`
(commit 5da24d07), §2 Pillar 1. Grounded on the live v12 + JS-extractor artifact (`9b1fef99`, 50,439
nodes).

**The defect (exact, grounded):** the extractor records an import by its **bare final segment**. The
edge queries capture `@import.name` = the last identifier, so `use crate::extract::schema::RelationKind`
becomes `target_label = "RelationKind"` and the disambiguating path is discarded
(`crates/spur-graph/queries/{rust,python,typescript}/spur-edges.scm`). `import_resolution_candidates`
(`extract/tree_sitter.rs:1189`) therefore matches only the bare name against the symbol index; when that
name has >1 workspace definition, the dispatch marks it ambiguous and drops it.

**Live evidence (9,686 unresolved imports):** 5,666 external (no workspace def → Pillar 2); **81**
bare-name-unique (recoverable today, marginal); **3,939 ambiguous bare names** that are *structurally
unreachable* without the path. The 3,939 are this plan's target. (Downstream, Pillar 3 / Frontier B
spends the resolved imports for ~4,933 licensable cross-crate calls — **out of scope here**.)

**The boundary this plan respects:** Tier-1 precision is closed. Path resolution is *more* precise than
bare-name (it binds only what the path names), so it **strictly raises recall without new phantom risk**,
and the v9 language-family gate still applies to the final bind. Single-language corpora ⇒ predictable,
additive golden churn (newly-resolved import edges only).

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.
🧪 **Two-corpus precision check** (used throughout Tier-1) is the safety net: after each task, gated bind
arms stay **0 cross-crate** and all arms stay **0 cross-language** on both the SPUR and `anywidget`
graphs.

**Out of scope (whole plan):** external-symbol modeling (Pillar 2), import-licensed cross-crate recall
(Pillar 3/B), receiver-type inference, the 5,666 external imports, and the 81 bare-name-unique cleanup
(C-lite — fold in opportunistically only).

---

### Task P1.1 — Capture the full import path (extractor + schema)

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

**Depends on:** none

**Acceptance Criteria:**
- [ ] Every `imports` edge in the goldens carries `import_path` = the full source path text
      (e.g. `crate::extract::schema::RelationKind`, `./helpers.mjs`, `os.path`), while `target_label`
      stays the bare segment. Non-import edges are unchanged.
- [ ] `PendingEdge.import_path` is `Some(_)` for imports with a scoped/dotted/relative path and `None`
      for bare single-segment imports; resolution behavior is **unchanged** (same resolved/unresolved
      counts as today — this task only adds a field).
- [ ] Unit test `import_path_capture_records_full_path` (per language: a Rust `use a::b::C`, a Python
      `from a.b import C`, a TS `import { C } from "./m"`) asserts the captured `import_path`.
- [ ] `SCHEMA_VERSION` + `EXTRACTOR_VERSION` bumped; goldens re-blessed; the **only** golden change is the
      additive `import_path` field on import edges (resolved targets unchanged). If a non-import edge or
      any resolved target changes, STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** per-language capture confirmed; golden delta is import_path-only; version bumps.

**Suggested Worker:** codex. **Scope Boundary:** IN: path capture + `PendingEdge`/schema field + version
bumps + tests + bless. OUT: any resolver change (P1.2), re-export (P1.3), other relations.

---

### Task P1.2 — Module-path resolver (consume the path) — depends on P1.1

**Task ID:** `task-p1-module-path-resolver`

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — a new `module_path_resolution_candidates`
  (or extend `import_resolution_candidates`, line 1189) that, when `edge.import_path` is `Some`, resolves
  the path against the workspace module/file tree and returns the **unique** path-named symbol; falls back
  to today's bare-name behavior when `import_path` is `None`. Stamp resolved path-binds with
  `bind_method = "import_path"`.
- Modify: `crates/spur-graph/src/store/build.rs` — bump `RESOLVER_VERSION` (line 29); add `"import_path"`
  to the `resolution_is_stamped` skip set in `rebind_cross_file_edges`.
- Regenerate (bless — expect newly-resolved imports): fixtures.

**Depends on:** `task-p1-import-path-capture`

**Acceptance Criteria:**
- [ ] Path segments handled: Rust `crate::`/`super::`/`self::`/crate-name roots; Python dotted +
      relative (`.`,`..`) modules; JS/TS relative (`./`,`../`) + bare module specifiers. The resolver maps
      a path to the file/module that defines the final segment and binds to that unique symbol.
- [ ] An import whose bare name is **ambiguous** but whose **path** names exactly one workspace symbol now
      **resolves** (`bind_method="import_path"`); an import whose path leaves the workspace stays
      **unresolved** (it is Pillar 2's job, not a phantom).
- [ ] **Precision preserved (two-corpus):** gated arms 0 cross-crate, all arms 0 cross-language, on SPUR
      **and** `anywidget`. The language-family gate applies to the final path-bind (no cross-language import bind).
- [ ] Unit + behavioral tests: a fixture with the same bare type name defined in two modules, imported by
      full path in a third — asserts the import binds to the path-named one, not dropped as ambiguous.
- [ ] `RESOLVER_VERSION` bumped; rebind skip entry added; goldens re-blessed (only import edges gain
      targets); clippy clean; suite green except flaky `incremental_ingest`.
- [ ] **Report:** count of imports newly resolved per corpus; precision two-corpus check; v-bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: path-consuming resolver + `import_path` bind_method
+ rebind skip + `RESOLVER_VERSION` + tests + bless. OUT: re-export chains (P1.3), external symbols
(Pillar 2), non-import relations, the extractor (done in P1.1).

---

### Task P1.3 — Re-export following — depends on P1.2

**Task ID:** `task-p1-reexport-following`

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — when a path resolves to a re-export
  (`pub use a::B` in Rust; `export { B } from "./m"` / `export * from` in TS/JS; `from .m import B` re-export
  in Python `__init__`), follow the chain to the original definition. **Bounded** depth + **cycle-guarded**
  (visited set); on ambiguity or depth-exceeded, leave unresolved (never guess).
- Modify: `crates/spur-graph/src/store/build.rs` — bump `RESOLVER_VERSION`.
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
- [ ] `RESOLVER_VERSION` bumped; goldens re-blessed; clippy clean; suite green except flaky test.
- [ ] **Report:** re-export hops resolved per corpus; cycle/ambiguity handling proven; v-bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: bounded cycle-guarded re-export following +
`RESOLVER_VERSION` + tests + bless. OUT: external symbols (Pillar 2), import-licensed cross-crate recall
(Pillar 3/B), non-import relations.

---

## Self-Review
- **Coverage:** P1.1 captures the path (extractor/schema), P1.2 consumes it (resolver), P1.3 follows
  re-exports — together they make the 3,939 ambiguous imports reachable and lay the router Pillar 2 needs.
- **DAG:** strict chain P1.1 → P1.2 → P1.3 (each consumes the prior's output; no parallelism, by design —
  the resolver cannot run before the path exists).
- **Risk:** path resolution only *narrows* what an import can bind to, so recall rises with no new phantom
  class; the v9 language gate + two-corpus precision check are the guardrails; re-export following is
  explicitly bounded + cycle-guarded. Version-bump discipline (SCHEMA in P1.1, EXTRACTOR in P1.1, RESOLVER
  in P1.2/P1.3) forces clean re-derives.
- **Boundary:** strictly Pillar 1; Pillars 2 (external symbols) and 3 (Frontier B) are separate plans.
