-- SPUR code-graph analyst — DuckDB init script.
--
-- Loads DuckPGQ (SQL/PGQ pattern matching) and Onager (graph algorithms),
-- exposes the spur-graph Parquet artifact through views, registers a DuckPGQ
-- property graph, and builds a BIGINT-keyed edge view that Onager can chew on.
--
-- Template variable (substituted by setup.sh before execution):
--   __SPUR_GRAPH_ARTIFACT_DIR__  — absolute resolved Parquet artifact directory
--
-- Produces:
--   node_dense_id_map          — TABLE: (stable_symbol_id → dense_id) globally unique
--   nodes                      — view over nodes.parquet, with node_id REPLACED by dense_id
--   edges                      — view over edges.parquet, src_id/dst_id re-keyed via stable_symbol_id
--   edges_by_dst               — same treatment as edges
--   edges_unresolved           — src_id re-keyed (no dst — unresolved)
--   files                      — view over files.parquet
--   file_manifests             — view over file_manifests.parquet
--   tombstones                 — view over tombstones.parquet
--   v_cross_crate_calls        — resolved static calls crossing crate/package boundaries
--   v_import_licensed_precision_gate
--                              — zero-row-on-pass witness/language gate for import_licensed calls
--   _meta                      — manifest metadata and row counts
--   PROPERTY GRAPH code        — DuckPGQ surface
--   onager_edges(src, dst)     — Onager surface (BIGINT-keyed, globally unique)
--
-- Why the re-key: the upstream Parquet's `node_id` column is NOT globally unique —
-- empirically (artifact 3744e65c…) 28543 rows have only 27868 distinct `node_id`s,
-- with 675 collisions spread across multiple symbol kinds (sections, functions,
-- methods, structs, …). Joining Onager / PageRank output back to `nodes` via
-- node_id produced two rows per result. We rebuild the dense ID from the unique
-- `stable_symbol_id` column and re-key edges accordingly so all downstream queries
-- can rely on `node_id` being a primary key.

INSTALL duckpgq FROM community;
INSTALL onager  FROM community;
-- TODO: pin lance once DuckDB exposes a stable SQL pin syntax for community extension builds.
INSTALL lance;
LOAD duckpgq;
LOAD onager;
LOAD lance;

__SPUR_LANCE_ATTACH_SQL__

SET preserve_insertion_order = false;
SET memory_limit = '6GB';
SET threads = 4;

-- Build the globally-unique dense ID mapping from the upstream stable_symbol_id
-- (which IS guaranteed unique in the Parquet). Materialized as a TABLE because
-- every nodes/edges view joins against it.
--
-- The map covers every stable_symbol_id that appears in any input: nodes OR any
-- edge endpoint. This matters because the upstream edges Parquet references
-- some stable_ids (mostly `references_other` to types in std/external crates)
-- that are not present as rows in nodes.parquet — ~12k such danglers in the
-- current artifact. Without the UNION, an INNER-JOIN re-keying would silently
-- drop those edges. Including them in the map keeps edges intact; downstream
-- queries joining edges→nodes naturally filter them when needed.
CREATE OR REPLACE TABLE node_dense_id_map AS
WITH referenced_ids AS (
  SELECT stable_symbol_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/nodes.parquet')
  UNION
  SELECT source_stable_id AS stable_symbol_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges.parquet')
  UNION
  SELECT target_stable_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges.parquet')
  UNION
  SELECT source_stable_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_by_dst.parquet')
  UNION
  SELECT target_stable_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_by_dst.parquet')
  UNION
  SELECT source_stable_id FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_unresolved.parquet')
)
SELECT
  stable_symbol_id,
  ROW_NUMBER() OVER (ORDER BY stable_symbol_id) AS dense_id
FROM (
  SELECT DISTINCT stable_symbol_id
  FROM referenced_ids
  WHERE stable_symbol_id IS NOT NULL
);

CREATE OR REPLACE VIEW nodes AS
SELECT n.* REPLACE (m.dense_id AS node_id)
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/nodes.parquet') n
JOIN node_dense_id_map m USING (stable_symbol_id);

