---
name: spur-analyst
description: "Use this when a code question needs aggregation, time-series, multi-table JOIN, ranking, or property-graph path traversal — anything beyond what the per-symbol `code_*` MCP tools can answer in one call. Establishes the spur-analyst DuckDB graph index as the SQL substrate for hotspot detection, churn-weighted impact analysis, co-change rings, reachability paths, and graph algorithms via DuckPGQ + Onager extensions."
role: both
---

# Spur Analyst — Graph Database Analysis

The spur-analyst MCP server exposes the same graph artifact the `code_*` tools query, but as a full DuckDB instance with **DuckPGQ** (property-graph MATCH) and **Onager** (graph algorithms) extensions. Use it when one symbol's call list is not enough — when you need ranking, history, JOINs, paths, or algorithms.

<HARD-GATE>
Before writing any SQL against this database, run a schema discovery query first (`information_schema.columns` filtered by `table_name`). The schema uses non-obvious column names (`content_oid` not `blob_oid`, `function_oid` not `oid`, `events` not `event_count`), and DuckDB's binder error on a wrong name will list candidate columns — that is your fastest path to the right query.
</HARD-GATE>

## When to use spur-analyst vs `code_*` MCP tools

`code_*` is a per-symbol tool — one selector, one budget-limited response, fast. `spur-analyst` is SQL — joins, aggregations, time-series, paths, algorithms.

| Question shape | Right tool |
|---|---|
| "What does this symbol do / who calls it?" | `code_*` (code-explore skill) |
| "What's at this file path?" | `code_search file=...` |
| "Top 20 risk hotspots in spur-mcp" | spur-analyst: `v_blast_radius` JOIN nodes |
| "Files that co-change with mod.rs in the last 90d" | spur-analyst: `v_file_cochange` |
| "Who has touched this symbol in the last month?" | spur-analyst: `temporal_edges` + `commits` |
| "Did this symbol move between files? Renames?" | spur-analyst: `symbol_snapshots` / `temporal_edges` change_kind = renamed_from_* |
| "Shortest call path from A to B" | spur-analyst + DuckPGQ MATCH or Onager BFS |
| "Strongly connected components of the calls graph" | spur-analyst + Onager |
| "Reverse-import chain that brings in zeromq" | spur-analyst: recursive CTE on `edges WHERE relation='imports'` |

If the question is "show me one symbol's bytes" — use `code_read_symbol`. If it's "show me 20 symbols ranked by something" — use spur-analyst.

This is **layer 3 of the retrieval stack** (see the code-explore skill): `knowledge_context_pack` orients, `code_*` works one symbol at a time, spur-analyst answers set-shaped questions. Hand-chaining many `code_callers`/`code_callees` calls to build a ranking, closure, or path is the signal to switch to SQL here.

## Database inventory

The artifact lives at `.spur/analyst.duckdb`. Current scale (refreshed every graph rebuild):
- ~31k symbol nodes, ~53k resolved edges, ~81k unresolved-by-name edges
- ~2k files, ~2.5k commits, ~355k symbol_snapshots, ~370k temporal edges
- 3 tombstones (deleted-file records)

Extensions installed (verify with `SELECT extension_name FROM duckdb_extensions() WHERE installed`):
- **duckpgq** — property-graph MATCH syntax (path patterns, SHORTEST)
- **onager** — graph algorithms (`onager_edges` view is the dense-id call subgraph)
- **lance**, **spatial**, **ducklake**, **json**, **parquet**, **httpfs**, **sqlite_scanner**, **excel**

### Table layout

**Base tables** (DuckPGQ/Onager substrate — usually queried via views):
- `duckpgq_nodes` — `(stable_symbol_id, node_id BIGINT, qualified_name, entity_name, symbol_kind, file_path)`. The `node_id` is dense BIGINT for joins with Onager.
- `duckpgq_edges` — `(source_stable_id, target_stable_id, edge_kind, relation, confidence)`. Resolved edges only.
- `node_dense_id_map` — translation between stable_symbol_id and dense node_id.
- `_meta` — one-row table with artifact metadata (`graph_content_hash`, `indexed_head_oid` equivalent, `node_count`, `resolved_edge_count`, `manifest_version`, …). **Always sanity-check this first.**

