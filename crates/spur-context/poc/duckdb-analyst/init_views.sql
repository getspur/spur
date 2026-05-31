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

-- Coverage guard: of the current symbol nodes the temporal walk is expected to
-- track, at least 90% must join directly to a symbol_snapshots row by
-- stable_symbol_id.
--
-- We count DISTINCT nodes, NOT raw join rows. A node joins all of its historical
-- snapshots, so `COUNT(*) FROM nodes JOIN symbol_snapshots` is inflated by churn
-- multiplicity (~2x here) and can sit above node_count even when half the symbols
-- have no snapshot at all — it gives false PASS and false FAIL signals. Distinct
-- coverage is the honest measure.
--
-- "Expected to track" deliberately excludes:
--   * markdown `section` and synthetic `mcp_tool` — the git-walk symbol diff never
--     emits these as snapshots, so counting them can never reach the threshold;
--   * symbols in files the walk never saw (untracked / brand-new) — they have no
--     committed history and legitimately have no snapshot.
--
-- The threshold is 90%, not 99%, on purpose: stable_symbol_id embeds the symbol's
-- byte offset, so position drift (e.g. churny fields shifting within a file)
-- leaves a few percent unmatched even on a healthy index. 90% still trips on a
-- stale temporal store — e.g. an extractor/query upgrade that left new member
-- kinds unwalked drops trackable coverage well below 90%. Raise toward 99% once
-- node↔snapshot identity is made position-independent.
CREATE OR REPLACE TEMP TABLE _assert_symbol_snapshot_direct_join_coverage AS
WITH walked_files AS (
  SELECT DISTINCT file_path FROM symbol_snapshots WHERE file_path IS NOT NULL
),
trackable_nodes AS (
  SELECT DISTINCT n.stable_symbol_id
  FROM nodes n
  JOIN walked_files w ON w.file_path = n.file_path
  WHERE n.stable_symbol_id IS NOT NULL
    AND n.symbol_kind NOT IN ('section', 'mcp_tool')
),
snapshot_ids AS (
  SELECT DISTINCT stable_symbol_id FROM symbol_snapshots
),
coverage AS (
  SELECT
    (SELECT COUNT(*) FROM trackable_nodes) AS expected_nodes,
    (SELECT COUNT(*) FROM trackable_nodes t
       WHERE t.stable_symbol_id IN (SELECT stable_symbol_id FROM snapshot_ids)) AS covered_nodes
)
SELECT
  CASE
    WHEN expected_nodes = 0 THEN error(
      'symbol_snapshot coverage guard found no temporally-trackable nodes; '
      || 'temporal index appears empty'
    )
    WHEN covered_nodes * 100 >= expected_nodes * 90 THEN covered_nodes
    ELSE error(
      'distinct symbol_snapshot coverage below 90%: '
      || CAST(covered_nodes AS VARCHAR)
      || ' of '
      || CAST(expected_nodes AS VARCHAR)
      || ' temporally-trackable nodes joined '
      || '(excludes section/mcp_tool and never-walked files)'
    )
  END AS covered_nodes
FROM coverage;

DROP TABLE _assert_symbol_snapshot_direct_join_coverage;

CREATE OR REPLACE VIEW v_symbol_file AS
SELECT DISTINCT
  stable_symbol_id,
  commit_sha,
  TRY_CAST(decode(b64_decode_lenient(file_path_b64)) AS VARCHAR) AS file_path
FROM symbol_snapshots;

CREATE OR REPLACE VIEW v_symbol_churn_90d AS
WITH structural_symbols AS (
  SELECT
    stable_symbol_id AS structural_stable_symbol_id,
    file_path,
    entity_name,
    symbol_kind,
    enclosing_scope
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
    MIN(structural_stable_symbol_id) AS structural_stable_symbol_id
  FROM structural_symbols
  GROUP BY file_path, entity_name, symbol_kind, enclosing_scope
  HAVING COUNT(*) = 1
     AND COUNT(DISTINCT structural_stable_symbol_id) = 1
),
snapshot_symbols AS (
  SELECT DISTINCT
    stable_symbol_id AS snapshot_stable_symbol_id,
    file_path,
    entity_name,
    symbol_kind,
    enclosing_scope
  FROM symbol_snapshots
  WHERE stable_symbol_id IS NOT NULL
    AND file_path IS NOT NULL
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
),
snapshot_churn AS (
  SELECT
    s.snapshot_stable_symbol_id,
    count(*) AS events,
    count(DISTINCT t.source_commit) AS commits,
    sum(CASE WHEN t.change_kind = 'added' THEN 1 ELSE 0 END) AS added,
    sum(CASE WHEN t.change_kind = 'modified' THEN 1 ELSE 0 END) AS modified,
    sum(CASE WHEN t.change_kind = 'deleted' THEN 1 ELSE 0 END) AS deleted,
    sum(CASE WHEN t.change_kind = 'renamed_from_symbol' THEN 1 ELSE 0 END) AS renamed,
    max(c.author_ts) AS last_touched
  FROM (
    SELECT DISTINCT snapshot_stable_symbol_id
    FROM snapshot_symbols
  ) s
  JOIN temporal_edges t
    ON t.target_stable_symbol_id = s.snapshot_stable_symbol_id
  JOIN commits c
    ON c.sha = t.source_commit
  WHERE c.author_ts > (now() - INTERVAL '90 day')
  GROUP BY s.snapshot_stable_symbol_id
)
SELECT
  su.structural_stable_symbol_id AS stable_symbol_id,
  sc.events,
  sc.commits,
  sc.added,
  sc.modified,
  sc.deleted,
  sc.renamed,
  sc.last_touched
FROM snapshot_churn sc
JOIN snapshot_unique sku
  ON sku.snapshot_stable_symbol_id = sc.snapshot_stable_symbol_id
JOIN structural_unique su
  ON su.file_path = sku.file_path
 AND su.entity_name = sku.entity_name
 AND su.symbol_kind = sku.symbol_kind
 AND su.enclosing_scope IS NOT DISTINCT FROM sku.enclosing_scope;

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
    AND COALESCE(e.bind_method, '') != 'macro_body_singleton'
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