CREATE OR REPLACE VIEW edges AS
SELECT e.* REPLACE (
  s.dense_id AS src_id,
  d.dense_id AS dst_id
)
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges.parquet') e
JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id
JOIN node_dense_id_map d ON d.stable_symbol_id = e.target_stable_id;

CREATE OR REPLACE VIEW edges_by_dst AS
SELECT e.* REPLACE (
  s.dense_id AS src_id,
  d.dense_id AS dst_id
)
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_by_dst.parquet') e
JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id
JOIN node_dense_id_map d ON d.stable_symbol_id = e.target_stable_id;

-- Unresolved edges have no target_stable_id (that's why they're unresolved).
-- Re-key only the src side; dst_id stays as the upstream Parquet's value
-- (typically a placeholder / 0).
CREATE OR REPLACE VIEW edges_unresolved AS
SELECT e.* REPLACE (s.dense_id AS src_id)
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_unresolved.parquet') e
JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_id;

CREATE OR REPLACE VIEW files AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/files.parquet');

CREATE OR REPLACE VIEW file_manifests AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/file_manifests.parquet');

CREATE OR REPLACE VIEW tombstones AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/tombstones.parquet');

CREATE OR REPLACE TABLE _meta AS
WITH manifest_json AS (
  SELECT decode(content) AS content
  FROM read_blob('__SPUR_GRAPH_ARTIFACT_DIR__/manifest.json')
),
manifest AS (
  SELECT *
  FROM read_json_auto('__SPUR_GRAPH_ARTIFACT_DIR__/manifest.json')
)
SELECT '__SPUR_GRAPH_ARTIFACT_DIR__'        AS artifact_dir,
       graph_index_version,
       schema_version,
       manifest_version,
       graph_content_hash,
       extractor_version,
       complete,
       row_counts.nodes                    AS node_count,
       row_counts.edges                    AS resolved_edge_count,
       row_counts.edges_by_dst             AS edges_by_dst_count,
       row_counts.edges_unresolved         AS unresolved_edge_count,
       row_counts.files                    AS file_count,
       row_counts.file_manifests           AS file_manifest_count,
       row_counts.tombstones               AS tombstone_count,
       -- read_json_auto omits stripped temporal row_count keys; decode(read_blob) JSON avoids struct binder errors.
       TRY_CAST(json_extract(manifest_json.content, '$.row_counts.commits') AS BIGINT) AS commit_count,
       TRY_CAST(json_extract(manifest_json.content, '$.row_counts.symbol_snapshots') AS BIGINT) AS symbol_snapshot_count,
       TRY_CAST(json_extract(manifest_json.content, '$.row_counts.temporal_edges') AS BIGINT) AS temporal_edge_count,
       TRY_CAST(json_extract(manifest_json.content, '$.row_counts.diagnostics') AS BIGINT) AS diagnostic_count,
       edges_by_dst_present,
       parquet_writer.compression          AS parquet_compression,
       parquet_writer.row_group_size       AS parquet_row_group_size
FROM manifest
CROSS JOIN manifest_json;

CREATE OR REPLACE VIEW external_nodes AS
SELECT
  n.*,
  CASE
    WHEN regexp_extract(n.qualified_name, '^[^:./]+', 0) IN ('std', 'core', 'alloc') THEN 'std'
    ELSE regexp_extract(n.qualified_name, '^[^:./]+', 0)
  END AS origin
FROM nodes n
WHERE n.symbol_kind = 'external';

CREATE OR REPLACE VIEW v_dependency_surface AS
SELECT
  CASE
    WHEN s.file_path LIKE 'crates/%' THEN regexp_extract(s.file_path, '^crates/([^/]+)', 1)
    ELSE regexp_extract(s.file_path, '^[^/]+', 0)
  END AS crate_name,
  s.file_path,
  ext.origin AS external_origin,
  ext.qualified_name AS external_symbol,
  ext.stable_symbol_id AS external_stable_symbol_id,
  count(*) AS inbound_import_count