**Symbol/edge views:**
- `nodes` — `(stable_symbol_id, node_id, file_path, byte_range_start/end, line_start/end, entity_name, qualified_name, symbol_kind, anchor_hash, enclosing_scope)`. The canonical symbol table.
- `edges` — `(source_stable_id, target_stable_id, src_id, dst_id, target_label, relation, confidence, confidence_score, edge_kind, bind_method)`. Resolved direct edges. `relation ∈ {contains, calls, imports, references, links}`. `edge_kind ∈ {calls, calls_dyn, references_hof, references_other}`.
- `edges_by_dst` — same rows, indexed (clustered) on `dst_id` for caller queries.
- `edges_unresolved` — text-label edges with no resolved target (e.g. macro-bodied calls). Same shape minus the resolved columns.
- `files` — `(stable_file_id, node_id, file_path)`. Per-file root node.
- `file_manifests` — `(stable_file_id, path, content_oid, node_ids[])`. `node_ids` lists every symbol declared in the file.
- `tombstones` — `(path, stable_file_id)`. Deleted files (kept for symbol-history continuity).

**Temporal views:**
- `commits` — `(sha, parents VARCHAR[], author_time BIGINT, summary, author_ts TIMESTAMP WITH TIME ZONE)`.
- `symbol_snapshots` — per-(symbol, commit) row with line range, anchor_hash, and tokens. Powers blame-like queries.
- `temporal_edges` — `(source_kind, source_stable_symbol_id, source_commit, target_kind, target_stable_symbol_id, target_commit, relation='touches', parent, change_kind, rename_prev_*)`. The "commit touched symbol" feed. `change_kind ∈ {added, modified, deleted, renamed_from_symbol, renamed_from_file}`.

**Aggregate / ranking views (the most useful for analysis):**
- `v_blast_radius` — `(stable_symbol_id, entity_name, symbol_kind, file_path, caller_count, hot_caller_count, caller_churn_90d, self_churn_90d, self_last_touched, blast_radius_score DOUBLE)`. *Ranking signal for refactor risk.*
- `v_symbol_inbound` — `(stable_symbol_id, callers, importers, containers, inbound_total)`. Quick "who depends on this" without full edge enumeration.
- `v_symbol_churn_90d` — `(stable_symbol_id, events, commits, added, modified, deleted, renamed, last_touched)`. 90-day per-symbol activity.
- `v_symbol_file` — `(stable_symbol_id, commit_sha, file_path)`. Symbol↔file mapping per commit (handles renames).
- `v_commit_files` — `(commit_sha, file_path)`. Flat denormalization of files touched per commit.
- `v_file_cochange` — `(file_a, file_b, cochange_count, has_static_edge)`. 90-day co-change pairs with a flag indicating whether they're connected in the static graph too.
- `v_file_static_edges` — `(file_a, file_b)`. File-level static edges (collapsed from symbol-level).
- `onager_edges` — `(src BIGINT, dst BIGINT)`. Dense-id call subgraph; the input format Onager algorithms expect.
- `diagnostics` — `(message VARCHAR)`. Build warnings from the last graph rebuild.

## Discovery first (avoid binder-error round trips)

DuckDB's binder error is your friend — it lists candidate column names. Use that to converge quickly. Standard discovery batch for an unfamiliar view:

```sql
-- Confirm artifact metadata + freshness:
SELECT graph_content_hash, manifest_version, node_count, resolved_edge_count,
       file_count, commit_count, complete
FROM _meta;

-- Get columns for the table you intend to query:
SELECT table_name, column_name, data_type
FROM information_schema.columns
WHERE table_schema = 'main' AND table_name = 'v_blast_radius'
ORDER BY ordinal_position;
```

If you get `Binder Error: Referenced column "X" not found`, read the `Candidate bindings:` line — that names the actual column. Don't guess twice.

## Query templates by intent

### Symbol-level

