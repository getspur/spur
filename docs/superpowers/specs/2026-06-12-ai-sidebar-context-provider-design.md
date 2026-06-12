# AI Sidebar Context Provider — Design Spec

- **Status:** Draft for user review (design direction approved in session)
- **Date:** 2026-06-12
- **Surface:** `crates/spur-notebook/src/mcp/` (tools + server instructions),
  `crates/spur-notebook/src/sidebar_chat/`
- **Related:** `2026-06-12-ai-sidebar-context-lenses-design.md` (companion — UI/lens
  framing), `2026-06-10-spur-graph-jupyter-notebook-support-design.md` (approved —
  notebook container extraction in spur-graph; §6 builds on it),
  `2026-06-09-notebook-sidebar-ai-agent-design.md`
- **Design Epic:** `bd-f1ab`

## 1. Goal

The context-lenses spec gives the AI sidebar turn-level *framing* (view mode +
lens). This spec designs the context *substrate*: what the agent can know about
the notebook's tables, app, cells, and DAG — and how it learns it.

It answers four questions:

1. How are datasource tables linked to the agent's context session?
2. What notebook/Spur App metadata is shared with the agent, in what shape?
3. How are multi-language notebook cells indexed/graphed?
4. How is DAG lineage/context gathered?

## 2. First Principles

The agent needs exactly three capabilities:

| Capability | Meaning |
|---|---|
| **Discover** | Learn what entities exist (tables, cells, ports, app) without the user pasting anything |
| **Navigate** | From any entity, reach related entities deterministically (schema of a table, producer of a port, upstream of a failed node) |
| **Trust** | Everything it reads is version-anchored so staleness is detectable, and never contains credentials |

Under two constraints: **token budget** (context is finite; notebooks and
catalogs are not) and **agent variance** (small models skip optional tool
calls).

### Core decision: pull-first

All context metadata is **queryable through namespaced MCP tools** following one
reference convention. Guidance for navigating it is embedded in the MCP server
itself (server instructions + tool descriptions) and in the app skill. The
**only push** per turn is the lens preamble (owned by the lenses spec) plus one
orientation hint:

```text
Current user perspective: <lens preamble>.
Orient via notebook_context_pack before answering from notebook state.
```

No context cards, no session ledgers, no per-turn metadata dumps. If an agent
never calls tools, behavior degrades exactly to the approved lenses spec — no
worse.

## 3. Reference Convention

Every entity gets a stable, typed ref. Refs are valid MCP resource URIs: tools
accept and return them (required path, works in every ACP client), and the
notebook MCP server additionally mirrors layer-1 entities through
`resources/list` for clients that support MCP resources.

```text
ds://<datasource-id>[/<table>]          datasource tree (depth varies by kind)
cell://<notebook>/<cell-id>[@v<N>]      cell, optionally version-anchored
sym://<notebook>/<cell-id>/<name>       symbol inside a cell (slice 3)
port://<notebook>/<port>[@v<N>]         Arrow port, version from port_manifest
```

Rules:

- `<notebook>` is the daemon's notebook key (URI-encoded absolute path). Tools
  that already take `notebook_path` accept relative refs (`cell://<cell-id>`)
  scoped to that argument.
- `<datasource-id>` is **derived, not stored**: slugified entry name, suffixed
  with a short hash of `(path, kind)` only when names collide. Stable across
  daemon restarts.
- `@v<N>` anchors a ref to a version: port versions come from `port_manifest`,
  cell versions from the cell's `expected_version` counter. Responses always
  emit anchored refs; requests may omit the anchor.
- Every tool response includes `next_queries`: pre-filled `{tool, ref, reason}`
  suggestions (same pattern as `knowledge_context_pack.recommended_next_tools`).
  Standard refs + `next_queries` are what make "the agent knows what to query
  next" deterministic rather than hoped-for.

## 4. Q1 — Tables: Layered Queryable Catalog

### Layer model

Datasource metadata is a tree with **kind-dependent depth** (2 or 3 layers),
self-described by `node_type`:

| Kind | Layer 1 | Layer 2 | Layer 3 |
|---|---|---|---|
| `api_tables` | catalog | connection/provider | table |
| `duck_db`, `sqlite` | catalog | attached file | table |
| `csv`, `parquet`, `json` | catalog | table (file *is* the table) | — |

### Tool: `notebook_catalog`

```jsonc
notebook_catalog({ notebook_path, ref?, scope? })
// ref absent  → layer 1: all datasources for this notebook scope
// ref present → that node + its children (descend one level per call)
// scope: "all" (default) | "used"
```

