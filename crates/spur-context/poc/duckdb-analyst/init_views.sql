-- Analyst views for code-review and co-change use cases.
--
-- Depends on the views defined in init_temporal.sql (commits, symbol_snapshots,
-- temporal_edges) and on the base tables loaded by init.sql (nodes, edges).
-- Safe to re-run; all CREATE OR REPLACE.
--
-- Window for "recent" churn is 90 days, hardcoded for now. To narrow/widen,
-- override at query time with: WHERE author_ts > now() - INTERVAL '30 day'.

-- ---------------------------------------------------------------------------
-- Helper: decoded symbol_snapshots path (works around unpadded base64).
-- The temporal-init view's `file_path` column can fail TRY_CAST on rows whose
-- file_path_b64 is not length-mod-4; we pad explicitly here.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW v_symbol_file AS
SELECT DISTINCT
  stable_symbol_id,
  commit_sha,
  decode(
    from_base64(
      file_path_b64 || repeat('=', (4 - length(file_path_b64) % 4) % 4)
    )
  )::VARCHAR AS file_path
FROM symbol_snapshots;

-- ---------------------------------------------------------------------------
-- Per-symbol inbound-edge counts, by edge_kind. Static "blast radius" weight.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW v_symbol_inbound AS
SELECT
  target_stable_id AS stable_symbol_id,
  SUM(CASE WHEN edge_kind IN ('calls', 'calls_dyn') THEN 1 ELSE 0 END) AS callers,
  SUM(CASE WHEN edge_kind = 'references_other' AND relation = 'imports' THEN 1 ELSE 0 END) AS importers,
  SUM(CASE WHEN edge_kind = 'references_other' AND relation = 'contains' THEN 1 ELSE 0 END) AS containers,
  COUNT(*) AS inbound_total
FROM edges
GROUP BY target_stable_id;

-- ---------------------------------------------------------------------------
-- Per-symbol temporal activity in the last 90 days. Used as a "self churn"
-- and as a multiplier when scoring caller blast radius.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW v_symbol_churn_90d AS
SELECT
  t.target_stable_symbol_id AS stable_symbol_id,
  COUNT(*)                                                          AS events,
  COUNT(DISTINCT t.source_commit)                                   AS commits,
  SUM(CASE WHEN t.change_kind = 'added' THEN 1 ELSE 0 END)          AS added,
  SUM(CASE WHEN t.change_kind = 'modified' THEN 1 ELSE 0 END)       AS modified,
  SUM(CASE WHEN t.change_kind = 'deleted' THEN 1 ELSE 0 END)        AS deleted,
  SUM(CASE WHEN t.change_kind = 'renamed_from_symbol' THEN 1 ELSE 0 END) AS renamed,
  MAX(c.author_ts)                                                  AS last_touched
FROM temporal_edges t
JOIN commits c ON c.sha = t.source_commit
WHERE c.author_ts > now() - INTERVAL '90 day'
  AND t.target_stable_symbol_id IS NOT NULL
GROUP BY t.target_stable_symbol_id;

-- ---------------------------------------------------------------------------
-- USE CASE #1: blast-radius–weighted review scoring.
--
-- For every current symbol (in `nodes`), compute:
--   * caller_count          - inbound 'calls'/'calls_dyn' edges
--   * caller_churn_90d      - SUM of recent events across all callers
--   * self_churn_90d        - this symbol's own recent events
--   * blast_radius_score    - log-scaled composite for ranking review priority
--
-- To use during a PR review:
--   SELECT * FROM v_blast_radius
--   WHERE stable_symbol_id IN (<symbols touched by the PR>)
--   ORDER BY blast_radius_score DESC;
-- ---------------------------------------------------------------------------
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
    COUNT(DISTINCT ce.caller_stable_id)        AS caller_count,
    COALESCE(SUM(c.events), 0)                 AS caller_churn_90d,
    COUNT(DISTINCT CASE WHEN c.events > 0 THEN ce.caller_stable_id END)
                                               AS hot_caller_count
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
  COALESCE(cc.caller_count, 0)         AS caller_count,
  COALESCE(cc.hot_caller_count, 0)     AS hot_caller_count,
  COALESCE(cc.caller_churn_90d, 0)     AS caller_churn_90d,
  COALESCE(sc.events, 0)               AS self_churn_90d,
  sc.last_touched                      AS self_last_touched,
  -- Score: caller-side weight + self-side weight. Log-scale so a symbol with
  -- 200 callers doesn't dwarf one with 20 high-churn callers.
  (
    ln(1 + COALESCE(cc.caller_count, 0)) *
    ln(1 + COALESCE(cc.caller_churn_90d, 0))
  ) + ln(1 + COALESCE(sc.events, 0))   AS blast_radius_score
FROM nodes n
LEFT JOIN caller_churn cc      ON cc.stable_symbol_id = n.stable_symbol_id
LEFT JOIN v_symbol_churn_90d sc ON sc.stable_symbol_id = n.stable_symbol_id;

-- ---------------------------------------------------------------------------
-- USE CASE #2: file-level co-change hotspots.
--
-- For every unordered pair of files that appeared in the same commit, count
-- the co-occurrences and flag whether ANY static edge connects the two files.
-- Pairs with high co-change but NO static edge = hidden coupling.
--
-- Tunable thresholds in queries, e.g.:
--   SELECT * FROM v_file_cochange
--   WHERE cochange_count >= 5 AND NOT has_static_edge
--   ORDER BY cochange_count DESC;
-- ---------------------------------------------------------------------------

-- Per (commit, file) the set of files that commit touched (DISTINCT).
CREATE OR REPLACE VIEW v_commit_files AS
SELECT DISTINCT
  t.source_commit AS commit_sha,
  sf.file_path
FROM temporal_edges t
JOIN v_symbol_file sf
  ON sf.stable_symbol_id = t.target_stable_symbol_id
 AND sf.commit_sha       = t.target_commit
WHERE sf.file_path IS NOT NULL;

-- Per-file → per-file static-edge bridge derived from current symbol edges.
-- A and B are connected if ANY symbol in A has an edge to ANY symbol in B.
CREATE OR REPLACE VIEW v_file_static_edges AS
SELECT DISTINCT
  na.file_path AS file_a,
  nb.file_path AS file_b
FROM edges e
JOIN nodes na ON na.stable_symbol_id = e.source_stable_id
JOIN nodes nb ON nb.stable_symbol_id = e.target_stable_id
WHERE na.file_path IS NOT NULL
  AND nb.file_path IS NOT NULL
  AND na.file_path <> nb.file_path;

CREATE OR REPLACE VIEW v_file_cochange AS
WITH pairs AS (
  SELECT
    LEAST(a.file_path, b.file_path)    AS file_a,
    GREATEST(a.file_path, b.file_path) AS file_b,
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
    COUNT(DISTINCT commit_sha) AS cochange_count
  FROM pairs
  GROUP BY file_a, file_b
),
static AS (
  SELECT DISTINCT
    LEAST(file_a, file_b)    AS file_a,
    GREATEST(file_a, file_b) AS file_b
  FROM v_file_static_edges
)
SELECT
  a.file_a,
  a.file_b,
  a.cochange_count,
  (s.file_a IS NOT NULL) AS has_static_edge
FROM agg a
LEFT JOIN static s
  ON s.file_a = a.file_a AND s.file_b = a.file_b;
