-- SPUR code-graph analyst — graph-algorithm materialization (Tier A).
--
-- Onager graph algorithms are table functions that (a) require `LOAD onager`
-- and (b) are too costly to recompute on every analytical query. This script
-- runs ONCE per analyst build — in the same duckdb session as init.sql, where
-- onager is already loaded — and bakes the algorithm outputs into TABLEs the
-- read-only MCP can SELECT without ever touching Onager.
--
-- Ordering contract:
--   * runs AFTER  init.sql            (needs `onager_edges`, `nodes`)
--   * runs BEFORE init_views.sql      (v_symbol_risk / v_symbol_scorecard join
--                                       the v_symbol_centrality/_component/_community views)
--
-- `onager_edges` is the resolved calls subgraph (edge_kind='calls'), dense-keyed
-- by node_id. All _algo_* tables are therefore keyed by the SAME dense node_id
-- the `nodes` view exposes, so the public v_symbol_* views join cleanly.
--
-- Note: Louvain community ids are not stable across rebuilds (the algorithm is
-- randomized); only the grouping is meaningful, not the integer label.

-- PageRank centrality over the calls graph.
CREATE OR REPLACE TABLE _algo_pagerank AS
SELECT node_id, rank AS pagerank
FROM onager_par_pagerank((SELECT src, dst FROM onager_edges));

-- Weakly-connected components (connectivity islands / dead clusters).
CREATE OR REPLACE TABLE _algo_component AS
SELECT node_id, component AS component_id
FROM onager_par_components((SELECT src, dst FROM onager_edges));

CREATE OR REPLACE TABLE _algo_component_size AS
SELECT component_id, count(*) AS component_size
FROM _algo_component
GROUP BY component_id;

-- Louvain communities (de-facto modules; cross-crate communities = leaky abstraction).
CREATE OR REPLACE TABLE _algo_community AS
SELECT node_id, community AS community_id
FROM onager_cmm_louvain((SELECT src, dst FROM onager_edges));

-- In/out degree over the calls graph (plain SQL — no Onager needed).
CREATE OR REPLACE TABLE _algo_degree AS
WITH outd AS (SELECT src AS node_id, count(*) AS out_degree FROM onager_edges GROUP BY src),
     ind  AS (SELECT dst AS node_id, count(*) AS in_degree  FROM onager_edges GROUP BY dst)
SELECT COALESCE(o.node_id, i.node_id) AS node_id,
       COALESCE(i.in_degree, 0)  AS in_degree,
       COALESCE(o.out_degree, 0) AS out_degree
FROM outd o
FULL OUTER JOIN ind i ON i.node_id = o.node_id;

-- ── Public, symbol-keyed surfaces (no LOAD onager required downstream) ──────

CREATE OR REPLACE VIEW v_symbol_centrality AS
SELECT n.stable_symbol_id,
       n.node_id,
       COALESCE(pr.pagerank, 0.0) AS pagerank,
       COALESCE(d.in_degree, 0)   AS in_degree,
       COALESCE(d.out_degree, 0)  AS out_degree
FROM nodes n
LEFT JOIN _algo_pagerank pr USING (node_id)
LEFT JOIN _algo_degree   d  USING (node_id);

CREATE OR REPLACE VIEW v_symbol_component AS
SELECT n.stable_symbol_id,
       n.node_id,
       c.component_id,
       cs.component_size
FROM nodes n
JOIN _algo_component c USING (node_id)
LEFT JOIN _algo_component_size cs USING (component_id);

CREATE OR REPLACE VIEW v_symbol_community AS
SELECT n.stable_symbol_id,
       n.node_id,
       cm.community_id
FROM nodes n
JOIN _algo_community cm USING (node_id);

-- One-row whole-graph metrics — trend these across rebuilds.
CREATE OR REPLACE TABLE v_graph_metrics AS
SELECT
  (SELECT count(*)                       FROM onager_edges)        AS calls_edges,
  (SELECT count(DISTINCT node_id)        FROM _algo_component)     AS connected_nodes,
  (SELECT count(DISTINCT component_id)   FROM _algo_component)     AS components,
  (SELECT max(component_size)            FROM _algo_component_size) AS largest_component,
  (SELECT count(DISTINCT community_id)   FROM _algo_community)     AS communities,
  (SELECT density FROM onager_mtr_density((SELECT src, dst FROM onager_edges))) AS density;
