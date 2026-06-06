# Tier-2 Pillar 3 — Import-licensed cross-crate call recall Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan(persist_as_epic=true)`. **4-task strict-chain DAG.**

**Source:** Tier-2 design spec `docs/superpowers/specs/2026-06-05-code-graph-tier2-fusion-design.ipynb`,
§3 Pillar 3 (Frontier B). Re-grounded on the **freshly reindexed post-Pillar-2 artifact** (`4e09e9e9`,
51,290 nodes, 107,368 resolved edges; 415 External nodes live, `import_path` = 2,801, external imports =
5,840) via a 3-phase review (code-explore + spur-analyst + first-principles/sequential-thinking).

**What Pillar 3 is.** Pillar 1 made imports resolve by path; Pillar 2 gave the symbols we don't own typed
`External` identity. Pillar 3 **spends the now-resolved imports as licenses**: if file *F* imports symbol
*S* from crate *X*, then a bare call to *S* inside *F* is *licensed* to bind cross-crate — the import is
the explicit evidence that disambiguates what closed-world bare-name matching refused. This is the bridge
from "a precise map of intra-crate calls" to "a precise map of how crates actually call each other."

**The exact seam (grounding).** Cross-crate calls die at **two refusal points** inside
`rebind_cross_file_edges` (`crates/spur-graph/src/store/build.rs:1133`):

| # | Site | Cause | P3 conversion |
|---|---|---|---|
| **(a)** | `build.rs:1237` `matches.len() > 1` | bare callee name ambiguous across crates → `target=None` | import names exactly one candidate → bind it |
| **(b)** | `build.rs:1253-1260` single match but `function_singleton_safe()` false | `tree_sitter.rs:2629` hard-returns false for `src_crate != tgt_crate` | import names that exact symbol → license the cross-crate bind |

`function_singleton_safe` (`tree_sitter.rs:2629`) is THE closed-world cross-crate refusal: same-file or
same-crate-same-family ⇒ true, **cross-crate ⇒ false**. Pillar 3 relaxes it **only when an import
licenses the call**. New arm `bind_method = "import_licensed"`.

**The live structural baseline (drives the build order):**

| Signal | Live (`4e09e9e9`) | Implication |
|---|---|---|
| Unresolved calls (headline) | 88,496 | dominated by std/method/builtin noise, NOT P3-addressable |
| Cross-crate **ambiguous** callees | 993 names / ~26,833 sites | the closed-world-refused population (point a) |
| Cross-crate **workspace** imports (license pool) | **750** — *all unstamped bare-name* | `import_path` cross-crate = **0** |
| `import_path` binds | 805 intra / **0 cross** | the underscore↔hyphen / extern-crate-name gap |
| Dropped `use spur_acp::X` imports | 95 names / **373 sites** | recoverable cross-crate licenses (P3.1) |
| **Realized licensable call sites today** | **73** (11 callees) | gated by import **supply**, not call volume |
| — of which method-target | **0** | name-equality excludes methods (receiver-type = Tier-3) |

**Two findings that shape this plan.** (1) **`import_path` produces zero cross-crate licenses today** —
the 750 cross-crate licenses are all *unstamped bare-name singleton* binds (weak provenance), and 373 more
`use <crate>::Sym` imports are dropped entirely because the resolver never maps the leading segment
`spur_acp` → the workspace crate dir `crates/spur-acp/`. (2) **P3 is a high-precision sliver, not a
recall-mass pillar** — it binds only cross-crate **free-function** calls (low hundreds, growing with
supply); method-name calls need receiver-type inference (Tier-3) and are naturally excluded by the
name-equality join. The pillar's value is *quality* — the cross-crate call edges refactor-impact and
architecture-drift analysis specifically need ("does `spur-mcp` actually *call* into `spur-graph`?").

