-- Structural fallback for analyst builds without optional temporal shards.
--
-- Keeps search and graph-risk surfaces available without pretending temporal
-- history exists. Temporal metrics are typed as NULL/0 so downstream callers can
-- use the same view shape regardless of whether history was built.

CREATE OR REPLACE VIEW v_search_symbol_tokens AS
SELECT
  stable_symbol_id,
  regexp_extract_all(COALESCE(qualified_name, entity_name, ''), '[A-Za-z_][A-Za-z0-9_]*') AS tokens
FROM nodes
WHERE stable_symbol_id IS NOT NULL;

CREATE OR REPLACE VIEW v_symbol_inbound AS
SELECT
  target_stable_id AS stable_symbol_id,
  sum(CASE WHEN edge_kind IN ('calls', 'calls_dyn') THEN 1 ELSE 0 END) AS callers,
  sum(CASE WHEN edge_kind = 'references_other' AND relation = 'imports' THEN 1 ELSE 0 END) AS importers,
  sum(CASE WHEN edge_kind = 'references_other' AND relation = 'contains' THEN 1 ELSE 0 END) AS containers,
  count(*) AS inbound_total
FROM edges
GROUP BY target_stable_id;

CREATE OR REPLACE VIEW v_symbol_risk AS
SELECT
  n.stable_symbol_id,
  n.entity_name,
  n.symbol_kind,
  n.file_path,
  COALESCE(ct.pagerank, 0.0) AS pagerank,
  COALESCE(ct.in_degree, 0) AS in_degree,
  COALESCE(ct.out_degree, 0) AS out_degree,
  0::BIGINT AS churn_90d,
  CAST(NULL AS TIMESTAMP) AS last_touched,
  CASE
    WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
    ELSE 'load-bearing wall'
  END AS posture
FROM nodes n
LEFT JOIN v_symbol_centrality ct USING (stable_symbol_id);

CREATE OR REPLACE VIEW v_symbol_scorecard AS
SELECT
  n.stable_symbol_id,
  n.entity_name,
  n.qualified_name,
  n.symbol_kind,
  n.file_path,
  COALESCE(ct.pagerank, 0.0) AS pagerank,
  COALESCE(ct.in_degree, 0) AS in_degree,
  COALESCE(ct.out_degree, 0) AS out_degree,
  cmp.component_id,
  cmp.component_size,
  comm.community_id,
  COALESCE(ib.callers, 0) AS callers,
  COALESCE(ib.importers, 0) AS importers,
  COALESCE(ib.inbound_total, 0) AS inbound_total,
  0::BIGINT AS churn_90d,
  CAST(NULL AS TIMESTAMP) AS last_touched,
  CAST(NULL AS TIMESTAMP) AS born,
  CAST(NULL AS TIMESTAMP) AS last_seen,
  CAST(NULL AS BIGINT) AS lifespan_days,
  CAST(NULL AS DOUBLE) AS blast_radius_score,
  CASE
    WHEN COALESCE(ct.in_degree, 0) = 0 THEN 'leaf'
    ELSE 'load-bearing wall'
  END AS posture
FROM nodes n
LEFT JOIN v_symbol_centrality ct USING (stable_symbol_id)
LEFT JOIN v_symbol_component cmp USING (stable_symbol_id)
LEFT JOIN v_symbol_community comm USING (stable_symbol_id)
LEFT JOIN v_symbol_inbound ib USING (stable_symbol_id);

CREATE OR REPLACE VIEW v_catalog AS
SELECT * FROM (VALUES
  ('v_symbol_scorecard',   'symbol', 'structural per-symbol row: centrality+inbound+component+posture'),
  ('v_symbol_risk',        'symbol', 'structural centrality posture (leaf / load-bearing wall)'),
  ('v_symbol_centrality',  'symbol', 'PageRank + in/out degree (materialized via Onager)'),
  ('v_symbol_component',   'symbol', 'weakly-connected component id + size (connectivity islands)'),
  ('v_symbol_community',   'symbol', 'Louvain community id (de-facto modules)'),
  ('v_symbol_inbound',     'symbol', 'inbound callers / importers / containers'),
  ('search',               'macro',  'SELECT * FROM search(''q'') - fused doc+code BM25 with high-value signal'),
  ('search_docs',          'macro',  'SELECT * FROM search_docs(''q'') - BM25 over section bodies'),
  ('search_code',          'macro',  'SELECT * FROM search_code(''q'') - BM25 over symbol text, fused with scorecard'),
  ('sections',             'doc',    'full section forest (every copy) - backs v_doc_tree'),
  ('sections_search',      'doc',    'deduped prose corpus + FTS (backs search_docs/search)'),
  ('symbol_text',          'symbol', 'per-symbol identifier text, deduped + FTS (code corpus)'),
  ('v_doc_tree',           'doc',    'section heading tree with depth (PageIndex navigation substrate)')
) AS t(view_name, grain, purpose);
