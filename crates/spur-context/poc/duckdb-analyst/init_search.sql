-- SPUR analyst — materialized free-text SEARCH APPLIANCE.
--
-- Persists FTS indexes over both corpora and exposes fused search() macros so an
-- agent answers a question with a single  SELECT * FROM search('...')  — every
-- expensive thing (centrality, churn, risk, components, full-text indexes) is
-- precomputed at build time; query time is just a ranked lookup + join.
--
-- Ordering contract:
--   * runs AFTER init_views.sql        (search_code joins v_symbol_scorecard)
--   * requires lance_ns attached       (sections come from section_bodies)
-- analyst.rs gates inclusion on (temporal AND lance) presence.

INSTALL fts; LOAD fts;

-- ── Prose corpus: section bodies materialized from Lance + persistent FTS ─────
CREATE OR REPLACE TABLE sections AS
SELECT stable_symbol_id, parent_stable_id, qualified_name, file_path,
       heading_level, child_count, content_hash, body_byte_start, body_text
FROM lance_ns.section_bodies
WHERE body_text IS NOT NULL AND length(body_text) > 0;

-- Deduped search corpus. Skills are installed into ~6 agent dirs
-- (.claude/.codex/.kiro/.gemini/.kimi/.opencode), so the raw `sections` table is
-- ~95% duplicate by body — FTS over it would return the same skill section many
-- times. Keep `sections` FULL (v_doc_tree needs the whole forest), but index a
-- deduped copy: one row per distinct body, preferring the canonical (non-dot-dir)
-- path. Empty-body sections collapse to one row here but never match FTS anyway.
CREATE OR REPLACE TABLE sections_search AS
SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
FROM sections
QUALIFY row_number() OVER (
  PARTITION BY content_hash
  ORDER BY (file_path LIKE '.%')::INT, length(file_path), file_path
) = 1;

PRAGMA create_fts_index('sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');

-- ── Code corpus: per-(live)symbol identifier text + persistent FTS ───────────
-- Aggregates distinct tokens across a symbol's snapshots (restricted to live
-- nodes), prepended with its qualified_name so name terms are searchable too.
CREATE OR REPLACE TABLE symbol_text AS
WITH toks AS (
  SELECT s.stable_symbol_id, string_agg(DISTINCT tok, ' ') AS token_text
  FROM symbol_snapshots s, UNNEST(s.tokens) AS u(tok)
  WHERE s.tokens IS NOT NULL
    AND s.stable_symbol_id IN (SELECT stable_symbol_id FROM nodes)
  GROUP BY s.stable_symbol_id
)
SELECT n.stable_symbol_id, n.entity_name, n.qualified_name, n.file_path, n.symbol_kind,
       COALESCE(n.qualified_name, '') || ' ' || COALESCE(t.token_text, '') AS doc_text
FROM nodes n
LEFT JOIN toks t USING (stable_symbol_id)
WHERE n.symbol_kind NOT IN ('section', 'mcp_tool')   -- sections live in the prose corpus
-- Dedup symbols copied into agent dirs (the skill installer drops .rs files into
-- .claude/.codex/...) by identical content, preferring the canonical path.
QUALIFY row_number() OVER (
  PARTITION BY COALESCE(n.qualified_name, '') || ' ' || COALESCE(t.token_text, '')
  ORDER BY (n.file_path LIKE '.%')::INT, length(n.file_path), n.file_path
) = 1;

PRAGMA create_fts_index('symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);

-- ── Document tree (PageIndex substrate): parent walk with depth ──────────────
CREATE OR REPLACE VIEW v_doc_tree AS
WITH RECURSIVE walk AS (
  SELECT stable_symbol_id, parent_stable_id, qualified_name, file_path,
         heading_level, child_count, 0 AS depth
  FROM sections WHERE parent_stable_id IS NULL
  UNION ALL
  SELECT s.stable_symbol_id, s.parent_stable_id, s.qualified_name, s.file_path,
         s.heading_level, s.child_count, w.depth + 1
  FROM sections s JOIN walk w ON s.parent_stable_id = w.stable_symbol_id
)
SELECT * FROM walk;

-- ── Search macros — one call, ranked + fused with high-value graph signals ───

-- Prose-only: BM25 over documentation/skill/plan section bodies.
CREATE OR REPLACE MACRO search_docs(q) AS TABLE
  SELECT s.qualified_name AS section, s.file_path, s.heading_level,
         round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS bm25
  FROM sections_search s
  WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
  ORDER BY bm25 DESC NULLS LAST
  LIMIT 25;

-- Code-only: BM25 over symbol token text, FUSED with the scorecard so each hit
-- carries its centrality / churn / posture / component — high-value by default.
CREATE OR REPLACE MACRO search_code(q) AS TABLE
  SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
         round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3) AS bm25,
         round(sc.pagerank * 1e4, 2) AS pagerank_x1e4,
         sc.churn_90d, sc.posture, sc.component_size
  FROM symbol_text st
  JOIN v_symbol_scorecard sc USING (stable_symbol_id)
  WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  ORDER BY bm25 DESC NULLS LAST
  LIMIT 25;

-- Unified: docs + code in one ranked result set with high-value detail inline.
CREATE OR REPLACE MACRO search(q) AS TABLE
  SELECT * FROM (
    SELECT 'doc' AS kind,
           s.qualified_name AS title,
           regexp_replace(s.file_path, '^(crates|docs|\.claude|\.spur|\.codex|\.kiro|\.gemini)/', '') AS file,
           round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS score,
           CAST(NULL AS VARCHAR) AS signal
    FROM sections_search s
    WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
    UNION ALL
    SELECT 'code',
           st.entity_name,
           regexp_replace(st.file_path, '^crates/', ''),
           round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3),
           sc.posture || ' · pr=' || round(sc.pagerank * 1e4, 1) || ' · churn=' || sc.churn_90d
    FROM symbol_text st
    JOIN v_symbol_scorecard sc USING (stable_symbol_id)
    WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  )
  WHERE score IS NOT NULL
  ORDER BY score DESC
  LIMIT 30;