**Two architectural facts established by grounding:**
1. **The resolver has no per-file import index.** It holds `workspace_index` (external classification) and
   a **global** `symbols_by_entity_name` (the very map that is ambiguous). Licensing requires a new
   *file-scoped* resolved-import index: `file → {imported entity_name → resolved target sid(s)}`.
2. **`rebind_cross_file_edges` is a single pass over buckets→edges.** Import and call edges mutate in
   place interleaved, so a call cannot reliably see an import resolved later in iteration order. Licensing
   demands the import index be **complete before any call licensing decision** → a two-phase split of the
   one function (resolve imports → then resolve calls consulting the index).

**The boundary this plan respects (additive by construction).** P3 only ever acts on call/reference edges
that are **still unresolved after the existing arms** (the two refusal branches set `target=None`); it
never touches an already-resolved or already-stamped edge (those hit `resolution_is_stamped`/`skip_rebind`
and `continue` at `build.rs:1160-1191`). So the count of resolved calls that exist today is **a lower
bound that cannot decrease** — only previously-`None` edges may become `import_licensed`. Recall rises with
no new phantom class; the language-family gate still applies.

⚠️ **`tests/incremental_ingest.rs` is FLAKY** — do NOT run it, do NOT fix it.
🧪 **Two-corpus precision check** (the Tier-1/Pillar-1/Pillar-2 safety net): after each task, gated bind
arms stay **0 cross-language** and every `import_licensed` edge is **witness-backed** (a same-file import
of the same name), on both the SPUR and `anywidget` (`/Volumes/Projects/anywidget`, the polyglot Python+JS
proof) graphs. `anywidget` is the cross-language stressor — a same-name import must NOT license a call
across a Python↔JS boundary.
🔁 **Bless goldens** only with `SPUR_GRAPH_BLESS=1 SPUR_REMOTE=0 scripts/spur-cargo test -p spur-graph
--test extractor`.

**Out of scope (whole plan):** receiver-type inference / method-name cross-crate calls (Tier-3), calls
into External **bodies** (Tier-3 — P3 is strictly workspace↔workspace; External nodes are import targets,
never call targets here), cross-repo identity (Tier-3+). Pillar 3 stops at "resolved imports license
cross-crate **function**-call binds + the cross-crate-call query layer."

---

### Task P3.0 — File-scoped resolved-import index + two-phase resolver split (no recall change)

**Task ID:** `task-p3-import-index`

**The change:** split `rebind_cross_file_edges` into Phase 1 (resolve imports as today, **and** record a
`FileImportIndex`) then Phase 2 (resolve calls/references), with **no graph-behavior change yet** — the
index is built but not consulted. The keystone: it makes licensing order-independent and TDD-able.

**Files:**
- Modify: `crates/spur-graph/src/store/build.rs` — refactor `rebind_cross_file_edges`
  (line 1133) into two passes over the buckets. Phase 1 resolves `Imports` edges exactly as today and, for
  each resolved import, records into a new `FileImportIndex`: `source_file → { imported entity_name →
  Vec<resolved RebindTarget> }` (capture target `stable_symbol_id`, `file_path`, `symbol_kind`). **Only
  workspace targets** are indexed (skip `external` binds — they are never workspace-call licenses). Phase 2
  resolves the remaining relations (`Calls`/`References`/…) exactly as today, with the index **passed in but
  not consulted**.
- (No version bump — pure structural refactor, no content emitted; the index is dead until P3.2.)

**Depends on:** none

**Acceptance Criteria:**
- [ ] **No behavior change:** building every fixture produces byte-for-byte identical goldens (imports and
      calls resolve exactly as before the split). If any golden changes, STOP and emit `risk`.
- [ ] Unit test: after Phase 1 over a 2-file fixture (file A `use crateB::foo`), `FileImportIndex` for
      A maps `foo → [resolved sid in crate B]`; external imports are **absent** from the index.
