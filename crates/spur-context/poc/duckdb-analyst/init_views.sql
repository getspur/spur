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

-- ============================================================================
-- One-stop analyst surface (Tier B).
--
-- Lazy views that compose the static graph, the temporal layer, and the
-- materialized algorithm tables from init_algorithms.sql into the named
-- answers an analyst actually asks for. The goal: one named view per question,
-- so the brain never hand-rolls an Onager call or a four-table join.
--
-- Dependency note: v_symbol_risk / v_symbol_scorecard reference the
-- v_symbol_centrality / v_symbol_component / v_symbol_community views created in
-- init_algorithms.sql, which the analyst build concatenates BEFORE this file.
-- ============================================================================

-- Conventional-commit classification — feat/fix/docs/refactor/test/chore/...
-- `commit_type` is the lowercase prefix before the first '(' / ':' / '!'.
CREATE OR REPLACE VIEW v_commit_classified AS
SELECT
  c.sha,
  c.author_ts,
  c.summary,
  NULLIF(regexp_extract(c.summary, '^([a-z]+)(\(|:|!)', 1), '') AS commit_type,
  (c.summary LIKE 'fix%') AS is_fix
FROM commits c;

-- Fix-magnets — per-file commit volume, fix-commit count, and fix-rate.
-- The defect-density signal (replaces bus-factor on single-author repos).
CREATE OR REPLACE VIEW v_fix_hotspots AS
SELECT
  vcf.file_path,
  count(DISTINCT vcf.commit_sha) AS commits,
  count(DISTINCT CASE WHEN c.is_fix THEN vcf.commit_sha END) AS fix_commits,
  round(100.0 * count(DISTINCT CASE WHEN c.is_fix THEN vcf.commit_sha END)
        / nullif(count(DISTINCT vcf.commit_sha), 0), 1) AS fix_pct
FROM v_commit_files vcf
JOIN v_commit_classified c ON c.sha = vcf.commit_sha
GROUP BY vcf.file_path;

-- Hidden coupling — files that co-change but have NO static edge connecting
-- them. The highest-value refactoring signal: logical dependencies the call
-- graph cannot see (shared contracts, generated pairs, protocol seams).
CREATE OR REPLACE VIEW v_hidden_coupling AS
SELECT file_a, file_b, cochange_count
FROM v_file_cochange
WHERE NOT has_static_edge;

-- Velocity — per-month symbol touches, distinct commits, and churn direction.
CREATE OR REPLACE VIEW v_velocity AS
SELECT
  date_trunc('month', c.author_ts) AS month,
  count(*) AS touches,
  count(DISTINCT c.sha) AS commits,
  sum(CASE WHEN t.change_kind = 'added'   THEN 1 ELSE 0 END) AS added,
  sum(CASE WHEN t.change_kind = 'deleted' THEN 1 ELSE 0 END) AS deleted
FROM temporal_edges t
JOIN commits c ON c.sha = t.source_commit
GROUP BY date_trunc('month', c.author_ts);

-- Symbol age / stability — born, last_seen, lifespan.
--
-- Keyed on STRUCTURAL identity (file, entity, kind, scope), NOT raw
-- stable_symbol_id: the latter embeds a byte offset, so a symbol that merely
-- shifts position mints a fresh id, yielding hundreds of thousands of phantom
-- "symbols" with a bogus ~1.6-day lifespan. This mirrors the reconciliation in
-- v_symbol_churn_90d and keeps only unambiguously-identified symbols.
CREATE OR REPLACE VIEW v_symbol_age AS
WITH structural_unique AS (
  SELECT file_path, entity_name, symbol_kind, enclosing_scope,
         MIN(stable_symbol_id) AS stable_symbol_id
  FROM nodes
  WHERE file_path IS NOT NULL AND entity_name IS NOT NULL AND symbol_kind IS NOT NULL
  GROUP BY file_path, entity_name, symbol_kind, enclosing_scope
  HAVING COUNT(*) = 1 AND COUNT(DISTINCT stable_symbol_id) = 1
),
snap AS (
  SELECT s.file_path, s.entity_name, s.symbol_kind, s.enclosing_scope,
         min(c.author_ts) AS born,
         max(c.author_ts) AS last_seen,
         count(DISTINCT s.commit_sha) AS history_commits
  FROM symbol_snapshots s
  JOIN commits c ON c.sha = s.commit_sha
  WHERE s.entity_name IS NOT NULL AND s.symbol_kind IS NOT NULL
  GROUP BY s.file_path, s.entity_name, s.symbol_kind, s.enclosing_scope
)
SELECT
  su.stable_symbol_id,
  sn.born,
  sn.last_seen,
  date_diff('day', sn.born, sn.last_seen) AS lifespan_days,
  sn.history_commits
FROM structural_unique su
JOIN snap sn
  ON sn.file_path = su.file_path
 AND sn.entity_name = su.entity_name
 AND sn.symbol_kind = su.symbol_kind
 AND sn.enclosing_scope IS NOT DISTINCT FROM su.enclosing_scope;

-- Symbol genealogy — rename trails from the temporal walk.
CREATE OR REPLACE VIEW v_symbol_genealogy AS
SELECT
  t.target_stable_symbol_id AS stable_symbol_id,
  t.source_commit AS commit_sha,
  t.change_kind,
  t.rename_prev_stable_symbol_id,
  t.rename_prev_commit
FROM temporal_edges t
WHERE t.change_kind IN ('renamed_from_symbol', 'renamed_from_file');

