# SPUR code-graph analyst — DuckDB POC

Stands up a local **DuckDB** instance that reads the current spur-graph
Parquet artifact via `.spur/graph/CURRENT`, loads **DuckPGQ** (SQL/PGQ pattern
matching, SQL:2023) and **Onager** (graph
algorithms — PageRank, Louvain, components, centralities), and is ready to be
fronted by a DuckDB MCP server so the brain can issue arbitrary read-only
analytic queries against the code graph.

Architecturally this is the warm tier from `bd-1rqxk`:

```
spur-graph (petgraph, hot)  ◄── code_callers / code_callees / code_subgraph
       │
       └─ artifact Parquet ─►  this POC: DuckDB + DuckPGQ + Onager (warm)
                                       │
                                       └─ DuckDB MCP server (separate process)
                                                                 │
                                                                 └─ brain  (data-analyst profile)
```

No SPUR Rust crate links `libduckdb` in this POC. The brain talks to DuckDB
only via an MCP subprocess.

## Building the analyst DuckDB

The analyst DB is rebuilt automatically as a post-step of `spur-cli graph build`.
You should rarely need to invoke it manually.

### Canonical entry points

```bash
# Recommended: build the graph; the analyst DB is refreshed automatically.
spur-cli graph build --workspace

# Manual: refresh the analyst DB against the current graph.
spur-cli analyst build

# Legacy entry point — forwards to `spur-cli analyst build`.
./crates/spur-context/poc/duckdb-analyst/setup.sh
```

### Opting out

Pass `--no-analyst` to `spur-cli graph build`, or set
`SPUR_GRAPH_SKIP_ANALYST=1` in the environment. Useful in CI lanes that
build the graph but don't need the analyst surface.

### First-run cost

`init.sql` installs the `duckpgq` and `onager` DuckDB community extensions.
On a host with no cached community extensions, the first run downloads
both (5-30s, network-dependent). Subsequent runs are sub-second.

### Soft-fail behavior