Response shape:

```jsonc
{
  "notebook_version": 42,
  "ref": "ds://polymarket",
  "node_type": "connection",            // catalog | connection | table
  "name": "Polymarket",
  "kind": "api_tables",
  "status": "connected",                // connected | auth_expired | unreachable | static
  "children": [
    { "ref": "ds://polymarket/markets", "node_type": "table", "name": "markets",
      "invoke": "polymarket_markets()", "columns": 14, "row_count": 3200 }
  ],
  "used_by": [
    { "cell": "cell://a3f1@v7", "via": "dag.source" },
    { "cell": "cell://b2c9@v3", "via": "table_function" }
  ],
  "next_queries": [
    { "tool": "notebook_catalog", "ref": "ds://polymarket/markets",
      "reason": "full column schema and preview hint" }
  ]
}
```

At a `table` leaf the response carries full `columns: [{name, sql_type}]`, the
`invoke` syntax for cells, and a preview pointer (`notebook_preview_api_tables`).

### The table↔session link is derived, never stored

The session is linked to a notebook (`AppScope.app_key`); the notebook is
linked to tables through its cells (`cell.metadata.spur.dag.source` wiring plus
table-function name matches against cell sources). `scope: "used"` computes the
composition at query time. In slice 1 the table-function match is a literal
string search of each catalog `invoke` name over cell sources; slice 3 upgrades
it to the spur-graph extractor's table facts. No session-side ledger or cache exists to drift.
Tables mentioned only in earlier chat turns are not tracked — the transcript is
that memory.

`notebook_list_datasources` and `notebook_preview_api_tables` remain as the
compatibility surface beneath `notebook_catalog`.

## 5. Q2 — App/Notebook Metadata: Orientation Pack

### Tool: `notebook_context_pack`

One orientation call (named after the repo's own `knowledge_context_pack`
pattern) returning every section the agent needs to get grounded, each stamped
with the `notebook_version` it was computed at:

```jsonc
{
  "notebook_version": 42,
  "app": {
    "name": "Code Graph Workbench",       // null section for plain notebooks
    "app_key": "/path/to/app",
    "entry_notebook": "app.ipynb",
    "open_mode": "app",
    "runtime_features": ["frontend-cells", "ports-arrow"],
    "mcp_server": "present",              // present | none — name only, never config
    "skill": "<inline SKILL.md markdown>",
    "skill_path": "skill/SKILL.md"        // app-root-relative
  },
  "notebook": {
    "path": "/path/to/app.ipynb",
    "cells": { "code": 9, "markdown": 4 },
    "languages": { "python": 7, "javascript": 2 },   // from cell.metadata.spur.code_type
    "kernel_slots": [ { "spec": "python3", "state": "alive" },
                      { "spec": "deno", "state": "alive" } ],
    "venv": "default"
  },
  "catalog": {
    "count": 3,
    "nodes": [ { "ref": "ds://polymarket", "name": "Polymarket",
                 "kind": "api_tables", "node_type": "connection",
                 "status": "connected" } ]
  },
  "dag": {
    "nodes": 7, "edges": 8,
    "failed": [ { "ref": "cell://c4d2@v9", "error_excerpt": "KeyError: 'volume'" } ],
    "stale": [ "cell://e1aa@v2" ],
    "frontend_cells": [ { "ref": "cell://f00d@v5", "binds": ["risk"], "emits": ["horizon"] } ]
  },
  "next_queries": [ ... ],
  "truncated": []                         // explicit markers, never silent caps
}
```

### Skill delivery (fixes the dead `AppScope.skill`)

`resolve_app_scope` currently loads the app's SKILL.md into `AppScope.skill`,
which **nothing consumes** — ACP `new_session` has no system-prompt field, so it
silently drops. This spec moves skill delivery pull-side: `notebook_context_pack`
returns the skill text inline (session-scoped knowledge, read once) plus
`skill_path`. The daemon reads the skill from the manifest directly;
`AppScope.skill` is removed as part of this work.

### Budget rule

Each list section is capped (catalog nodes ~12, failed nodes ~5, frontend cells
~8). Anything cut is named in `truncated` with the ref to query deeper. Target
for the whole pack: comfortably under ~1.5k tokens for typical notebooks.

## 6. Q3 — Cells: Symbol-Level Indexing (owned by spur-graph)