- [ ] `FileImportIndex` is deterministic (stable ordering) and contains only workspace targets.
- [ ] Full `-p spur-graph` suite green except flaky `incremental_ingest`; clippy `-D warnings` clean
      (force remote from sandbox: `SPUR_REMOTE=1 scripts/spur-cargo clippy`).
- [ ] **Report:** the two-phase structure; index population counts on the SPUR fixtures; confirmation of
      zero golden delta.

**Suggested Worker:** codex. **Scope Boundary:** IN: the two-phase split + `FileImportIndex` construction
+ tests. OUT: consuming the index / any licensing (P3.2), cross-crate import resolution (P3.1), version
bumps, query surface (P3.3), `.scm`, other crates.

---

### Task P3.1 — Cross-crate import resolution (workspace crate-name normalization) — depends on P3.0

**Task ID:** `task-p3-xcrate-import-supply`

**The change:** resolve `use <workspace_crate>::path::Sym` imports to their workspace target by mapping the
leading path segment to the crate directory (underscore↔hyphen normalization), stamping them
`import_path`. Grows the cross-crate license **supply** (750 → ~1,123) **and** upgrades the existing 750
from weak unstamped provenance to precise `import_path` — the provenance the licensing arm depends on.

**Files:**
- Modify: `crates/spur-graph/src/store/build.rs` — extend the import resolution path (Phase 1 from P3.0;
  `import_rebind_candidates`, line 1279, and/or `import_workspace_index_from_buckets`, line 1334) so a
  leading import segment that normalizes (`replace('-','_')`) to a known workspace crate directory
  (`crates/<name>/`) is resolved against that crate's symbols by the **remaining path**, not the bare final
  segment. Precision-safe: the full path `spur_acp::error::AcpError` uniquely identifies crate + symbol —
  narrowing, never widening. Reuse the existing language-family gate.
- (No `SCHEMA_VERSION` bump — no parquet column. `RESOLVER_VERSION` bump deferred to P3.2 so a single
  re-derive covers both resolver changes; P3.1 + P3.2 land as one semantic resolver delta.)

**Depends on:** `task-p3-import-index`

**Acceptance Criteria:**
- [ ] `use spur_graph::build_facts` (or equivalent fixture) **resolves cross-crate** to the workspace
      symbol, stamped `bind_method="import_path"`, and appears in the `FileImportIndex`.
- [ ] **Narrowing only:** a leading segment that is NOT a workspace crate (`serde`, `std`, `react`) is
      untouched (stays External per P2 / unresolved); a bare-name collision with no disambiguating path
      still refuses. If any previously-resolved import flips target, STOP and emit `risk`.
- [ ] **Additivity:** resolved **intra-crate** imports unchanged; the delta is only previously-unresolved
      `use <ws_crate>::…` sites becoming `import_path` cross-crate binds (expect ≈373 sites recovered on
      SPUR, and the 750 unstamped cross-crate imports now stamped `import_path`).
- [ ] **Two-corpus precision:** gated arms 0 cross-crate phantom, all arms 0 cross-language, on SPUR and
      `anywidget`.
- [ ] Full `-p spur-graph` suite green except flaky test; clippy clean.
- [ ] **Report:** cross-crate `import_path` count before/after per corpus; the additivity check; the
      crate-name normalization rule.

**Suggested Worker:** codex. **Scope Boundary:** IN: cross-crate workspace import resolution + crate-name
normalization + tests. OUT: the call-licensing arm (P3.2), the index plumbing (P3.0), External
classification (P2, do not touch), query surface (P3.3), non-import relations.

---

### Task P3.2 — The `import_licensed` cross-crate call arm (the recall mover) — depends on P3.1

**Task ID:** `task-p3-licensed-arm`

**The change:** consume the `FileImportIndex` at the two refusal points to bind cross-crate **function**
calls the closed-world resolver refused — justified by an explicit import, never a guess. This is the only
recall-moving task.