-- Resolver blind spots — unresolved call labels ranked by call-site count.
CREATE OR REPLACE VIEW v_unresolved_hotspots AS
SELECT target_label, edge_kind, count(*) AS sites
FROM edges_unresolved
WHERE target_label IS NOT NULL
GROUP BY target_label, edge_kind;

-- Risk board — centrality × recent churn, bucketed into a posture.
CREATE OR REPLACE VIEW v_symbol_risk AS
SELECT
  n.stable_symbol_id,
  n.entity_name,
  n.symbol_kind,
  n.file_path,
  COALESCE(ct.pagerank, 0.0) AS pagerank,
  COALESCE(ct.in_degree, 0)  AS in_degree,
  COALESCE(ct.out_degree, 0) AS out_degree,
  COALESCE(ch.events, 0) AS churn_90d,
  ch.last_touched,
  CASE
    WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
    WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
    WHEN ch.events >= 10 THEN 'hot-central'
    ELSE 'active'
  END AS posture
FROM nodes n
LEFT JOIN v_symbol_centrality ct USING (stable_symbol_id)
LEFT JOIN v_symbol_churn_90d  ch USING (stable_symbol_id);

-- MASTER one-stop row — every signal per live symbol, pre-joined.
-- After this view exists, the mining queries collapse to single WHERE/ORDER BY
-- clauses against v_symbol_scorecard.
CREATE OR REPLACE VIEW v_symbol_scorecard AS
SELECT
  n.stable_symbol_id,
  n.entity_name,
  n.qualified_name,
  n.symbol_kind,
  n.file_path,
  COALESCE(ct.pagerank, 0.0) AS pagerank,
  COALESCE(ct.in_degree, 0)  AS in_degree,
  COALESCE(ct.out_degree, 0) AS out_degree,
  cmp.component_id,
  cmp.component_size,
  comm.community_id,
  COALESCE(ib.callers, 0)       AS callers,
  COALESCE(ib.importers, 0)     AS importers,
  COALESCE(ib.inbound_total, 0) AS inbound_total,
  COALESCE(ch.events, 0) AS churn_90d,
  ch.last_touched,
  age.born,
  age.last_seen,
  age.lifespan_days,
  br.blast_radius_score,
  CASE
    WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
    WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
    WHEN ch.events >= 10 THEN 'hot-central'
    ELSE 'active'
  END AS posture
FROM nodes n
LEFT JOIN v_symbol_centrality ct  USING (stable_symbol_id)
LEFT JOIN v_symbol_component  cmp USING (stable_symbol_id)
LEFT JOIN v_symbol_community  comm USING (stable_symbol_id)
LEFT JOIN v_symbol_inbound    ib  USING (stable_symbol_id)
LEFT JOIN v_symbol_churn_90d  ch  USING (stable_symbol_id)
LEFT JOIN v_symbol_age        age USING (stable_symbol_id)
LEFT JOIN v_blast_radius      br  USING (stable_symbol_id);

-- Self-describing catalog — the discoverable surface (SELECT * FROM v_catalog).
CREATE OR REPLACE VIEW v_catalog AS
SELECT * FROM (VALUES
  ('v_symbol_scorecard',   'symbol',  'master per-symbol row: centrality+churn+age+inbound+component+posture'),
  ('v_symbol_risk',        'symbol',  'centrality x churn posture (leaf / load-bearing wall / hot-central)'),
  ('v_symbol_centrality',  'symbol',  'PageRank + in/out degree (materialized via Onager)'),
  ('v_symbol_component',   'symbol',  'weakly-connected component id + size (connectivity islands)'),
  ('v_symbol_community',   'symbol',  'Louvain community id (de-facto modules)'),
  ('v_symbol_age',         'symbol',  'born / last_seen / lifespan (structural-identity keyed)'),
  ('v_symbol_genealogy',   'symbol',  'rename trails (renamed_from_symbol / renamed_from_file)'),
  ('v_symbol_churn_90d',   'symbol',  '90-day per-symbol churn (events/added/modified/deleted)'),
  ('v_symbol_inbound',     'symbol',  'inbound callers / importers / containers'),
  ('v_blast_radius',       'symbol',  'refactor-risk score ln(callers)*ln(caller_churn)+ln(self_churn)'),
  ('v_hidden_coupling',    'file',    'co-change pairs with NO static edge (logical dependencies)'),
  ('v_file_cochange',      'file',    '90-day file co-change pairs (+has_static_edge flag)'),
  ('v_fix_hotspots',       'file',    'per-file fix-commit count + fix-rate (defect density)'),
  ('v_commit_classified',  'commit',  'conventional-commit type per commit'),
  ('v_velocity',           'temporal','per-month touches / commits / added / deleted'),
  ('v_unresolved_hotspots','edge',    'unresolved call labels by site count (resolver gaps)'),
  ('v_graph_metrics',      'graph',   'one-row whole-graph metrics (density, components, communities)'),
  -- search appliance (init_search.sql; present when the Lance section store is attached)
  ('search',               'macro',   'SELECT * FROM search(''q'') — fused doc+code BM25 with high-value signal'),
  ('search_docs',          'macro',   'SELECT * FROM search_docs(''q'') — BM25 over section bodies'),
  ('search_code',          'macro',   'SELECT * FROM search_code(''q'') — BM25 over symbol tokens, fused with scorecard'),
  ('sections',             'doc',     'full section forest (every copy) — backs v_doc_tree'),
  ('sections_search',      'doc',     'deduped prose corpus + FTS (backs search_docs/search)'),
  ('symbol_text',          'symbol',  'per-symbol identifier text, deduped + FTS (code corpus)'),
  ('v_doc_tree',           'doc',     'section heading tree with depth (PageIndex navigation substrate)')
) AS t(view_name, grain, purpose);
