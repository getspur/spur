# Tier-2 Pillar 2 — External-symbol modeling Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. **4-task strict-chain DAG.**

**Source:** Tier-2 design spec `docs/superpowers/specs/2026-06-05-code-graph-tier2-fusion-design.ipynb`,
§3 Pillar 2. Re-grounded on the **freshly reindexed post-Pillar-1 artifact** (`e02ed227`, 50,712 nodes,
101,162 resolved edges; `import_path` now materialized — 2,790 edges carry `bind_method=import_path`) via a
3-phase review (code-explore + spur-analyst + first-principles/sequential-thinking).

**What Pillar 1 left behind (the live re-baseline that drives this plan).** P1 drained buckets B and C from
the unresolved import pool exactly as designed; what remains is **bucket A — external symbols**:

| Bucket | Pre-P1 | Post-P1 (live `e02ed227`) | Status |
|---|---|---|---|
| **A · external** (no importable workspace type by that bare name) | 943 names / 6,403 sites | **977 names / 6,611 sites** | ← **Pillar 2 target** |
| C · multi-file workspace type | 173 / 1,028 | 87 / 520 | P1.1/P1.2 resolved ~half (residue) |
| B · unique workspace type | 253 / 2,255 | 12 / 77 | P1.0 resolved 97% (residue) |

Bucket A is now the dominant unresolved import population. Each site dies today at the terminal
`add_pending_edge(resolution_edge, None)` branch of import resolution
(`crates/spur-graph/src/extract/tree_sitter.rs:892`) → an `edges_unresolved` row, a bare text label, no
identity. **Pillar 2 gives each a typed `External(origin)` node — identity without a body —
de-duplicated across all sites that import it.**

**Bucket A by language family (live, the build-order driver):**

| family | sites | % | names |
|---|---|---|---|
| **Rust** | 5,518 | 84% | 554 |
| **JS/TS** | 963 | 15% | 379 |
| **Python** | 123 | 2% | 46 |
| C++ | 7 | — | 7 |

→ the classifier is built **Rust-first** (Rust is 84% and its module-path semantics are already modeled by
P1.1/P1.2), with JS bare-specifiers and Python next.

**The keystone is the boundary classifier, not the node.** Pillar 1's keystone was the path resolver;
Pillar 2's is the **external/internal classifier**. P1 asked "does this path resolve to a *unique workspace
symbol*?" (positive match). P2 asks the *complement*: "did resolution fail because the path **leaves the
workspace** (→ External) or because it is genuinely ambiguous/unresolvable **within** the workspace (→ stay
unresolved)?" Getting this wrong is the only way Pillar 2 can regress precision — either by re-binding the
**collision-trap names** (live: `Path` ×234, `Command` ×90, `Terminal` ×72, `Ordering` ×65, `Write` ×44 —
each collides with a workspace `enum_variant`/`constant`/`section`) to a workspace symbol, or by minting
External nodes for workspace-relative imports. These names are bucket A *precisely because* P1.0 hygiene
excluded those non-type-like kinds — Pillar 2 builds directly on that invariant and must never undo it.

**Two architectural facts established by grounding:**
1. **External is the first synthetic, bodyless node class.** Every existing node is parsed from a file with
   `file_path` + `byte_range` + `line_range` + `anchor_hash` (all non-`Option` in `GraphSymbolArtifact`,
   `schema.rs:157`) and a `stable_symbol_id` derived from `(file, fqn, kind, byte_offset)`. An External node
   has none of these.
2. **Import resolution runs in TWO places** — extract-time (`tree_sitter.rs`) and compose-time rebind
   (`build.rs:import_rebind_candidates`, line 1110). Pillar 1 had to keep both consistent; Pillar 2 inherits
   that constraint — External synthesis must be single-sourced or the rebind will silently re-resolve an
   external edge to a workspace phantom.

**Chosen representation (R-A storage + dedicated views) — stated for review.** Externals are stored as
`nodes` rows with `symbol_kind = "external"`, a sentinel `file_path` (`external://`), zero byte/line
ranges, empty `anchor_hash`, `qualified_name` = the full external path (`serde::Deserialize`),
`entity_name` = the bare name (`Deserialize`); `origin` (the package, `serde`) is **derived** from the
first path segment (no new column). This reuses the entire parquet / `symbol_index` / DuckPGQ / `code_*`
surface — the resolver binds edges to a real `NodeId` with no new id-space — and exposes ergonomic
`external_nodes` / `v_dependency_surface` **views** for the spec's dependency-surface queries. Rejected the
alternative (a separate `external_symbols` artifact table) as far more plumbing for no invariant benefit at
T2; it can be revisited at T3 when externals gain *bodies*. **Because no parquet column is added,
`SCHEMA_VERSION` does NOT bump** (and the analyst `SUPPORTED_GRAPH_SCHEMA_VERSION` stays at v8); the graph
re-derive is forced by the `RESOLVER_VERSION` bump in P2.3.