**Files:**
- Modify: `crates/spur-graph/src/store/build.rs` — in Phase 2 of `rebind_cross_file_edges`:
  - **Refusal (a)** (`matches.len() > 1`, line 1237): before leaving unresolved, look up
    `FileImportIndex[source_file][target_label]`; if it names **exactly one** sid that is a member of the
    kind-filtered `matches`, bind to it, `bind_method="import_licensed"`.
  - **Refusal (b)** (single match but `function_singleton_safe`/`same_directory_path` false, lines
    1253-1260): if `FileImportIndex[source_file][target_label]` names exactly that `resolved.stable_symbol_id`,
    the import licenses the cross-crate bind → `bind_method="import_licensed"`; else keep today's refusal.
  - **Precision contract (all must hold):** licensed candidate kind ∈ `{function}` (methods excluded —
    receiver-type is Tier-3; the name-equality join already excludes them, assert it); same language family
    (imports are same-family by construction — assert); the witnessing import must be a **resolved
    workspace** import (not external). Never license a `CallsDyn` edge.
  - Bump `RESOLVER_VERSION` (line 34) → `"2026-06-06-import-licensed-cross-crate-v17"`. Add
    `"import_licensed"` to the `resolution_is_stamped` skip set (line 1160 region) so a later rebind never
    re-touches a licensed edge.
- Regenerate (bless — expect newly-resolved cross-crate call edges only):
  `crates/spur-graph/tests/fixtures/*/expected_graph_index.json`.

**Depends on:** `task-p3-xcrate-import-supply`

**Acceptance Criteria:**
- [ ] **Additivity invariant (headline gate):** every call edge resolved before P3.2 keeps its exact target
      AND `bind_method`; only previously-`None` call edges may become `import_licensed`. If any prior bind
      changes, STOP and emit `risk`.
- [ ] **Witness-backed:** every `import_licensed` edge has a same-file resolved workspace import of the same
      `entity_name` (the license). A behavioral test asserts this property across the built SPUR graph.
- [ ] Fixture: file A `use crateB::foo; foo()` where `foo` is **ambiguous** across crates → the call binds
      cross-crate to crate B's `foo` (`import_licensed`); the SAME call in a file that does **not** import
      `foo` stays unresolved.
- [ ] **Method/cross-language exclusion:** a `use crateB::Type` + `x.method()` call is **not** licensed
      (method target); a cross-language same-name import never licenses a call (anywidget Python↔JS).
- [ ] **Two-corpus precision:** gated arms 0 cross-crate phantom, all arms 0 cross-language, on SPUR and
      `anywidget`.
- [ ] `RESOLVER_VERSION` bumped to v17; `"import_licensed"` added to the skip set; goldens re-blessed (only
      new cross-crate call edges); clippy clean; suite green except flaky test.
- [ ] **Report:** `import_licensed` edges bound per corpus (expect low hundreds on SPUR, function-targets
      only); the additivity + witness-backed checks; two-corpus precision; v17 bump.

**Suggested Worker:** codex. **Scope Boundary:** IN: the import-licensed call arm + `RESOLVER_VERSION` +
skip-set entry + tests + bless. OUT: the index plumbing (P3.0), import resolution (P3.1), query surface
(P3.3), methods/receiver-type inference, non-call relations, External call targets.

---

### Task P3.3 — Cross-crate-call query surface + precision gate — depends on P3.2

**Task ID:** `task-p3-xcrate-query`

**The change:** make the cross-crate call graph observable (the spec §4 payoff question) and lock the
precision gate as a standing assertion.

**Files:**
- Modify: the analyst views applied by `analyst build`
  (`crates/spur-context/poc/duckdb-analyst/init.sql`, the single-sourced embedded SQL) — add a
  `v_cross_crate_calls` view (resolved `calls` where source crate ≠ target crate, exposing `bind_method`
  and the source/target crate + symbol) so "does `spur-mcp` call into `spur-graph`?" is one query, and
  surface `import_licensed` in the bind-method provenance. Optionally label these edges in the DuckPGQ
  `code` property graph.
