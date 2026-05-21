-- SPUR code-graph analyst — worked examples across all three tiers.
-- Safe to run against a persistent .duckdb file built by init.sql, but the
-- extension state is per-connection so we LOAD here too.

LOAD duckpgq;
LOAD onager;

--------------------------------------------------------------------------
-- Tier 1 — Plain SQL: top 20 most-called functions (in-degree on Calls).
--------------------------------------------------------------------------
.print
.print ===  T1. Top 20 most-called functions (plain SQL, in-degree)  ===
SELECT n.qualified_name, COUNT(*) AS callers
FROM   edges e
JOIN   nodes n ON e.dst = n.id
WHERE  e.kind = 'calls'
GROUP BY n.qualified_name
ORDER BY callers DESC
LIMIT 20;

--------------------------------------------------------------------------
-- Tier 1 — Recursive CTE: 3-hop reverse reachability from a target.
--                          (i.e., "what depends on this transitively")
--------------------------------------------------------------------------
.print
.print ===  T2. 3-hop reverse reachability into 'handle_submit_plan' (recursive SQL)  ===
WITH RECURSIVE seed AS (
  SELECT id FROM nodes WHERE entity_name = 'handle_submit_plan'
),
reach AS (
  SELECT s.id AS frontier, 0 AS hops FROM seed s
  UNION
  SELECT e.src AS frontier, r.hops + 1 AS hops
  FROM   reach r
  JOIN   edges e ON e.dst = r.frontier AND e.kind = 'calls'
  WHERE  r.hops < 3
)
SELECT DISTINCT n.qualified_name, MIN(r.hops) AS min_hops
FROM   reach r
JOIN   nodes n ON n.id = r.frontier
WHERE  r.hops > 0
GROUP BY n.qualified_name
ORDER BY min_hops, n.qualified_name
LIMIT 25;

--------------------------------------------------------------------------
-- Tier 2 — DuckPGQ MATCH: variable-length path from main() to anything
--                          that touches 'WorkerRegistry'.
--------------------------------------------------------------------------
.print
.print ===  T3. DuckPGQ MATCH: callees of 'main' (1 hop)  ===
FROM GRAPH_TABLE (code
  MATCH (a:nodes)-[e:edges]->(b:nodes)
  WHERE a.entity_name = 'main' AND e.kind = 'calls'
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
JOIN   node_ids m ON m.node_id = p.node_id
JOIN   nodes    n ON n.id = m.id
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

--------------------------------------------------------------------------
-- Cross-domain sanity check: nodes-with-incoming-edges-but-not-outgoing
--                            i.e., "leaf consumers" in the call graph.
--------------------------------------------------------------------------
.print
.print ===  T6. Leaf consumers (called but never call others)  ===
SELECT n.qualified_name, COUNT(e.src) AS callers
FROM   nodes n
JOIN   edges e ON e.dst = n.id AND e.kind = 'calls'
WHERE  n.id NOT IN (SELECT DISTINCT src FROM edges WHERE kind = 'calls')
GROUP BY n.qualified_name
ORDER BY callers DESC
LIMIT 10;