**The boundary this plan respects.** Tier-1 precision is closed and Pillar 1 is complete. External binding
only ever consumes import edges that are *currently unresolved* (`target = None`), so it is **additive by
construction**: the count of resolved **workspace** imports must be UNCHANGED by Pillar 2 — only the
unresolved→external delta moves. Recall rises with no new phantom class; the language-family gate still
applies.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.
🧪 **Two-corpus precision check** (the Tier-1/Pillar-1 safety net): after each task, gated bind arms stay
**0 cross-crate** and all arms stay **0 cross-language** on both the SPUR and `anywidget`
(`/Volumes/Projects/anywidget`, the polyglot Python+JS proof: `numpy`/`react` externals) graphs.

**Out of scope (whole plan):** external package **bodies** (Tier-3), **receiver-type inference** (Tier-3),
**cross-repo identity** (Tier-3+), and **import-licensed cross-crate CALL recall** (Pillar 3 / Frontier B,
which *consumes* P2's resolved imports as licenses — separate plan). Pillar 2 stops at "imports → typed
External nodes + the dependency-surface query layer."

---

### Task P2.1 — External node class + path-derived identity (schema/identity)

**Task ID:** `task-p2-external-node-schema`

**The change:** introduce the External node class and its deterministic, dedup-able identity — with **no
graph-behavior change yet** (nothing emits externals until P2.3).

**Files:**
- Modify: `crates/spur-graph/src/schema.rs` — add `NodeKind::External` (line ~238) with discriminator
  `"external"` (line ~260). No new fields on `GraphSymbolArtifact` (externals reuse the existing shape with
  sentinel values).
- Modify: `crates/spur-graph/src/identity.rs` — add a helper that derives a deterministic
  `stable_symbol_id` for an external from its **full path** (e.g. `stable_symbol_id_for(file="external://",
  fqn=full_path, kind="external", byte=0)`), so every site importing the same path maps to the **same** id
  (natural dedup). Document the scheme.
- (No `build.rs` version bump — no parquet column changes, no content emitted.)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `NodeKind::External` round-trips through serde (discriminator `"external"`) — a `node_kind_*` test
      modeled on `schema.rs::change_kind_tests`.
- [ ] The external identity helper is **deterministic and collision-free**: same full path → same
      `stable_symbol_id`; different paths → different ids; unit-tested across Rust/JS/Python path shapes.
- [ ] **No behavior change:** building any fixture produces byte-for-byte identical goldens (no externals
      emitted yet). If any golden changes, STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean.
- [ ] **Report:** the External identity scheme; confirmation of zero golden delta.

**Suggested Worker:** codex. **Scope Boundary:** IN: `NodeKind::External` + external identity helper +
tests. OUT: the classifier (P2.2), any synthesis/binding (P2.3), views/tools (P2.4), version bumps, `.scm`,
other crates.

---

### Task P2.2 — External/internal boundary classifier (pure fn) — depends on P2.1

**Task ID:** `task-p2-boundary-classifier`

**The change:** a pure, language-family-gated function over `import_path` (captured by P1.1) + the
workspace crate/module set — the precision keystone. It computes a verdict only; it emits nothing into the
graph in this task.

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — add
  `classify_import_origin(import_path, source_file, workspace_index) -> Internal | External { origin, path,
  name }`. Rules:
  - **Rust:** first segment ∈ {`crate`, `self`, `super`, `$crate`} OR a workspace crate name → **Internal**;
    `std`/`core`/`alloc` (collapse origin → `std`) or any non-workspace crate → **External**.
  - **JS/TS:** specifier starting `./` `../` `/` (or a tsconfig path-alias to a workspace dir) → **Internal**;
    bare specifier (`react`, `@scope/pkg`) → **External**.
  - **Python:** leading-dot relative OR resolves to a workspace module/package → **Internal**; else →
    **External**. Reconcile with the existing `is_python_bare_import_path` (tree_sitter.rs:1468).
- (No version bump — pure fn, no caller yet, no content change.)

**Depends on:** `task-p2-external-node-schema` (merge-safety: shared `tree_sitter.rs`; no semantic dep).

**Acceptance Criteria:**
- [ ] Unit tests over crafted paths per family — including every **collision-trap** name
      (`Path`=`std::path::Path`, `Command`, `Terminal`, `Ordering`, `Write`): each classifies **External by
      path** despite a same-named workspace `enum_variant`/`constant`/`section`.
- [ ] Workspace-relative imports (`crate::`/`super::`/`self::`, `./x`, `from .mod import`) classify
      **Internal**; bare external specifiers classify **External** with the correct `origin`.
- [ ] **Two-corpus sanity:** running the classifier over SPUR and `anywidget` import paths yields 0
      mis-classifications of a known workspace path as External (spot-checked against the workspace crate
      set); no cross-language leakage.
- [ ] **No behavior change:** nothing calls the classifier yet → goldens unchanged. If any golden changes,
      STOP and emit `risk`.
- [ ] Full `-p spur-graph` suite green except flaky test; clippy clean.
- [ ] **Report:** the per-language rules; classifier verdict counts when run (read-only) over the live
      bucket-A names; confirmation of zero golden delta.

**Suggested Worker:** codex. **Scope Boundary:** IN: the pure classifier + tests. OUT: node synthesis /
edge binding (P2.3), views/tools (P2.4), the node schema (P2.1), `.scm`, other relations.

---

### Task P2.3 — Synthesis + dedup registry + bind arm (the recall mover) — depends on P2.2

**Task ID:** `task-p2-external-synthesis-bind`

**The change:** turn unresolved bucket-A imports into edges that target deduplicated synthetic External
nodes — single-sourced so the extract/rebind paths cannot disagree.

**Files:**
- Modify: `crates/spur-graph/src/store/build.rs` — **single-source the synthesis at compose-time rebind**
  (`import_rebind_candidates`, line 1110 / its caller): a registry maps each classified-External full path
  → one synthetic External `NodeId` (created once, deduped), and each currently-unresolved bucket-A import
  edge is upgraded to target it with `bind_method = "external"`. Bump `RESOLVER_VERSION` (line 29) →
  `"2026-06-06-external-symbol-modeling-v16"`. Add `"external"` to the `resolution_is_stamped` skip set so a
  later rebind never re-resolves an external edge to a workspace phantom.
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs` — the External bind arm fires **strictly last**
  (after module-path P1.2, type-like/bare-singleton P1.0, and re-export P1.3 have all declined), i.e. at the
  terminal unresolved branch (line ~892). If synthesis is done here instead of compose-time, it MUST be
  mirror-consistent with the rebind (same classifier + registry).
- Ensure synthetic externals are **excluded from `file_manifests` node lists and the temporal walk** (they
  belong to no real file and have no git history).
- Regenerate (bless — expect newly-targeted import edges + new external nodes only): `crates/spur-graph/tests/fixtures/*/expected_graph_index.json`.

**Depends on:** `task-p2-boundary-classifier`

**Acceptance Criteria:**
- [ ] **Additivity invariant (the headline gate):** the count of resolved **workspace** imports is
      **UNCHANGED** vs. P2.2; every delta is an unresolved bucket-A import becoming an `external` bind. No
      previously-resolved target flips. If any workspace import resolution changes, STOP and emit `risk`.
- [ ] N sites importing the same path (`serde::Deserialize`) bind to **one** shared External node
      (dedup verified); the node carries `symbol_kind="external"`, `qualified_name` = full path,
      `entity_name` = bare name.
- [ ] **Collision-trap proof:** `Path`/`Command`/`Ordering`/`Write` become `External(std::…)` binds, NOT
      re-bound to the workspace `enum_variant`/`constant`/`section` of the same name.
- [ ] **Precision preserved (two-corpus):** gated arms 0 cross-crate, all arms 0 cross-language, on SPUR
      **and** `anywidget`.
- [ ] Synthetic externals are absent from `file_manifests` and `temporal_edges`/`symbol_snapshots`.
- [ ] Unit + behavioral `build_facts` tests: a fixture importing an external path from 2 files → 1 deduped
      External node with 2 inbound `imports` edges (`bind_method="external"`); a workspace import in the same
      fixture stays workspace-resolved.
- [ ] `RESOLVER_VERSION` bumped to v16; `"external"` added to the rebind skip set; goldens re-blessed (only
      new external nodes + newly-targeted import edges); clippy clean; suite green except flaky test.
- [ ] **Report:** external nodes synthesized + import sites bound per corpus (**expect ≈6,611 sites /
      ≈977 names on SPUR**, modulo dedup); the additivity check (workspace imports unchanged); two-corpus
      precision; v16 bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: synthesis + dedup registry + the last-firing external
bind arm + `RESOLVER_VERSION` + rebind skip + tests + bless. OUT: the classifier (P2.2), the node schema
(P2.1), views/tools (P2.4), import-licensed cross-crate CALL recall (Pillar 3), non-import relations.

---

### Task P2.4 — Query surface + tool hardening — depends on P2.3

**Task ID:** `task-p2-dependency-surface`

**The change:** make the bodyless External class safe to read and turn the spec's dependency-surface
questions into one-JOIN queries.

**Files:**
- Modify: `crates/spur-mcp/src/server/handlers/code_graph.rs` — `code_read_symbol`
  (`code_read_symbol_with_client`, line 1198) must **short-circuit on `symbol_kind="external"`**: return the
  origin/path/kind metadata and an explicit "no indexed body — Tier-3" marker, never attempt a
  `file_path`/`byte_range` content read. `code_symbol_search` may return externals but must tag them so they
  don't masquerade as workspace symbols in ranking.
- Modify: the analyst view definitions applied by `analyst build` (see
  `crates/spur-cli/src/commands/analyst.rs` and the `init_*.sql` it applies) — add an `external_nodes` view
  (`SELECT … , regexp_extract(qualified_name,'^[^:./]+',0) AS origin FROM nodes WHERE
  symbol_kind='external'`) and a `v_dependency_surface` view (`crate/file → external origin → external
  symbol`, with inbound import counts) so the spec §4 queries run. Externals must also appear in
  `duckpgq_nodes` so `MATCH (s)-[:imports]->(e:External)` works.
- (No graph `RESOLVER_VERSION`/`SCHEMA_VERSION` bump — read-side + view-only changes.)

**Depends on:** `task-p2-external-synthesis-bind`

**Acceptance Criteria:**
- [ ] `code_read_symbol` on an External target returns origin metadata + the bodyless marker and **does not
      crash / does not read a file**; covered by a handler test.
- [ ] The spec §4 demo queries execute and return sane rows: "which third-party crates does `spur-core`
      depend on, and through which symbols?" (`MATCH (s)-[:imports]->(e:External)`), "blast radius of bumping
      `serde`" (inbound edges to `External(serde::*)` via `origin`).
- [ ] `external_nodes` + `v_dependency_surface` views exist and group correctly by `origin`
      (std/core/alloc collapse to `std`).
- [ ] No `code_*` tool regresses on workspace symbols (externals are additive in search; workspace ranking
      unchanged).
- [ ] Relevant `-p spur-mcp` / analyst tests green; clippy clean.
- [ ] **Report:** the dependency-surface query results on SPUR (top external origins by fan-in); tool
      short-circuit confirmation.

**Suggested Worker:** codex. **Scope Boundary:** IN: bodyless-external tool hardening + analyst
dependency-surface views + tests. OUT: any resolver/extractor/schema change (P2.1–P2.3), Pillar 3, other
relations.

---

## Self-Review
- **Coverage & ordering:** P2.1 (node class + identity; zero behavior change, independently round-trip
  tested) → P2.2 (pure boundary classifier; the precision keystone, unit + two-corpus tested before any
  emission) → P2.3 (synthesis + dedup + last-firing bind arm; the only recall mover, gated by the additivity
  invariant) → P2.4 (read-side surface + tool hardening; the spec's query payoff). Cheapest/safest first; the
  one content-changing task (P2.3) lands behind a proven classifier.
- **DAG:** strict chain P2.1 → P2.2 → P2.3 → P2.4. P2.1→P2.2 is a merge-safety edge (shared
  `tree_sitter.rs`; no semantic dep — schema and classifier are independent). P2.2→P2.3→P2.4 are true
  semantic dependencies (synthesis consumes the classifier; the surface consumes synthesized externals).
- **Risk:** the single highest-precision risk is the collision-trap regression — guarded by the classifier's
  external-by-path rule (P2.2), the last-firing additive bind arm (P2.3), explicit collision-trap tests, and
  the two-corpus gate at every task. Dual-path (extract vs compose rebind) divergence is closed by
  single-sourcing synthesis at compose-time and adding `"external"` to the rebind skip set. The bodyless node
  is the only novel shape — its read-path is hardened in P2.4 and it is kept out of `file_manifests`/temporal
  in P2.3. `RESOLVER_VERSION` bump (P2.3) forces a clean re-derive; no `SCHEMA_VERSION` bump is needed
  (R-A adds no parquet column).
- **Boundary:** strictly Pillar 2 (imports → typed External nodes + dependency-surface views). External
  bodies, receiver-type inference, cross-repo identity (Tier-3) and import-licensed cross-crate CALL recall
  (Pillar 3 / Frontier B) are separate plans.
