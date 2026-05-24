-- Analyst views for code-review and co-change use cases.
--
-- Depends on the views defined in init_temporal.sql (commits, symbol_snapshots,
-- temporal_edges) and on the base tables loaded by init.sql (nodes, edges).
-- Safe to re-run; all CREATE OR REPLACE.
--
-- Window for "recent" churn is 90 days, hardcoded for now. To narrow/widen,
-- override at query time with: WHERE author_ts > now() - INTERVAL '30 day'.

CREATE OR REPLACE MACRO b64_decode_lenient(s) AS (
  from_base64(replace(replace(s, '-', '+'), '_', '/') || repeat('=', (4 - length(s) % 4) % 4))
);

-- TRANSITIONAL. Bridges divergent stable_symbol_id recipes between structural extractor
-- and temporal writer. Remove once both writers call one shared identity function.
-- Tracked in bd-sidbridge1.
CREATE OR REPLACE VIEW v_symbol_id_bridge AS
WITH structural_symbols AS (
  SELECT
    stable_symbol_id AS structural_stable_symbol_id,
    node_id AS structural_node_id,
    file_path,
    entity_name,
    symbol_kind,
    enclosing_scope,
    line_start
  FROM nodes
  WHERE file_path IS NOT NULL
    AND entity_name IS NOT NULL
    AND symbol_kind IS NOT NULL
),
structural_unique AS (
  SELECT
    file_path,
    entity_name,
    symbol_kind,
    enclosing_scope,
    MIN(structural_stable_symbol_id) AS structural_stable_symbol_id,
    MIN(structural_node_id) AS structural_node_id,
    MIN(line_start) AS structural_line_start
  FROM structural_symbols
  GROUP BY file_path, entity_name, symbol_kind, enclosing_scope
  HAVING COUNT(*) = 1
     AND COUNT(DISTINCT structural_stable_symbol_id) = 1
),
snapshot_symbols AS (
  SELECT DISTINCT
    stable_symbol_id AS snapshot_stable_symbol_id,
    TRY_CAST(decode(b64_decode_lenient(file_path_b64)) AS VARCHAR) AS file_path,
    entity_name,
    symbol_kind,
    enclosing_scope
  FROM symbol_snapshots
  WHERE file_path_b64 IS NOT NULL
    AND entity_name IS NOT NULL
    AND symbol_kind IS NOT NULL
),
snapshot_unique AS (
  SELECT
    file_path,
    entity_name,
    symbol_kind,
    enclosing_scope,
    MIN(snapshot_stable_symbol_id) AS snapshot_stable_symbol_id
  FROM snapshot_symbols
  GROUP BY file_path, entity_name, symbol_kind, enclosing_scope
  HAVING COUNT(*) = 1
     AND COUNT(DISTINCT snapshot_stable_symbol_id) = 1
)
SELECT
  su.structural_stable_symbol_id,
  sku.snapshot_stable_symbol_id,
  su.structural_node_id,
  su.file_path,
  su.entity_name,
  su.symbol_kind,
  su.enclosing_scope,
  su.structural_line_start
FROM structural_unique su
JOIN snapshot_unique sku
  ON sku.file_path = su.file_path
 AND sku.entity_name = su.entity_name
 AND sku.symbol_kind = su.symbol_kind
 AND sku.enclosing_scope IS NOT DISTINCT FROM su.enclosing_scope;

CREATE OR REPLACE VIEW v_symbol_file AS
SELECT DISTINCT
  stable_symbol_id,
  commit_sha,
  TRY_CAST(decode(b64_decode_lenient(file_path_b64)) AS VARCHAR) AS file_path
FROM symbol_snapshots;

CREATE OR REPLACE VIEW v_symbol_churn_90d AS
SELECT
  b.structural_stable_symbol_id AS stable_symbol_id,
  count(*) AS events,
  count(DISTINCT t.source_commit) AS commits,
  sum(CASE WHEN t.change_kind = 'added' THEN 1 ELSE 0 END) AS added,
  sum(CASE WHEN t.change_kind = 'modified' THEN 1 ELSE 0 END) AS modified,
  sum(CASE WHEN t.change_kind = 'deleted' THEN 1 ELSE 0 END) AS deleted,
  sum(CASE WHEN t.change_kind = 'renamed_from_symbol' THEN 1 ELSE 0 END) AS renamed,
  max(c.author_ts) AS last_touched
FROM temporal_edges t
JOIN v_symbol_id_bridge b
  ON b.snapshot_stable_symbol_id = t.target_stable_symbol_id
JOIN commits c
  ON c.sha = t.source_commit