```sql
-- Top N risk hotspots in a crate, ranked by blast radius:
SELECT entity_name, file_path, caller_count, self_churn_90d, blast_radius_score
FROM v_blast_radius
WHERE file_path LIKE 'crates/spur-mcp/%'
ORDER BY blast_radius_score DESC NULLS LAST
LIMIT 20;

-- Functions changed in the last 30 days, ordered by churn:
SELECT n.qualified_name, n.file_path, c.events, c.modified, c.last_touched
FROM v_symbol_churn_90d c
JOIN nodes n USING (stable_symbol_id)
WHERE c.last_touched >= now() - INTERVAL 30 DAY
  AND n.symbol_kind = 'function'
ORDER BY c.events DESC
LIMIT 25;
```

### Edge-level (call graph)

```sql
-- All resolved callers of a function (faster than code_callers when you need
-- to JOIN with churn or kind filters):
SELECT src.qualified_name AS caller, src.file_path
FROM edges e
JOIN nodes src ON src.stable_symbol_id = e.source_stable_id
JOIN nodes dst ON dst.stable_symbol_id = e.target_stable_id
WHERE dst.qualified_name = 'impl NotebookDaemonControl::reopen'
  AND e.relation = 'calls';

-- Popular sinks (functions called by >30 sites), file-by-file:
SELECT dst.file_path, dst.qualified_name, count(*) AS callers
FROM edges e
JOIN nodes dst ON dst.stable_symbol_id = e.target_stable_id
WHERE e.relation = 'calls'
GROUP BY dst.file_path, dst.qualified_name
HAVING count(*) > 30
ORDER BY callers DESC
LIMIT 20;

-- Unresolved call labels (suggests macro bodies / dynamic dispatch):
SELECT target_label, count(*) AS sites
FROM edges_unresolved
WHERE edge_kind = 'calls'
GROUP BY target_label
ORDER BY sites DESC
LIMIT 20;
```

### File-level

```sql
-- Co-change ring for a file (90-day window):
SELECT file_b, cochange_count, has_static_edge
FROM v_file_cochange
WHERE file_a = 'crates/spur-notebook/src/mcp/mod.rs'
   OR file_b = 'crates/spur-notebook/src/mcp/mod.rs'
ORDER BY cochange_count DESC
LIMIT 12;

-- Files that co-change a lot but have NO static edge — suspect implicit
-- coupling (shared schema, generated code, doc-stamp pairs):
SELECT file_a, file_b, cochange_count
FROM v_file_cochange
WHERE cochange_count >= 5 AND NOT has_static_edge
ORDER BY cochange_count DESC
LIMIT 20;
```

### Temporal / blame

```sql
-- Recent commits touching a specific symbol:
SELECT c.sha, c.author_ts, c.summary, t.change_kind
FROM temporal_edges t
JOIN commits c ON c.sha = t.source_commit
WHERE t.target_stable_symbol_id = '<stable_symbol_id from code_search>'
  AND t.relation = 'touches'
ORDER BY c.author_ts DESC
LIMIT 20;

-- Rename trail for a symbol — follow renamed_from_symbol/file change_kinds:
SELECT source_commit, change_kind,
       rename_prev_stable_symbol_id, target_stable_symbol_id
FROM temporal_edges
WHERE target_stable_symbol_id = '<sid>'
  AND change_kind LIKE 'renamed_from_%'
ORDER BY source_commit;

-- Symbol-as-it-was at a specific commit (line range, byte range, tokens):
SELECT entity_name, line_range_start, line_range_end, anchor_hash, tokens
FROM symbol_snapshots
WHERE stable_symbol_id = '<sid>' AND commit_sha = '<sha>';
```

### Hotspot / risk (combining views)

```sql
-- Refactor heat map: high blast radius + high recent churn:
SELECT b.entity_name, b.file_path, b.caller_count, b.self_churn_90d,
       b.blast_radius_score
FROM v_blast_radius b
WHERE b.self_churn_90d > 0 AND b.caller_count > 5
ORDER BY (b.blast_radius_score * log(1 + b.self_churn_90d)) DESC
LIMIT 25;

-- "Quiet but central" — high blast radius, zero recent churn (these are
-- the load-bearing walls; touch with extra care):
SELECT entity_name, file_path, caller_count, blast_radius_score
FROM v_blast_radius
WHERE caller_count >= 10 AND self_churn_90d = 0
ORDER BY blast_radius_score DESC
LIMIT 20;
```