FROM edges e
JOIN nodes s
  ON s.stable_symbol_id = e.source_stable_id
JOIN external_nodes ext
  ON ext.stable_symbol_id = e.target_stable_id
WHERE e.relation = 'imports'
GROUP BY
  crate_name,
  s.file_path,
  ext.origin,
  ext.qualified_name,
  ext.stable_symbol_id;

CREATE OR REPLACE VIEW v_cross_crate_calls AS
WITH resolved_calls AS (
  SELECT
    e.source_stable_id AS source_stable_symbol_id,
    e.target_stable_id AS target_stable_symbol_id,
    e.target_label,
    COALESCE(e.edge_kind, 'calls') AS edge_kind,
    e.relation,
    e.confidence,
    e.confidence_score,
    e.bind_method,
    CASE
      WHEN s.file_path LIKE 'crates/%' THEN regexp_extract(s.file_path, '^crates/([^/]+)', 1)
      ELSE regexp_extract(s.file_path, '^[^/]+', 0)
    END AS source_crate,
    s.file_path AS source_file_path,
    s.entity_name AS source_symbol,
    s.qualified_name AS source_qualified_symbol,
    s.symbol_kind AS source_symbol_kind,
    CASE
      WHEN t.file_path LIKE 'crates/%' THEN regexp_extract(t.file_path, '^crates/([^/]+)', 1)
      ELSE regexp_extract(t.file_path, '^[^/]+', 0)
    END AS target_crate,
    t.file_path AS target_file_path,
    t.entity_name AS target_symbol,
    t.qualified_name AS target_qualified_symbol,
    t.symbol_kind AS target_symbol_kind
  FROM edges e
  JOIN nodes s
    ON s.stable_symbol_id = e.source_stable_id
  JOIN nodes t
    ON t.stable_symbol_id = e.target_stable_id
  WHERE e.relation = 'calls'
    AND COALESCE(e.edge_kind, 'calls') = 'calls'
    AND t.symbol_kind <> 'external'
)
SELECT *
FROM resolved_calls
WHERE source_crate <> target_crate;

-- Standing precision gate for the P3 import_licensed arm. The view returns
-- violations, so the expected count is 0 on each checked corpus.
CREATE OR REPLACE VIEW v_import_licensed_precision_gate AS
WITH licensed_calls AS (
  SELECT
    e.source_stable_id AS source_stable_symbol_id,
    e.target_stable_id AS target_stable_symbol_id,
    e.target_label,
    s.file_path AS source_file_path,
    s.entity_name AS source_symbol,
    s.qualified_name AS source_qualified_symbol,
    t.file_path AS target_file_path,
    t.entity_name AS target_symbol,
    t.qualified_name AS target_qualified_symbol,
    CASE
      WHEN lower(s.file_path) LIKE '%.rs' THEN 'rust'
      WHEN lower(s.file_path) LIKE '%.py' THEN 'python'
      WHEN lower(s.file_path) LIKE '%.js'
        OR lower(s.file_path) LIKE '%.jsx'
        OR lower(s.file_path) LIKE '%.ts'
        OR lower(s.file_path) LIKE '%.tsx' THEN 'javascript'
      ELSE 'unknown'
    END AS source_language_family,
    CASE
      WHEN lower(t.file_path) LIKE '%.rs' THEN 'rust'
      WHEN lower(t.file_path) LIKE '%.py' THEN 'python'
      WHEN lower(t.file_path) LIKE '%.js'
        OR lower(t.file_path) LIKE '%.jsx'
        OR lower(t.file_path) LIKE '%.ts'
        OR lower(t.file_path) LIKE '%.tsx' THEN 'javascript'
      ELSE 'unknown'
    END AS target_language_family
  FROM edges e
  JOIN nodes s
    ON s.stable_symbol_id = e.source_stable_id
  JOIN nodes t
    ON t.stable_symbol_id = e.target_stable_id
  WHERE e.relation = 'calls'
    AND COALESCE(e.edge_kind, 'calls') = 'calls'
    AND e.bind_method = 'import_licensed'
    AND t.symbol_kind <> 'external'
),
checked AS (
  SELECT
    l.*,
    EXISTS (
      SELECT 1
      FROM edges witness
      JOIN nodes witness_source
        ON witness_source.stable_symbol_id = witness.source_stable_id
      WHERE witness.relation = 'imports'
        AND witness.target_stable_id = l.target_stable_symbol_id
        AND witness_source.file_path = l.source_file_path
        AND (
          witness.target_label = l.target_symbol
          OR witness.import_path = l.target_symbol
          OR witness.import_path LIKE '%::' || l.target_symbol
        )
    ) AS witness_backed,
    source_language_family <> 'unknown'
      AND target_language_family <> 'unknown'
      AND source_language_family <> target_language_family AS cross_language
  FROM licensed_calls l
)
SELECT
  CASE
    WHEN NOT witness_backed THEN 'missing_witness'
    ELSE 'cross_language'
  END AS violation_kind,
  source_stable_symbol_id,
  target_stable_symbol_id,
  target_label,
  source_file_path,
  source_symbol,
  source_qualified_symbol,
  target_file_path,
  target_symbol,
  target_qualified_symbol,
  source_language_family,
  target_language_family
