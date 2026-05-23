-- SPUR code-graph analyst — worked examples across all three tiers.
-- Safe to run against a persistent .duckdb file built by init.sql, but the
-- extension state is per-connection so we LOAD here too.

.timer on

LOAD duckpgq;
LOAD onager;

--------------------------------------------------------------------------
-- Tier 1 — Plain SQL: top 20 most-called functions (in-degree on Calls).
--------------------------------------------------------------------------
.print
.print ===  T1. Top 20 most-called functions (plain SQL, in-degree)  ===
SELECT n.qualified_name, COUNT(*) AS callers
FROM   edges e
JOIN   nodes n ON e.target_stable_id = n.stable_symbol_id
WHERE  e.edge_kind = 'calls'
GROUP BY n.qualified_name
ORDER BY callers DESC
LIMIT 20;

--------------------------------------------------------------------------
-- Tier 1 — Plain SQL: direct reverse callers through edges_by_dst.
--------------------------------------------------------------------------
.print
.print ===  T2. Reverse callers of 'impl JsonRpcResponse::success' via edges_by_dst  ===
SET variable reverse_target_dst_id = (
  SELECT node_id FROM nodes WHERE qualified_name = 'impl JsonRpcResponse::success'
);

SELECT target.qualified_name AS callee,
       caller.qualified_name AS caller,
       caller.file_path
FROM   edges_by_dst e
JOIN   nodes target ON target.node_id = getvariable('reverse_target_dst_id')
JOIN   nodes caller ON caller.node_id = e.src_id
-- edges_by_dst is sorted by (dst_id, src_id), so this WHERE dst_id = ? shape
-- lets DuckDB prune row groups for reverse-edge lookups instead of scanning edges.
WHERE  e.dst_id = getvariable('reverse_target_dst_id')
  AND  e.edge_kind = 'calls'
ORDER BY caller.qualified_name
LIMIT 20;

--------------------------------------------------------------------------
-- Tier 2 — DuckPGQ MATCH: direct callees from main().
--------------------------------------------------------------------------
.print
.print ===  T3. DuckPGQ MATCH: callees of 'main' (1 hop)  ===
FROM GRAPH_TABLE (code
  MATCH (a:duckpgq_nodes)-[e:duckpgq_edges]->(b:duckpgq_nodes)
  WHERE a.entity_name = 'main' AND e.edge_kind = 'calls'
  COLUMNS (a.qualified_name AS caller, b.qualified_name AS callee)
) LIMIT 20;

--------------------------------------------------------------------------
-- Tier 3 — Onager PageRank: rank all functions by inbound importance.
--                            Joined back to qualified_name for readability.
--------------------------------------------------------------------------
.print
.print ===  T4. Onager PageRank: top 20 most-important symbols (call graph)  ===
SELECT n.qualified_name, n.file_path, round(p.rank, 6) AS pagerank
FROM   onager_par_pagerank(
         (SELECT src, dst FROM onager_edges),
         damping := 0.85, iterations := 50, directed := true
       ) p
JOIN   nodes n ON n.node_id = p.node_id
ORDER BY p.rank DESC
LIMIT 20;

--------------------------------------------------------------------------
-- Tier 3 — Onager Weakly-Connected Components: subsystem discovery.
--------------------------------------------------------------------------
.print
.print ===  T5. Onager components: largest connected subsystems  ===
WITH components AS (
  SELECT * FROM onager_par_components(
                   (SELECT src, dst FROM onager_edges)
                 )
)
SELECT c.component AS component_id, COUNT(*) AS size
FROM   components c
GROUP BY c.component
ORDER BY size DESC
LIMIT 10;