### Reachability — recursive CTE (works without DuckPGQ)

```sql
-- All symbols reachable from a seed within N hops via calls only:
WITH RECURSIVE walk(sid, depth) AS (
  SELECT '<seed sid>' AS sid, 0
  UNION
  SELECT e.target_stable_id, w.depth + 1
  FROM walk w
  JOIN edges e ON e.source_stable_id = w.sid
  WHERE e.relation = 'calls' AND w.depth < 3
)
SELECT DISTINCT n.qualified_name, n.file_path, w.depth
FROM walk w
JOIN nodes n ON n.stable_symbol_id = w.sid
ORDER BY w.depth, n.qualified_name;
```

### DuckPGQ — property-graph MATCH

DuckPGQ adds SQL/PGQ `MATCH` syntax for path patterns. The `duckpgq_nodes` and `duckpgq_edges` tables are pre-populated and ready to register as a property graph.

```sql
-- One-time per session: create the property graph (run once, then MATCH freely):
-CREATE PROPERTY GRAPH spur_code
  VERTEX TABLES (duckpgq_nodes KEY (node_id))
  EDGE TABLES (
    duckpgq_edges
      SOURCE KEY (source_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
      DESTINATION KEY (target_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
      LABEL edge
  );

-- Shortest call path between two qualified names:
-FROM GRAPH_TABLE (spur_code
  MATCH p = ANY SHORTEST (src:duckpgq_nodes) -[:edge]->{1,6} (dst:duckpgq_nodes)
  WHERE src.qualified_name = 'foo'
    AND dst.qualified_name = 'bar'
  COLUMNS (src.qualified_name AS source, dst.qualified_name AS target,
           path_length(p) AS hops)
) t;
```

If `CREATE PROPERTY GRAPH` errors, the syntax may differ slightly across DuckPGQ releases — check `SELECT * FROM duckdb_extensions() WHERE extension_name = 'duckpgq'` for the installed version and look up the matching docs. Fallback: use the recursive CTE pattern above; it costs more rows but is portable.

### Onager — graph algorithms

`onager_edges` is the dense-id call subgraph (`src BIGINT, dst BIGINT`). Onager exposes algorithms via table functions; call them with the edge view as input.

```sql
-- Probe what's exposed (function names depend on the Onager build):
SELECT function_name, function_type
FROM duckdb_functions()
WHERE function_name ILIKE 'onager%' OR function_name ILIKE 'graph_%';

-- Typical use (verify the function name in your version first):
-- PageRank-style centrality:
-- SELECT node_id, pagerank FROM onager_pagerank('onager_edges') ORDER BY pagerank DESC LIMIT 20;
-- Strongly-connected components:
-- SELECT node_id, component_id FROM onager_scc('onager_edges');
```

Join Onager output back to `nodes` via `node_id`:

```sql
WITH ranks AS (SELECT node_id, pagerank FROM onager_pagerank('onager_edges'))
SELECT n.qualified_name, n.file_path, r.pagerank
FROM ranks r JOIN nodes n USING (node_id)
ORDER BY r.pagerank DESC LIMIT 25;
```

If Onager's function names don't match in your build, the dense-id edge list (`SELECT src, dst FROM onager_edges`) is still useful as input to any other graph processing — export to networkx via parquet, or implement BFS/SCC with recursive CTE.

## Combining views — example: "refactor planning" report

```sql
-- For a candidate refactor target, show: caller count, recent churn,
-- co-change neighbors, and which callers themselves churn.
WITH target AS (
  SELECT stable_symbol_id FROM nodes
  WHERE qualified_name = '<target name>' LIMIT 1
),
caller_list AS (
  SELECT e.source_stable_id AS sid
  FROM edges e, target t
  WHERE e.target_stable_id = t.stable_symbol_id AND e.relation = 'calls'
)
SELECT
  (SELECT count(*) FROM caller_list) AS direct_callers,
  (SELECT sum(events) FROM v_symbol_churn_90d c
    JOIN caller_list cl ON cl.sid = c.stable_symbol_id) AS caller_churn_90d,
  (SELECT events FROM v_symbol_churn_90d c
    WHERE c.stable_symbol_id = (SELECT stable_symbol_id FROM target)) AS self_churn_90d;
```

