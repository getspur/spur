-- SPUR code-graph analyst — DuckDB init script.
--
-- Loads DuckPGQ (SQL/PGQ pattern matching) and Onager (graph algorithms),
-- materializes the spur-graph artifact into tables, registers a DuckPGQ
-- property graph, and builds a BIGINT-keyed edge view that Onager can chew on.
--
-- Required variable (set with `SET variable artifact_path = '...';` before .read):
--   artifact_path  — absolute path to .spur/graph-index.json
--
-- Produces:
--   nodes(id, node_id, qualified_name, entity_name, kind, file_path, line_start, line_end, enclosing_scope)
--   edges(src, dst, src_id, dst_id, target_label, kind, confidence, confidence_score)
--   edges_unresolved(src, target_label, kind, confidence, confidence_score)
--   files(file_id, file_path)
--   _meta(graph_content_hash, node_count, resolved_edge_count, unresolved_edge_count, file_count)
--   PROPERTY GRAPH code                 — DuckPGQ surface
--   onager_edges(src, dst) view         — Onager surface (BIGINT-keyed)

INSTALL duckpgq FROM community;
INSTALL onager  FROM community;
LOAD duckpgq;
LOAD onager;

SET preserve_insertion_order = false;
SET memory_limit = '6GB';
SET threads = 4;

-- View, not table: re-streams the JSON on each reference so DuckDB does not
-- have to materialize the 42 MB single-row staging set in memory at once.
CREATE OR REPLACE VIEW _artifact AS
SELECT *
FROM read_json(
        getvariable('artifact_path'),
        maximum_object_size = 200000000
     );

CREATE OR REPLACE TABLE files AS
SELECT f.stable_file_id AS file_id,
       f.file_path
FROM   _artifact, UNNEST(_artifact.files) AS t(f);

-- Build nodes WITHOUT the dense node_id first — the row_number() sort buffer
-- cannot stream a 27k-row × multi-KB-struct projection. Compute node_id in a
-- separate small mapping table below, where the sort key (id) is the only column.
CREATE OR REPLACE TABLE nodes AS
SELECT s.stable_symbol_id    AS id,
       s.qualified_name      AS qualified_name,
       s.entity_name         AS entity_name,
       s.symbol_kind         AS kind,
       s.file_path           AS file_path,
       s.line_range[1]       AS line_start,
       s.line_range[2]       AS line_end,
       s.enclosing_scope     AS enclosing_scope
FROM   _artifact, UNNEST(_artifact.symbols) AS t(s);

-- Dense BIGINT mapping for algorithm functions that require it (e.g. Onager).
CREATE OR REPLACE TABLE node_ids AS
SELECT id,
       row_number() OVER (ORDER BY id) - 1 AS node_id
FROM   nodes;

CREATE OR REPLACE TABLE edges AS
SELECT e.source_stable_symbol_id   AS src,
       e.target_stable_symbol_id   AS dst,
       e.target_label              AS target_label,
       e.relation                  AS kind,
       e.confidence                AS confidence,
       e.confidence_score          AS confidence_score
FROM   _artifact, UNNEST(_artifact.edges) AS t(e)
WHERE  e.target_stable_symbol_id IS NOT NULL;


CREATE OR REPLACE TABLE edges_unresolved AS
SELECT e.source_stable_symbol_id   AS src,
       e.target_label              AS target_label,
       e.relation                  AS kind,
       e.confidence                AS confidence,
       e.confidence_score          AS confidence_score
FROM   _artifact, UNNEST(_artifact.edges) AS t(e)
WHERE  e.target_stable_symbol_id IS NULL;

-- BIGINT-keyed view for Onager: every algorithm fn expects (src BIGINT, dst BIGINT).
-- Lazy JOIN against the small node_ids mapping resolves dense ids at query time.
CREATE OR REPLACE VIEW onager_edges AS
SELECT sn.node_id AS src, dn.node_id AS dst
FROM   edges e
JOIN   node_ids sn ON sn.id = e.src
JOIN   node_ids dn ON dn.id = e.dst
WHERE  e.kind = 'calls';

CREATE OR REPLACE TABLE _meta AS
SELECT (SELECT graph_content_hash FROM _artifact)     AS graph_content_hash,
       (SELECT COUNT(*) FROM nodes)                   AS node_count,
       (SELECT COUNT(*) FROM edges)                   AS resolved_edge_count,
       (SELECT COUNT(*) FROM edges_unresolved)        AS unresolved_edge_count,
       (SELECT COUNT(DISTINCT file_path) FROM files)  AS file_count;

-- Drop the streaming view; nodes/edges/_meta are now self-contained.
DROP VIEW _artifact;

CREATE OR REPLACE PROPERTY GRAPH code
  VERTEX TABLES (
    nodes PROPERTIES (id, qualified_name, entity_name, kind, file_path)
  )
  EDGE TABLES (
    edges SOURCE      KEY (src) REFERENCES nodes (id)
          DESTINATION KEY (dst) REFERENCES nodes (id)
          PROPERTIES  (kind, confidence)
  );
