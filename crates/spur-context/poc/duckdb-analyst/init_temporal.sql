CREATE OR REPLACE MACRO b64_decode_lenient(s) AS (
  from_base64(replace(replace(s, '-', '+'), '_', '/') || repeat('=', (4 - length(s) % 4) % 4))
);

INSERT INTO node_dense_id_map (stable_symbol_id, dense_id)
WITH referenced_ids AS (
  SELECT key_stable_symbol_id AS stable_symbol_id
  FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/symbol_snapshots.parquet')
  UNION
  SELECT source_stable_symbol_id AS stable_symbol_id
  FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/temporal_edges.parquet')
  WHERE source_stable_symbol_id IS NOT NULL
  UNION
  SELECT target_stable_symbol_id AS stable_symbol_id
  FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/temporal_edges.parquet')
  WHERE target_stable_symbol_id IS NOT NULL
),
missing_ids AS (
  SELECT DISTINCT r.stable_symbol_id
  FROM referenced_ids r
  LEFT JOIN node_dense_id_map m ON m.stable_symbol_id = r.stable_symbol_id
  WHERE r.stable_symbol_id IS NOT NULL
    AND m.dense_id IS NULL
),
existing_ids AS (
  SELECT dense_id FROM node_dense_id_map
  UNION ALL
  SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM node_dense_id_map)
),
max_existing AS (
  SELECT MAX(dense_id) OVER () AS max_dense_id
  FROM existing_ids
  LIMIT 1
)
SELECT
  missing_ids.stable_symbol_id,
  max_existing.max_dense_id
    + ROW_NUMBER() OVER (ORDER BY missing_ids.stable_symbol_id) AS dense_id
FROM missing_ids
CROSS JOIN max_existing;

CREATE OR REPLACE VIEW commits AS
SELECT
  c.*,
  to_timestamp(c.author_time) AS author_ts
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/commits.parquet') c;

CREATE OR REPLACE VIEW symbol_snapshots AS
SELECT
  s.key_stable_symbol_id AS stable_symbol_id,
  s.key_commit AS commit_sha,
  m.dense_id AS node_id,
  TRY_CAST(decode(b64_decode_lenient(s.file_path_b64)) AS VARCHAR) AS file_path,
  s.file_path_b64,
  s.entity_name,
  s.symbol_kind,
  s.enclosing_scope,
  s.byte_range_start,
  s.byte_range_end,
  s.line_range_start,
  s.line_range_end,
  s.anchor_hash,
  s.tokens
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/symbol_snapshots.parquet') s
LEFT JOIN node_dense_id_map m ON m.stable_symbol_id = s.key_stable_symbol_id;

CREATE OR REPLACE VIEW temporal_edges AS
SELECT
  e.*,
  s.dense_id AS source_node_id,
  t.dense_id AS target_node_id
FROM read_parquet('__SPUR_GRAPH_ARTIFACT_DIR__/temporal_edges.parquet') e
LEFT JOIN node_dense_id_map s ON s.stable_symbol_id = e.source_stable_symbol_id
LEFT JOIN node_dense_id_map t ON t.stable_symbol_id = e.target_stable_symbol_id;