- Modify: `crates/spur-cli/tests/analyst_temporal_views.rs` (or a sibling analyst CLI test) — assert the
  new view builds and returns rows; add an `init.sql`-defines-`v_cross_crate_calls` unit assertion.
- Modify: `crates/spur-graph/src/schema.rs` — extend the `bind_method` taxonomy doc comment to include
  `import_licensed` (the Pillar 3 arm).
- (No graph `RESOLVER_VERSION`/`SCHEMA_VERSION` bump — read-side + view-only + doc.)

**Depends on:** `task-p3-licensed-arm`

**Acceptance Criteria:**
- [ ] `v_cross_crate_calls` exists, builds during `analyst build`, and returns sane rows on SPUR (e.g.
      `spur-mcp → spur-graph` edges appear with `bind_method` populated).
- [ ] The spec §4 demo query ("does crate X call into crate Y, or just import it?") runs and distinguishes
      `import_licensed` cross-crate calls from imports-only relationships.
- [ ] `code_callers`/`code_callees` surface `import_licensed` cross-crate edges (verify on the re-derived
      artifact — read-only, no rebuild needed; the tools return `bind_method` already).
- [ ] **Standing two-corpus gate test:** an assertion (test or documented check) that on SPUR + `anywidget`
      every `import_licensed` edge is witness-backed and there are 0 cross-language licensed edges.
- [ ] Relevant `-p spur-cli` / analyst tests green; clippy clean.
- [ ] **Report:** top cross-crate call pairs on SPUR by `import_licensed` count; the gate result.

**Suggested Worker:** codex. **Scope Boundary:** IN: cross-crate-call analyst view + provenance surfacing +
taxonomy doc + tests. OUT: any resolver/extractor/schema-column change (P3.0–P3.2), Pillar 1/2 surfaces,
Tier-3 frontiers.

---

## Self-Review
- **Coverage & ordering:** P3.0 (file-scoped import index + two-phase split; zero behavior change,
  independently tested) → P3.1 (cross-crate import resolution; grows + precisely stamps the license supply)
  → P3.2 (the `import_licensed` arm; the only recall mover, gated by additivity + witness-backed) → P3.3
  (cross-crate-call query surface + standing precision gate). Cheapest/safest first; the one
  content-changing task (P3.2) lands behind a proven index and a precise supply.
- **DAG:** strict chain P3.0 → P3.1 → P3.2 → P3.3. P3.0→P3.1 is both merge-safety (shared `build.rs`) and
  semantic (P3.1's resolved imports must land in the P3.0 index). P3.1→P3.2 is a true semantic dep (the arm
  consumes the supply). P3.2→P3.3 is a true semantic dep (the surface consumes licensed edges). A single
  `RESOLVER_VERSION` bump in P3.2 forces one clean re-derive covering the P3.1+P3.2 resolver delta.
- **Risk:** the highest-precision risk is licensing a *wrong* cross-crate target — guarded by requiring the
  import to name exactly one candidate present in the kind-filtered match set, the function-only kind gate,
  the same-language-family gate, the workspace-only (non-external) witness requirement, and the two-corpus
  gate at every task. Additivity (only fills `None` edges) means P3 cannot regress any existing bind. The
  two-phase split (P3.0) removes the single-pass order fragility that would otherwise make licensing
  nondeterministic.
- **Boundary:** strictly Pillar 3 (resolved imports license cross-crate **function**-call binds +
  cross-crate-call views). Receiver-type inference / method-name cross-crate calls, External-body calls,
  and cross-repo identity (Tier-3+) are out of scope. Realistic yield is a high-precision sliver (low
  hundreds, function-targets only), gated by import supply — the quality cross-crate call graph for
  refactor-impact and architecture-drift, not a recall-mass pillar.