## Anti-patterns

- **Querying without `_meta` sanity check** when the question is freshness-sensitive (post-merge, post-rebase). The hash should match the `code_*` response's `graph_content_hash`; if not, the artifact is mid-rebuild or two clients are reading different snapshots.
- **`SELECT * FROM edges` without a filter.** ~53k rows. Always join through `nodes` or `files` so the planner can prune.
- **Treating `target_label` as a resolved target.** `edges_unresolved` and `edges.target_label` are *text labels* the resolver couldn't match. Useful for "what got missed" diagnostics; do NOT use them as authoritative edges.
- **Using `v_file_cochange` as a current-state signal.** It's a 90-day rolling window. If the file was renamed or deleted recently, the historical rows still show. Cross-check with `tombstones` / `file_manifests` before reasoning about live structure.
- **Re-resolving by name across queries.** Once you have a `stable_symbol_id` from `code_*` or `nodes`, carry it as a parameter. Names are slower and can collide (e.g. `new` exists hundreds of times).
- **`SELECT count(*) FROM temporal_edges` as a complexity probe.** It's 370k rows by design. Use `_meta.temporal_edge_count` for the same answer in O(1).
- **DuckPGQ `MATCH` with no upper bound on hop count.** `-[:edge]->*` will enumerate all transitive paths — millions of rows on this graph. Always bound: `-[:edge]->{1,N}` or use `SHORTEST`.

## Cross-validation with `code_*` MCP tools

The same artifact backs both. Use this to sanity-check SQL results — disagreement is usually a JOIN bug or a `target_label`/`unresolved` filter slip, not a graph-data inconsistency.

- `code_callers(selector)` should equal `SELECT count(*) FROM edges WHERE target_stable_id = '<sid>' AND relation = 'calls'` (resolved-only) + the same against `edges_unresolved` for the unresolved tally.
- `code_search query=X` rows should be `SELECT * FROM nodes WHERE qualified_name = X OR entity_name = X`.
- `code_read_symbol(sid).line_range` should equal `(line_start, line_end)` in `nodes`.

If counts diverge: check (a) you used `edges` not `edges_by_dst` (same rows different index), (b) you filtered by both `relation` and `edge_kind` where appropriate, and (c) the artifact hash in `_meta` matches the `code_*` response.

## Key principles

- **Schema-first.** One `information_schema.columns` query per new table beats three binder-error retries. Read the candidate-bindings line on errors.
- **Aggregate views (`v_*`) before raw tables.** They're pre-aggregated for the questions you're most likely to ask; raw tables are an escape hatch when no view fits.
- **Join through `node_id` (BIGINT) when speed matters, `stable_symbol_id` (VARCHAR) when stability matters.** Node IDs are dense per-rebuild; stable IDs survive rebuilds.
- **Bound every traversal.** Recursive CTEs and DuckPGQ MATCHes default to "explore everything." Cap depth, cap row counts, cap variable-length edges.
- **One source of truth for symbol identity.** `stable_symbol_id` everywhere. Names collide; IDs don't.
- **Use the right history table.** `commits` for commit metadata. `temporal_edges` for "what touched what." `symbol_snapshots` for "what did the symbol look like at this commit." Don't reinvent the join.
- **Cross-check with `code_*`.** If a SQL count contradicts `code_callers`, the SQL is almost always wrong.

## TL;DR

```
0. SELECT * FROM _meta  → confirm freshness + scale
1. information_schema.columns  → confirm column names BEFORE writing the real query
2. Pick a view (v_blast_radius, v_file_cochange, v_symbol_churn_90d, ...) when one fits
3. Join through nodes/files using stable_symbol_id or node_id
4. Recursive CTE for reachability; DuckPGQ MATCH for path patterns; Onager for algorithms
5. Cross-validate the answer with one code_* call before reporting
```