If the `duckdb` CLI is not on PATH, `analyst build` prints a warning and
exits 0; the upstream `graph build` is unaffected. Install with
`brew install duckdb` (or your platform's equivalent) to enable analyst
refresh.

## What's in the DB

| Object              | Kind   | Notes                                                |
|---------------------|--------|------------------------------------------------------|
| `nodes`             | view   | symbols with stable IDs and dense `node_id` values   |
| `edges`             | view   | resolved edges with stable IDs and dense endpoints   |
| `edges_by_dst`      | view   | same resolved edges, sorted by `(dst_id, src_id)`    |
| `edges_unresolved`  | view   | unresolved labels (dynamic dispatch / macros)        |
| `files`             | view   | worktree files                                       |
| `file_manifests`    | view   | file content OIDs and per-file node IDs              |
| `tombstones`        | view   | deleted-file markers from incremental builds         |
| `onager_edges`      | view   | BIGINT (src, dst) over `edge_kind = 'calls'` only    |
| `_meta`             | table  | manifest metadata, graph_content_hash + counts       |
| `duckpgq_nodes`     | table  | DuckPGQ compatibility table sourced from `nodes`     |
| `duckpgq_edges`     | table  | DuckPGQ compatibility table sourced from `edges`     |
| `code`              | PG     | DuckPGQ property graph over nodes+edges              |

The primary analytical substrate is the Parquet-backed view set. The DuckPGQ
tables are compatibility copies because the current DuckPGQ extension rejects
property graphs over DuckDB views.

### One-stop analyst surface (`init_algorithms.sql` + `init_views.sql`)

So an analyst can ask one named view per question — never hand-rolling an Onager
call or a four-table join — the build layers two extra tiers on top of the base
views. `SELECT * FROM v_catalog` lists the whole surface.

**Tier A — graph algorithms, materialized once per build (`init_algorithms.sql`).**
Onager table functions need `LOAD onager` and are too costly to recompute per
query, so they are baked into TABLEs in the same build session (where init.sql
already loaded onager), then exposed as symbol-keyed views:

| View | Source | Notes |
|------|--------|-------|
| `v_symbol_centrality` | `onager_par_pagerank` + SQL degree | PageRank, in/out degree per symbol |
| `v_symbol_component`  | `onager_par_components` | weakly-connected component id + size |
| `v_symbol_community`  | `onager_cmm_louvain` | de-facto module id (label not stable across builds) |
| `v_graph_metrics`     | `onager_mtr_density` + rollups | one-row whole-graph metrics |

**Tier B — analytical views (`init_views.sql`).** Lazy compositions of the static
graph, the temporal layer, and the Tier-A tables:

| View | Answers |
|------|---------|
| `v_symbol_scorecard` | master per-symbol row (centrality+churn+age+inbound+component+posture) |
| `v_symbol_risk` | centrality × churn posture (`leaf` / `load-bearing wall` / `hot-central`) |
| `v_symbol_age` | born/last_seen/lifespan, keyed on **structural** identity (not raw stable_symbol_id) |
| `v_symbol_genealogy` | rename trails |
| `v_hidden_coupling` | co-change pairs with NO static edge (logical deps) |
| `v_fix_hotspots` | per-file fix-commit count + fix-rate |
| `v_commit_classified` | conventional-commit type per commit |
| `v_velocity` | per-month touches/commits/added/deleted |
| `v_unresolved_hotspots` | unresolved call labels by site count |
| `v_catalog` | self-describing list of the surface |

Build order is enforced in `analyst.rs`: `init.sql` → temporal → diagnostics →
**algorithms** → views (Tier-B views join the Tier-A surfaces). The `v_symbol_age`
view keys on `(file, entity, kind, scope)` because `stable_symbol_id` embeds a
byte offset — raw-id keying mints a fresh id whenever a symbol moves, producing
hundreds of thousands of phantom symbols with bogus sub-day lifespans.

### Search appliance (`init_search.sql`)

So an agent answers a question with **one query** — no per-session index build —
the build also materializes a full-text search surface and exposes `search()`
macros. Runs last (needs `v_symbol_scorecard` + the attached Lance section
store); `analyst.rs` gates it on `temporal && lance_present`. ~12s of the build.

| Object | Kind | Purpose |
|--------|------|---------|
| `sections` | TABLE | full section forest (every copy) — backs `v_doc_tree` navigation |
| `sections_search` | TABLE + FTS | **deduped** prose corpus (one row per distinct body) — backs `search_docs`/`search` |
| `symbol_text` | TABLE + FTS | per-symbol identifier text (name + aggregated tokens), **deduped** by content + BM25 index (code corpus) |
| `v_doc_tree` | VIEW | section heading tree with depth (PageIndex navigation substrate) |
| `search_docs(q)` | MACRO | BM25 over section bodies |
| `search_code(q)` | MACRO | BM25 over symbol tokens, **fused with `v_symbol_scorecard`** (pagerank/churn/posture/component) |
| `search(q)` | MACRO | unified doc+code ranked result with high-value signal inline |

```sql
LOAD fts;                                   -- mcp-init.sql does this per connection
SELECT * FROM search('oauth token refresh resolve auth');
SELECT * FROM search_code('review gate approve reject completed task');
```

**Dedup:** SPUR installs skills into ~6 agent dirs (`.claude`/`.codex`/`.kiro`/…),
so the raw section corpus is ~95% duplicate by body (19.7k rows → 989 distinct).
`sections_search` and `symbol_text` are deduped by content (preferring the
canonical, non-dot-dir path) before indexing, so a query returns each skill
section/symbol **once**, not once per agent dir. `sections` stays full so
`v_doc_tree` keeps every copy's heading hierarchy.

Each `search_code` hit carries its centrality/churn/posture, so a free-text query
returns *high-value* results — "the relevant symbol **and** how central/risky it
is" — in a single round trip. FTS indexes (`fts_main_sections`,
`fts_main_symbol_text`) are persisted in the `.duckdb`; only `LOAD fts` is needed
per connection (no rebuild).

## The three query tiers (all in `examples.sql`)

| Tier | Tool         | Example query                                           |
|------|--------------|---------------------------------------------------------|
| 1    | Plain SQL    | T1 — top callees by in-degree                          |
| 1    | Plain SQL    | T2 — reverse callers via `edges_by_dst`                |
| 2    | DuckPGQ MATCH| T3 — `GRAPH_TABLE(code MATCH (a)-[e]->(b) …)`           |
| 3    | Onager       | T4 — `onager_par_pagerank((SELECT src,dst FROM …))`     |
| 3    | Onager       | T5 — `onager_par_components(…)`                         |

