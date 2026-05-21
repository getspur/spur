# SPUR code-graph analyst — DuckDB POC

Stands up a local **DuckDB** instance that ingests `.spur/graph-index.json`,
loads **DuckPGQ** (SQL/PGQ pattern matching, SQL:2023) and **Onager** (graph
algorithms — PageRank, Louvain, components, centralities), and is ready to be
fronted by a DuckDB MCP server so the brain can issue arbitrary read-only
analytic queries against the code graph.

Architecturally this is the warm tier from `bd-1rqxk`:

```
spur-graph (petgraph, hot)  ◄── code_callers / code_callees / code_subgraph
       │
       └─ artifact JSON  ─►  this POC: DuckDB + DuckPGQ + Onager (warm)
                                       │
                                       └─ DuckDB MCP server (separate process)
                                                                 │
                                                                 └─ brain  (data-analyst profile)
```

No SPUR Rust crate links `libduckdb` in this POC. The brain talks to DuckDB
only via an MCP subprocess.

## Quick start

```sh
brew install duckdb           # one time
./crates/spur-context/poc/duckdb-analyst/setup.sh
duckdb .spur/analyst.duckdb < crates/spur-context/poc/duckdb-analyst/examples.sql
```

`setup.sh` is idempotent: drops `.spur/analyst.duckdb` and rebuilds. Override
defaults with env vars: `SPUR_GRAPH_ARTIFACT`, `SPUR_ANALYST_DB`.

## What's in the DB

| Object              | Kind   | Notes                                                |
|---------------------|--------|------------------------------------------------------|
| `nodes`             | table  | 27.5k symbols (id, qualified_name, kind, file_path…) |
| `node_ids`          | table  | dense BIGINT mapping for Onager                      |
| `edges`             | table  | 47k resolved edges (src, dst, kind, confidence…)     |
| `edges_unresolved`  | table  | 68k unresolved labels (dynamic dispatch / macros)    |
| `files`             | table  | 1.5k worktree files                                  |
| `onager_edges`      | view   | BIGINT (src, dst) over `kind = 'calls'` only         |
| `_meta`             | table  | graph_content_hash + counts                          |
| `code`              | PG     | DuckPGQ property graph over nodes+edges              |

Numbers above are for the SPUR worktree at content hash
`751d5367987e71e5f029aa757218e13bef23f18eab9d1b522545efff357013ab`.

## The three query tiers (all in `examples.sql`)

| Tier | Tool         | Example query                                           |
|------|--------------|---------------------------------------------------------|
| 1    | Plain SQL    | T1 — top callees by in-degree                          |
| 1    | Recursive CTE| T2 — 3-hop reverse reachability                        |
| 2    | DuckPGQ MATCH| T3 — `GRAPH_TABLE(code MATCH (a)-[e]->(b) …)`           |
| 3    | Onager       | T4 — `onager_par_pagerank((SELECT src,dst FROM …))`     |
| 3    | Onager       | T5 — `onager_par_components(…)`                         |

All five pass against the SPUR graph as of the hash above.

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
tables persist too. But:

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
| `init.sql`    | extensions, tables, BIGINT mapping, property graph     |
| `examples.sql`| 6 worked queries across all three tiers                |
| `setup.sh`    | idempotent bootstrap                                   |
| `README.md`   | this file                                              |

## Promotion path to production

When this POC graduates per `bd-1rqxk`:

1. **Replace JSON ingestion with Parquet.** `spur-graph` writes
   `nodes.parquet` + `edges.parquet` keyed on `graph_content_hash`. Init script
   becomes `CREATE VIEW … AS SELECT * FROM read_parquet(…)`. No `_artifact`
   staging view; no UNNEST. Faster, lower memory, no 42 MB JSON read.
2. **Move SQL into `crates/spur-context/src/sql/`** as `schema_code_graph.sql`
   alongside the existing `schema.sql` for agent events. One source of SQL
   truth.
3. **Cross-domain joins land here.** Once `all_events` (agent telemetry),
   `blame`, `beads_issues` views coexist in the same DB, the analyst can ask
   questions no single-substrate tool can answer.
4. **Custom MCP server** (Option B) with proper safety rails replaces Option A.