WHERE c.author_ts > (now() - INTERVAL '90 day')
  AND t.target_stable_symbol_id IS NOT NULL
GROUP BY b.structural_stable_symbol_id;

CREATE OR REPLACE VIEW v_symbol_inbound AS
SELECT
  target_stable_id AS stable_symbol_id,
  sum(CASE WHEN edge_kind IN ('calls', 'calls_dyn') THEN 1 ELSE 0 END) AS callers,
  sum(CASE WHEN edge_kind = 'references_other' AND relation = 'imports' THEN 1 ELSE 0 END) AS importers,
  sum(CASE WHEN edge_kind = 'references_other' AND relation = 'contains' THEN 1 ELSE 0 END) AS containers,
  count(*) AS inbound_total
FROM edges
GROUP BY target_stable_id;

CREATE OR REPLACE VIEW v_blast_radius AS
WITH caller_edges AS (
  SELECT
    e.target_stable_id AS stable_symbol_id,
    e.source_stable_id AS caller_stable_id
  FROM edges e
  WHERE e.edge_kind IN ('calls', 'calls_dyn')
),
caller_churn AS (
  SELECT
    ce.stable_symbol_id,
    count(DISTINCT ce.caller_stable_id) AS caller_count,
    COALESCE(sum(c.events), 0) AS caller_churn_90d,
    count(DISTINCT CASE WHEN c.events > 0 THEN ce.caller_stable_id END) AS hot_caller_count
  FROM caller_edges ce
  LEFT JOIN v_symbol_churn_90d c
    ON c.stable_symbol_id = ce.caller_stable_id
  GROUP BY ce.stable_symbol_id
)
SELECT
  n.stable_symbol_id,
  n.entity_name,
  n.symbol_kind,
  n.file_path,
  COALESCE(cc.caller_count, 0) AS caller_count,
  COALESCE(cc.hot_caller_count, 0) AS hot_caller_count,
  COALESCE(cc.caller_churn_90d, 0) AS caller_churn_90d,
  COALESCE(sc.events, 0) AS self_churn_90d,
  sc.last_touched AS self_last_touched,
  CASE
    WHEN COALESCE((SELECT temporal_edge_count FROM _meta LIMIT 1), 0) = 0 THEN NULL
    ELSE
      ln(1 + COALESCE(cc.caller_count, 0))
      * ln(1 + COALESCE(cc.caller_churn_90d, 0))
      + ln(1 + COALESCE(sc.events, 0))
  END AS blast_radius_score
FROM nodes n
LEFT JOIN caller_churn cc
  ON cc.stable_symbol_id = n.stable_symbol_id
LEFT JOIN v_symbol_churn_90d sc
  ON sc.stable_symbol_id = n.stable_symbol_id;

CREATE OR REPLACE VIEW v_commit_files AS
SELECT DISTINCT
  t.source_commit AS commit_sha,
  sf.file_path
FROM temporal_edges t
JOIN v_symbol_file sf
  ON sf.stable_symbol_id = t.target_stable_symbol_id
 AND sf.commit_sha = t.target_commit
WHERE sf.file_path IS NOT NULL;

CREATE OR REPLACE VIEW v_file_static_edges AS
SELECT DISTINCT
  na.file_path AS file_a,
  nb.file_path AS file_b
FROM edges e
JOIN nodes na
  ON na.stable_symbol_id = e.source_stable_id
JOIN nodes nb
  ON nb.stable_symbol_id = e.target_stable_id
WHERE na.file_path IS NOT NULL
  AND nb.file_path IS NOT NULL
  AND na.file_path != nb.file_path;

CREATE OR REPLACE VIEW v_file_cochange AS
WITH pairs AS (
  SELECT
    least(a.file_path, b.file_path) AS file_a,
    greatest(a.file_path, b.file_path) AS file_b,
    a.commit_sha
  FROM v_commit_files a
  JOIN v_commit_files b
    ON a.commit_sha = b.commit_sha
   AND a.file_path < b.file_path
),
agg AS (
  SELECT
    file_a,
    file_b,
    count(DISTINCT commit_sha) AS cochange_count
  FROM pairs
  GROUP BY file_a, file_b
),
static AS (
  SELECT DISTINCT
    least(file_a, file_b) AS file_a,
    greatest(file_a, file_b) AS file_b
  FROM v_file_static_edges
)
SELECT
  a.file_a,
  a.file_b,
  a.cochange_count,
  s.file_a IS NOT NULL AS has_static_edge
FROM agg a
LEFT JOIN static s
  ON s.file_a = a.file_a
 AND s.file_b = a.file_b;