All five are runnable against the current Parquet substrate.

## Wiring a DuckDB MCP server

The brain shouldn't link DuckDB. Run an MCP server in a subprocess that
attaches to `.spur/analyst.duckdb`. Two options:

### Option A — official MotherDuck server (Python via `uvx`)

Add to Claude Code MCP config (`~/.claude/mcp.json` or repo `.mcp.json`):

```json
{
  "mcpServers": {
    "spur-analyst": {
      "command": "uvx",
      "args": [
        "mcp-server-motherduck",
        "--db-path", "/Volumes/Projects/spur/.spur/analyst.duckdb",
        "--read-only"
      ]
    }
  }
}
```

Pros: maintained, ships `query`, `list_tables`, `describe`. Cons: extensions
(`duckpgq`, `onager`) are not auto-loaded on connect — see "Per-connection
extension state" below.

### Option B — write a thin custom MCP server

~150 lines of Python (`mcp` SDK + `duckdb-rs` or the Python binding) that:
1. Opens the persistent DB read-only.
2. Runs `LOAD duckpgq; LOAD onager;` plus any startup SQL on every connection.
3. Exposes `query(sql, limit=1000)`, `describe(table)`, `list_tables()`,
   `explain(sql)`.
4. Enforces statement timeout, row cap, rejects DDL/DML.

Recommended once the POC turns into a real capability — gives exact control
over safety rails. For the POC, use Option A and accept the rough edges.

## Per-connection extension state — the one gotcha

DuckDB extensions are **installed** persistently (the `INSTALL …` survives in
the DB file) but **must be `LOAD`-ed every connection**. The DuckPGQ property
graph `code` is created in `init.sql` and stored in the DB; the underlying
views and DuckPGQ compatibility tables persist too. But:

- DuckPGQ `MATCH` queries fail in a fresh connection until `LOAD duckpgq;`
  runs.
- Onager table functions are missing until `LOAD onager;` runs.

Workarounds:
1. `examples.sql` itself runs the `LOAD`s — drop-in safe.
2. If using Option A above, attach a startup SQL via MotherDuck-server flag
   (check current docs) or wrap it with a shell that pipes a startup script.
3. Custom server (Option B) bakes the LOADs into the connect path — cleanest.

## Observations on the SPUR graph

A few things worth knowing before reading PageRank output:

- **Section nodes outrank functions in T4.** The graph indexes markdown
  sections (`SKILL.md`, RCA docs, plans, growth-loop dailies) and they receive
  a lot of incoming `calls` references when code samples or symbols are quoted
  inline. To get a code-only PageRank, filter the `onager_edges` view to
  `kind = 'calls'` **and** join `nodes n ON n.id = e.src AND n.kind IN
  ('function','method')` upstream of the algorithm.
- **`submit_plan` exists only as docs Sections.** The runtime entry point is
  `impl McpCallbackServer::handle_submit_plan`. The `code_resolve` MCP tool
  would have caught this — useful selector hygiene reminder.
- **~59% of edges are unresolved** (68k of 115k). Dynamic dispatch, macro
  bodies, HOF arguments. They sit in `edges_unresolved` and can be inspected
  separately; they should not be included in algorithms that need a clean
  reachability graph.

## Files

| File          | Purpose                                                |
|---------------|--------------------------------------------------------|
| `init.sql`    | extensions, Parquet views, property graph, Onager view |
| `examples.sql`| 5 worked queries across all three tiers                |
| `setup.sh`    | idempotent bootstrap                                   |
| `README.md`   | this file                                              |

## Completed (v1.1.17)

The JSON ingestion swap from the old promotion path has shipped under
`bd-1wsxo`: `setup.sh` resolves `.spur/graph/CURRENT`, `init.sql` exposes the
Parquet artifact through `read_parquet(...)` views, `_artifact`/UNNEST and
`node_ids` are gone, and `edges_by_dst` is available for reverse-edge lookups.
