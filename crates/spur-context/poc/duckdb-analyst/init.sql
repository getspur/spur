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
--   nodes                      — view over nodes.parquet
--   edges                      — view over edges.parquet
--   edges_by_dst               — view over edges_by_dst.parquet
--   edges_unresolved           — view over edges_unresolved.parquet
--   files                      — view over files.parquet
--   file_manifests             — view over file_manifests.parquet
--   tombstones                 — view over tombstones.parquet
--   _meta                      — manifest metadata and row counts
--   PROPERTY GRAPH code        — DuckPGQ surface
--   onager_edges(src, dst)     — Onager surface (BIGINT-keyed)

INSTALL duckpgq FROM community;
INSTALL onager  FROM community;
LOAD duckpgq;
LOAD onager;

SET preserve_insertion_order = false;
SET memory_limit = '6GB';
SET threads = 4;

CREATE OR REPLACE VIEW nodes AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/nodes.parquet');

CREATE OR REPLACE VIEW edges AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges.parquet');

CREATE OR REPLACE VIEW edges_by_dst AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_by_dst.parquet');

CREATE OR REPLACE VIEW edges_unresolved AS
SELECT *
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/edges_unresolved.parquet');

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
       edges_by_dst_present,
       parquet_writer.compression          AS parquet_compression,
       parquet_writer.row_group_size       AS parquet_row_group_size
FROM read_json_auto('__SPUR_GRAPH_ARTIFACT_DIR__/manifest.json');

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

CREATE OR REPLACE TABLE duckpgq_edges AS
SELECT source_stable_id,
       target_stable_id,
       edge_kind,
       relation,
       confidence
FROM edges;

CREATE OR REPLACE VIEW onager_edges AS
SELECT src_id AS src, dst_id AS dst
FROM   edges
WHERE  edge_kind = 'calls';

CREATE OR REPLACE PROPERTY GRAPH code
  VERTEX TABLES (
    duckpgq_nodes PROPERTIES (
      stable_symbol_id, node_id, qualified_name, entity_name, symbol_kind, file_path
    )
  )
  EDGE TABLES (
    duckpgq_edges SOURCE      KEY (source_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                 DESTINATION KEY (target_stable_id) REFERENCES duckpgq_nodes (stable_symbol_id)
                 PROPERTIES  (edge_kind, relation, confidence)
  );