Symbol extraction for notebooks is the responsibility of `crates/spur-graph`,
**not** the notebook daemon. The approved spec
`2026-06-10-spur-graph-jupyter-notebook-support-design.md` already defines the
foundation: `.ipynb` is a custom container format with a dedicated parser
(`crates/spur-graph/src/extract/notebook.rs`) that decodes the JSON cells,
resolves each cell's language through the fallback chain
(`cell.metadata.spur.code_type` → cell kernelspec → notebook kernelspec →
`language_info`), and delegates to the tree-sitter grammars spur-graph already
ships (python, tsx, rust, go, julia, r; markdown cells via the md grammar).
Notebooks join code and docs as a **third indexed corpus**: cell symbols land
in the same graph artifact and analyst index and surface through
`code_*`/`knowledge_context_pack` like any other symbol.

### The delta this spec adds: the spur-semantic fact layer

The spur-graph spec extracts *generic* symbols. The spur notebook format also
carries semantics no generic grammar sees — this spec assigns their extraction
to the same notebook extractor:

| Fact | Source | Graph output |
|---|---|---|
| Cell container nodes | nbformat cells | cell node between file node and its symbols (promotes that spec's future-work item) — backs `cell://` refs |
| Actual port I/O | `spur.put("x")` / `spur.get("x")` string literals | port nodes + actual produces/consumes edges |
| Declared DAG | `cell.metadata.spur.dag` | declared produces/consumes/source edges |
| Frontend binding | `cell.metadata.spur.frontend` | binds/emits edges |
| Table references | table-function calls in cell source | cell → `ds://` table edges |
| Drift | declared vs actual port I/O mismatch | drift annotation on the cell node |

Deliberately a flat fact extractor, **not** a compiler: no type resolution, no
intra-cell call graphs. The spur-fact tree-sitter query patterns ship for
**python + javascript first** (the real notebook population: Python kernel
default, Deno app track); other languages get generic symbols from their
grammars immediately and spur-fact patterns only when needed.

### Edge rules (kernel-slot aware)

- **Within one kernel slot** (e.g. all-Python cells): def→use chains by name
  match in document order — an approximation of shared-kernel global semantics.
  Known false-positive mode: re-defined names shadow; document it, don't solve
  it.
- **Across kernel slots**: edges exist **only** through `port://` and `ds://`
  refs — `spur.put("x")` in Python matches `spur.get("x")` in a Deno cell by
  string literal. Language heterogeneity disappears at the data-plane boundary.
- **Drift check**: declared `dag.produces` with no matching `spur.put` in the
  cell's facts (and vice versa) is reported — this catches the most common
  wiring bug in the data-app skill's gotcha table.

### One extractor, two execution modes

Notebooks are working files and need not live in a git workspace, so the same
spur-graph extractor runs in two modes:

- **Batch** — `graph build` discovers repo-resident `.ipynb` files and indexes
  them into the graph artifact + analyst DB (Spur Apps living in a repo get
  full `code_*`/analyst coverage).
- **Live** — the notebook daemon links the spur-graph notebook extractor as a
  library and re-extracts the open notebook on **save/run boundaries**
  (debounced, per-cell content hash — this implements the spur-graph spec's
  "incremental re-extraction" future-work item). Loose notebooks outside any
  repo are covered by this mode alone.

One implementation, two write targets — no duplicated parsing logic in the
daemon. `notebook_context_pack` recomputes lazily on read if the live index is
dirty.

### Tools (slice 3)

Daemon-side query tools serve from the live index; `sym://` refs map 1:1 to
`graph://symbol/<stable_id>` wherever the batch index also covers the file:

```jsonc
notebook_symbol_search({ notebook_path, query, kind? })
  → { matches: [ { ref: "sym://…", name, kind, lang, cell: "cell://…@v" } ], next_queries }

notebook_symbol_refs({ notebook_path, ref })
  → { defined_in, used_by: [cell refs], ports_touched, drift: [...], next_queries }
```

## 7. Q4 — DAG Lineage

Everything required already exists in the engine; lineage is one join exposed
as a walk:

```text
catalog ⋈ dag.source ⋈ DAG edges ⋈ port_manifest ⋈ spur.frontend(binds/emits) ⋈ cell states
```

### Tool: `notebook_lineage`

```jsonc
notebook_lineage({ notebook_path, ref, direction: "upstream"|"downstream"|"both", depth? /*default 3*/ })
```

Accepts any `ds://`, `cell://`, or `port://` ref. Response uses **OpenLineage
vocabulary** (vocabulary only — not the protocol): tables and ports are
*datasets*, cells are *jobs*, executions are *runs* with version facets.

```jsonc
{
  "root": "port://nb/risk@v4",
  "nodes": [
    { "ref": "ds://polymarket/markets", "role": "dataset", "state": "fresh" },
    { "ref": "cell://a3f1@v7", "role": "job", "state": "failed",
      "execution_count": 12, "error_excerpt": "KeyError: 'volume'" },
    { "ref": "port://nb/markets@v12", "role": "dataset", "state": "fresh" }
  ],
  "edges": [
    { "from": "ds://polymarket/markets", "to": "cell://a3f1@v7", "via": "source" },
    { "from": "cell://a3f1@v7", "to": "port://nb/markets@v12", "via": "produces" }
  ],
  "truncated": false,
  "next_queries": [ ... ]
}
```

- **Depth-bounded** (default 3) with a defensive visited-node cap — DAG
  metadata is user-editable, so the walker never trusts acyclicity.
- **Failed jobs carry `error_excerpt`** pulled from the cell's last outputs —
  the single highest-value field for the `dag_ops` lens, whose preamble
  instructs: "start from the failed/stale ref and walk `notebook_lineage`
  upstream."
- Recorded gap: `port_manifest` stores versions, not timestamps. Lineage v1 is
  wiring + versions + states + error excerpts; run timings/timestamps are
  future work in the engine, not this layer.

## 8. Guidance Delivery

Three channels, none of which spend per-turn prompt tokens:

1. **MCP server instructions** (`crates/spur-notebook/src/mcp/mod.rs:178`) —
   extended to carry the navigation contract: the layer model, "every ref is
   queryable", and the orient-first rule. Delivered automatically at session
   init to any ACP agent.
2. **Tool descriptions** — each context tool states its layer and names the
   next tool. This makes discovery self-healing when an agent ignores
   instructions: finding any one tool leads to the others.
3. **App SKILL.md** — app-specific guidance, returned inline by
   `notebook_context_pack` (§5).

## 9. Turn Context Extension

The lenses spec's `ChatTurnContext` gains one optional field — the cheapest,
highest-value "current perspective" datum:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnContext {
    pub notebook_path: String,
    pub view_mode: NotebookViewMode,
    pub lens: ChatLens,
    pub selected_cell_ref: Option<String>,   // cell://<id>@v<N> of focused cell
}
```

When present, the turn preamble appends `Focused cell: <ref>` so "explain this"
resolves to a lookup instead of a guess. The lenses spec owns the rest of the
contract.

## 10. Trust Invariants

- **No credentials in metadata, ever.** Connection nodes expose `status` only —
  never tokens, headers, or connection strings. Enforced by a serialization
  test asserting on full response JSON.
- **Version anchoring everywhere.** Responses emit `@v`-anchored refs; every
  section stamps `notebook_version`.
- **No silent caps.** Every bounded list reports what was cut and the ref to
  query deeper.
- **Derived, not stored.** Usage links (`scope:"used"`), datasource ids, and
  lineage are computed at query time from primary state; the only
  daemon-persisted artifact is the live per-notebook fact index, recomputable
  from the `.ipynb` alone (the batch graph artifact is owned by spur-graph).
- **Stale-ref handling.** A ref to a deleted cell/port resolves to a structured
  error with the nearest valid parent ref, not a bare failure.

## 11. Implementation Slices

1. **Conventions + orientation + catalog**
   - ref parse/format module (`crates/spur-notebook/src/context/refs.rs`)
   - `notebook_context_pack`, `notebook_catalog` MCP tools
   - extend server instructions; remove dead `AppScope.skill`
   - sidebar turn: lens preamble + orient hint; `selected_cell_ref` pass-through
2. **Lineage**
   - `notebook_lineage` walker over existing engine state
   - `dag_ops` lens preamble references it
3. **Spur-semantic fact layer in spur-graph + daemon integration**
   - depends on the `2026-06-10` spur-graph notebook-support plan landing
     (container parser, language fallback chain, grammar delegation)
   - extend `crates/spur-graph/src/extract/notebook.rs`: cell container nodes,
     port/dag/frontend/table facts, drift annotation (python + javascript
     spur-fact patterns first)
   - daemon links the extractor for the live per-notebook index
     (save/run-debounced, per-cell content hash)
   - `notebook_symbol_search`, `notebook_symbol_refs` over the live index;
     `sym://` ↔ `graph://symbol/<id>` mapping where the batch index exists

## 12. Test Plan

Slice 1:
- ref round-trip (parse/format, version anchors, URI-encoded notebook paths)
- catalog depth by kind: csv leaf at layer 2; api_tables full 3 layers
- `scope:"used"` derivation from `dag.source` + table-function matches
- credential-absence assertion over serialized catalog/context-pack JSON
- context pack: section version stamps; caps emit `truncated` markers; skill
  inline for app scope, `app: null` for plain notebooks
- turn prompt prepends lens preamble + orient hint; `ChatTurnContext` with
  `selectedCellRef` serde round-trips camelCase

Slice 2:
- upstream/downstream/both walks; depth bound honored; visited cap survives a
  hand-corrupted cyclic metadata fixture
- failed job nodes carry `error_excerpt` from cell outputs
- node roles follow dataset/job vocabulary

Slice 3 (extraction tests live in `spur-graph`; integration tests in the daemon):
- python cells: defs/uses/puts/gets/tables extraction fixtures
- javascript cells: same fixtures incl. `spur.get` in Deno app cells
- cell container nodes appear between file node and symbols; back `cell://` refs
- same-slot doc-order def→use edges; shadowing fixture documents false positive
- cross-slot edges only via port string-literal match
- drift: declared `produces` without `spur.put` reported (and inverse)
- live mode: save-triggered re-extraction respects per-cell content hashes;
  batch and live extraction of the same notebook agree on symbols/facts
- `sym://` refs round-trip to `graph://symbol/<id>` for repo-resident notebooks

## 13. Non-Goals

- **Push context cards / session ledgers** — rejected on first principles
  (derived-not-stored, pull-first).
- **SQL views over metadata** (analyst-style). Trigger to revisit: a real
  aggregation question over notebook metadata that layered navigation can't
  answer in ≤2 calls.
- **Full OpenLineage protocol/export** — vocabulary only in v1.
- **Spur-fact query patterns for rust/go/julia/r cells** — those languages get
  generic symbol extraction free from their existing grammars (per the
  spur-graph spec); `spur.put/get`/table patterns are deferred until such
  notebooks exist.
- **A daemon-owned parser** — the daemon never reimplements notebook parsing;
  it links the spur-graph extractor (one implementation, two modes).
- **Cross-notebook lineage** for multi-notebook apps — v1 lineage is scoped to
  one notebook.
- **Semantic/embedding retrieval over cells.**
- **Run timestamps/timings in lineage** — requires engine changes, recorded as
  a gap.

## 14. Risks

**Agents that don't call tools.** Floor is the lenses spec behavior. The orient
hint + self-healing tool descriptions are the mitigations; if telemetry shows
systematic non-use, revisit a minimal push summary (one paragraph, not cards).

**Port-name string matching.** Dynamic port names (`spur.put(name_var, df)`)
escape the indexer and cross-slot edge detection. Accepted v1 limitation;
facts record an `opaque_put` marker when the argument isn't a literal.

**Ref stability across notebook restructuring.** Cell ids are stable, but
copy/paste between notebooks mints new ids. Stale-ref handling (§10) is the
containment.

**Token cost of the pack on large notebooks.** Caps + truncation markers bound
it; `next_queries` keeps depth one call away.

**Slice 3 sequencing.** It depends on the spur-graph notebook-support plan
landing first. Slices 1–2 are independent of it and ship regardless.

**Batch/live index divergence.** Two execution modes of one extractor can
still drift if the daemon's snapshot differs from the committed file. Contained
by the shared implementation, per-cell content hashes, and the
batch-live-agreement test in the slice 3 plan.

## 15. Acceptance Criteria

- All context metadata reachable through `notebook_context_pack` →
  `notebook_catalog` / `notebook_lineage` (→ `notebook_symbol_*` in slice 3)
  using one ref convention, with `next_queries` in every response.
- Datasource tables navigable in 2/3 kind-dependent layers; `scope:"used"`
  links tables to the session's notebook at query time.
- App SKILL.md actually reaches the agent (via context pack); dead
  `AppScope.skill` removed.
- The only per-turn push is the lens preamble + orient hint (+ optional focused
  cell ref).
- No response ever contains credentials; every response is version-stamped.
- DAG lineage answers upstream/downstream from any ref with bounded depth and
  failed-node error excerpts.
- Notebook cell symbols are extracted by the spur-graph notebook extractor
  (one parser, batch + live modes); repo-resident notebook symbols appear in
  the analyst index and `code_*` tools, and the daemon's `notebook_symbol_*`
  tools serve loose notebooks from the live index.
- Cross-language cell edges resolve through ports/tables only.