FROM checked
WHERE NOT witness_backed
   OR cross_language;

-- DuckPGQ currently rejects property graphs over views, so keep a small
-- compatibility surface sourced from the Parquet views. Analytical SQL and
-- Onager paths use the views directly.
CREATE OR REPLACE TABLE duckpgq_nodes AS
SELECT stable_symbol_id,
       node_id,
       qualified_name,
       entity_name,
       symbol_kind,
       file_path
FROM nodes;

CREATE OR REPLACE TABLE duckpgq_external_nodes AS
SELECT stable_symbol_id,
       node_id,
       qualified_name,
       entity_name,
       symbol_kind,
       file_path
FROM duckpgq_nodes
WHERE symbol_kind = 'external';

CREATE OR REPLACE TABLE duckpgq_edges AS
SELECT source_stable_id,
       target_stable_id,
       edge_kind,
       relation,
       confidence,
       bind_method
FROM edges;

CREATE OR REPLACE TABLE duckpgq_import_edges AS
SELECT e.*
FROM duckpgq_edges e
JOIN duckpgq_external_nodes n
  ON n.stable_symbol_id = e.target_stable_id
WHERE e.relation = 'imports';

CREATE OR REPLACE TABLE duckpgq_cross_crate_call_edges AS
SELECT DISTINCT e.*
FROM duckpgq_edges e
JOIN v_cross_crate_calls c
  ON c.source_stable_symbol_id = e.source_stable_id
 AND c.target_stable_symbol_id = e.target_stable_id
 AND c.relation = e.relation
 AND c.edge_kind = COALESCE(e.edge_kind, 'calls');

CREATE OR REPLACE VIEW onager_edges AS
SELECT src_id AS src, dst_id AS dst
FROM   edges
WHERE  edge_kind = 'calls';

CREATE OR REPLACE PROPERTY GRAPH code
  VERTEX TABLES (
    duckpgq_nodes PROPERTIES (
      stable_symbol_id, node_id, qualified_name, entity_name, symbol_kind, file_path
    ),
    duckpgq_external_nodes PROPERTIES (
      stable_symbol_id, node_id, qualified_name, entity_name, symbol_kind, file_path
    ) LABEL External
  )
  EDGE TABLES (
    duckpgq_edges SOURCE      KEY (source_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                 DESTINATION KEY (target_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                 PROPERTIES  (edge_kind, relation, confidence, bind_method),
    duckpgq_import_edges SOURCE      KEY (source_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                         DESTINATION KEY (target_stable_id) REFERENCES duckpgq_external_nodes (stable_symbol_id)
                         PROPERTIES  (edge_kind, relation, confidence, bind_method)
                         LABEL imports,
    duckpgq_cross_crate_call_edges SOURCE      KEY (source_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                                  DESTINATION KEY (target_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                                  PROPERTIES  (edge_kind, relation, confidence, bind_method)
                                  LABEL cross_crate_calls
  );
