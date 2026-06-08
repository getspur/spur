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
-- Lance hybrid search is opportunistic. The Rust query layer retries BM25-only
-- if this extension is unavailable in a read connection.
INSTALL lance; LOAD lance;

-- ── Prose corpus: section bodies materialized from Lance + persistent FTS ─────
CREATE OR REPLACE TABLE sections AS
SELECT stable_symbol_id, parent_stable_id, qualified_name, file_path,
       heading_level, child_count, content_hash, body_byte_start, body_text
FROM lance_ns.section_bodies
WHERE body_text IS NOT NULL AND length(body_text) > 0;

-- Deduped search corpus. Skills are installed into ~6 agent dirs
-- (.claude/.codex/.kiro/.gemini/.kimi/.opencode), so a skill's sections appear
-- 6-10x by identical body — except for the SPUR-MANAGED header injected per
-- vendored copy. FTS over the raw table returns the same section many times.
-- Keep `sections` FULL (v_doc_tree needs the whole forest), but index a deduped
-- copy: one row per distinct SECTION (same heading level + normalized body),
-- preferring the canonical (non-dot-dir) path.
--
-- Dedup on the SECTION BODY, NOT `content_hash`: content_hash is the *whole-file*
-- document hash (one value per file, shared by every section in it), so
-- partitioning by it would keep only ONE section per document — collapsing 19.7k
-- rows to ~990 and destroying ~17k distinct bodies. Genuine cross-copy
-- duplication is only ~8% (19.7k -> ~18.2k); the rest is unique prose.
--
-- Do NOT partition by `qualified_name` for markdown sections: Lance populates it
-- from the file path for top-level skill files, so each vendored agent directory
-- gets a different value and duplicate skill bodies never group.
CREATE OR REPLACE TABLE sections_search AS
SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
FROM sections
QUALIFY row_number() OVER (
  PARTITION BY heading_level,
               regexp_replace(COALESCE(body_text, ''), '<!-- SPUR-MANAGED[^>]*-->\n?', '')
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
-- Rust hybrid search uses the stable ID column for Lance ANN fusion; the public
-- search_docs macro preserves the old projection shape for callers.
CREATE OR REPLACE MACRO search_docs_bm25(q) AS TABLE
  SELECT s.qualified_name AS section, s.file_path, s.heading_level,
         s.stable_symbol_id,
         round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS bm25
  FROM sections_search s
  WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
  QUALIFY row_number() OVER (PARTITION BY s.file_path ORDER BY bm25 DESC) <= 2
  ORDER BY bm25 DESC NULLS LAST
  LIMIT 25;

CREATE OR REPLACE MACRO search_docs(q) AS TABLE
  SELECT section, file_path, heading_level, bm25
  FROM search_docs_bm25(q);

-- Code-only: BM25 over symbol token text, FUSED with the scorecard so each hit
-- carries its centrality / churn / posture / component — high-value by default.
CREATE OR REPLACE MACRO search_code(q) AS TABLE
  SELECT symbol, symbol_kind, file_path,
         round(bm25_raw, 3) AS bm25,
         round(pagerank * 1e4, 2) AS pagerank_x1e4,
         churn_90d, posture, component_size
  FROM (
    SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
           fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
           sc.pagerank, sc.churn_90d, sc.posture, sc.component_size
    FROM symbol_text st
    JOIN v_symbol_scorecard sc USING (stable_symbol_id)
    WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  )
  ORDER BY bm25_raw
    * CASE WHEN file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
    * CASE WHEN symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
           WHEN symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
    * (1 + 0.15 * ln(1 + pagerank * 1e4)) DESC NULLS LAST
  LIMIT 25;

-- Unified: docs + code in one ranked result set with high-value detail inline.
CREATE OR REPLACE MACRO search(q) AS TABLE
  SELECT kind, title, file, score, signal FROM (
    SELECT 'doc' AS kind,
           s.qualified_name AS title,
           regexp_replace(s.file_path, '^(crates|docs|\.claude|\.spur|\.codex|\.kiro|\.gemini)/', '') AS file,
           round(fts_main_sections_search.match_bm25(s.stable_symbol_id, q), 3) AS score,
           CAST(NULL AS VARCHAR) AS signal,
           fts_main_sections_search.match_bm25(s.stable_symbol_id, q) AS rank
    FROM sections_search s
    WHERE fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
    UNION ALL
    SELECT 'code',
           st.entity_name,
           regexp_replace(st.file_path, '^crates/', ''),
           round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3),
           sc.posture || ' · pr=' || round(sc.pagerank * 1e4, 1) || ' · churn=' || sc.churn_90d,
           fts_main_symbol_text.match_bm25(st.stable_symbol_id, q)
             * CASE WHEN st.file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
             * CASE WHEN st.symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                    WHEN st.symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
             * (1 + 0.15 * ln(1 + sc.pagerank * 1e4))
    FROM symbol_text st
    JOIN v_symbol_scorecard sc USING (stable_symbol_id)
    WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
  )
  WHERE score IS NOT NULL
  QUALIFY row_number() OVER (PARTITION BY file ORDER BY rank DESC) <= 2
  ORDER BY rank DESC NULLS LAST
  LIMIT 30;

-- Stable-ID-preserving context candidates for one-shot Knowledge Context packs.
-- Unlike the human-facing search/search_graph macros above, this keeps raw
-- file_path and stable_symbol_id so Rust callers can ground exact symbols/docs.
CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE
  SELECT kind, title, file_path, stable_symbol_id, symbol_kind,
         round(rank, 3) AS score, signal, neighbor_kind, edge_bind_method, grounding
  FROM (
    SELECT *,
           raw_rank
             * CASE
                 WHEN intent = 'plan' AND kind = 'doc' THEN 1.3
                 WHEN intent = 'debug' AND kind = 'code' THEN 1 + 0.12 * ln(1 + churn_90d)
                 WHEN intent = 'change' AND kind = 'code' THEN 1 + 0.10 * ln(1 + caller_count)
                 WHEN intent = 'review' AND kind = 'code' AND posture = 'load-bearing wall' THEN 1.35
                 ELSE 1.0
               END AS rank
    FROM (
      SELECT 'doc' AS kind,
             s.qualified_name AS title,
             s.file_path,
             s.stable_symbol_id,
             CAST('section' AS VARCHAR) AS symbol_kind,
             CAST(NULL AS VARCHAR) AS signal,
             CAST(NULL AS VARCHAR) AS neighbor_kind,
             CAST(NULL AS VARCHAR) AS edge_bind_method,
             'bm25-doc' AS grounding,
             fts_main_sections_search.match_bm25(s.stable_symbol_id, q) AS raw_rank,
             0::BIGINT AS churn_90d,
             0::BIGINT AS caller_count,
             CAST(NULL AS VARCHAR) AS posture
      FROM sections_search s
      WHERE requested_scope IN ('all', 'docs')
        AND fts_main_sections_search.match_bm25(s.stable_symbol_id, q) IS NOT NULL
      UNION ALL
      SELECT 'code' AS kind,
             st.entity_name AS title,
             st.file_path,
             st.stable_symbol_id,
             st.symbol_kind,
             sc.posture || ' · pr=' || round(sc.pagerank * 1e4, 1) || ' · churn=' || sc.churn_90d AS signal,
             'primary' AS neighbor_kind,
             CAST(NULL AS VARCHAR) AS edge_bind_method,
             'bm25-code' AS grounding,
             fts_main_symbol_text.match_bm25(st.stable_symbol_id, q)
               * CASE WHEN st.file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
               * CASE WHEN st.symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                      WHEN st.symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
               * (1 + 0.15 * ln(1 + sc.pagerank * 1e4)) AS raw_rank,
             sc.churn_90d,
             sc.callers AS caller_count,
             sc.posture
      FROM symbol_text st
      JOIN v_symbol_scorecard sc USING (stable_symbol_id)
      WHERE requested_scope IN ('all', 'code', 'graph')
        AND fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
    )
  )
  WHERE rank IS NOT NULL
  QUALIFY row_number() OVER (PARTITION BY file_path ORDER BY rank DESC) <= 2
  ORDER BY rank DESC NULLS LAST
  LIMIT 40;

CREATE OR REPLACE MACRO search_context_candidates_hybrid(q, requested_scope, intent, query_vec) AS TABLE
  SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, signal,
         neighbor_kind, edge_bind_method, grounding
  FROM (
    WITH bm25_rows AS (
      SELECT * FROM search_context_candidates(q, requested_scope, intent)
    ),
    hybrid_code AS (
      SELECT
        'code' AS kind,
        COALESCE(sc.entity_name, h.entity_name) AS title,
        COALESCE(sc.file_path, h.file_path) AS file_path,
        h.stable_symbol_id,
        COALESCE(sc.symbol_kind, h.symbol_kind) AS symbol_kind,
        round(
          COALESCE(h._hybrid_score, h._score, 1.0 - h._distance)
            * CASE WHEN COALESCE(sc.file_path, h.file_path) LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
            * CASE WHEN COALESCE(sc.symbol_kind, h.symbol_kind) IN ('function','method','struct','enum','trait') THEN 1.15
                   WHEN COALESCE(sc.symbol_kind, h.symbol_kind) IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
            * (1 + 0.15 * ln(1 + COALESCE(sc.pagerank, 0) * 1e4))
            * CASE
                WHEN intent = 'debug' THEN 1 + 0.12 * ln(1 + COALESCE(sc.churn_90d, 0))
                WHEN intent = 'change' THEN 1 + 0.10 * ln(1 + COALESCE(sc.callers, 0))
                WHEN intent = 'review' AND COALESCE(sc.posture, '') = 'load-bearing wall' THEN 1.35
                ELSE 1.0
              END,
          3
        ) AS score,
        COALESCE(sc.posture, 'unknown') || ' · pr=' || round(COALESCE(sc.pagerank, 0) * 1e4, 1)
          || ' · churn=' || COALESCE(sc.churn_90d, 0) AS signal,
        'primary' AS neighbor_kind,
        CAST(NULL AS VARCHAR) AS edge_bind_method,
        'hybrid-code' AS grounding
      FROM lance_hybrid_search(
        '__SPUR_GRAPH_ARTIFACT_DIR__/code_symbols.lance/code_symbols',
        'vector', query_vec, 'embed_text', q, 30, 0.5, 5
      ) h
      JOIN v_symbol_scorecard sc USING (stable_symbol_id)
      WHERE requested_scope IN ('all', 'code', 'graph')
    ),
    hybrid_docs AS (
      SELECT
        'doc' AS kind,
        s.qualified_name AS title,
        s.file_path,
        h.stable_symbol_id,
        CAST('section' AS VARCHAR) AS symbol_kind,
        round(
          COALESCE(h._hybrid_score, h._score, 1.0 - h._distance)
            * CASE WHEN intent = 'plan' THEN 1.3 ELSE 1.0 END,
          3
        ) AS score,
        CAST(NULL AS VARCHAR) AS signal,
        CAST(NULL AS VARCHAR) AS neighbor_kind,
        CAST(NULL AS VARCHAR) AS edge_bind_method,
        'hybrid-doc' AS grounding
      FROM lance_hybrid_search(
        'lance_ns.main.section_bodies',
        'vector', query_vec, 'body_text', q, 30, 0.5, 5
      ) h
      JOIN sections_search s USING (stable_symbol_id)
      WHERE requested_scope IN ('all', 'docs')
    ),
    unioned AS (
      SELECT * FROM bm25_rows WHERE query_vec IS NULL
      UNION ALL
      SELECT * FROM bm25_rows WHERE query_vec IS NOT NULL
      UNION ALL
      SELECT * FROM hybrid_code WHERE query_vec IS NOT NULL
      UNION ALL
      SELECT * FROM hybrid_docs WHERE query_vec IS NOT NULL
    )
    SELECT *,
           row_number() OVER (
             PARTITION BY COALESCE(stable_symbol_id, file_path || ':' || title)
             ORDER BY score DESC NULLS LAST
           ) AS dedupe_rank
    FROM unioned
  )
  WHERE dedupe_rank = 1
  ORDER BY score DESC NULLS LAST
  LIMIT 40;

-- Graph-augmented: BM25 top-k hits + selective 1-hop call-graph expansion.
-- Gate: symbols with posture = 'load-bearing wall' AND callers > 30 are popular
-- sinks — expanding them would flood results with noise. All other hits expand.
CREATE OR REPLACE MACRO search_graph(q, intent) AS TABLE
  SELECT kind, title, file_path, stable_symbol_id, symbol_kind, score, signal,
         neighbor_kind, edge_bind_method, grounding
  FROM (
    WITH base AS (
      SELECT
        st.stable_symbol_id,
        st.entity_name AS symbol,
        st.symbol_kind,
        st.file_path,
        fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
        fts_main_symbol_text.match_bm25(st.stable_symbol_id, q)
          * CASE WHEN st.file_path LIKE '%/tests/%' THEN 0.6 ELSE 1.0 END
          * CASE WHEN st.symbol_kind IN ('function','method','struct','enum','trait') THEN 1.15
                 WHEN st.symbol_kind IN ('constant','static','field') THEN 0.85 ELSE 1.0 END
          * (1 + 0.15 * ln(1 + sc.pagerank * 1e4))
          * CASE
              WHEN intent = 'debug' THEN 1 + 0.12 * ln(1 + sc.churn_90d)
              WHEN intent = 'change' THEN 1 + 0.10 * ln(1 + COALESCE(vi.callers, 0))
              WHEN intent = 'review' AND sc.posture = 'load-bearing wall' THEN 1.35
              ELSE 1.0
            END AS fused_rank,
        sc.pagerank,
        sc.churn_90d,
        sc.posture,
        sc.component_size,
        COALESCE(vi.callers, 0) AS caller_count
      FROM symbol_text st
      JOIN v_symbol_scorecard sc USING (stable_symbol_id)
      LEFT JOIN v_symbol_inbound vi USING (stable_symbol_id)
      WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
      ORDER BY fused_rank DESC NULLS LAST
      LIMIT 5
    ),
    -- Selective gate: expand only non-sink symbols.
    gated AS (
      SELECT * FROM base
      WHERE posture != 'load-bearing wall' OR caller_count <= 30
    ),
    primary_rows AS (
      SELECT
        'code' AS kind,
        symbol AS title,
        file_path,
        stable_symbol_id,
        symbol_kind,
        round(fused_rank, 3) AS score,
        posture || ' · pr=' || round(pagerank * 1e4, 1) || ' · churn=' || churn_90d AS signal,
        'primary' AS neighbor_kind,
        CAST(NULL AS VARCHAR) AS edge_bind_method,
        'bm25-graph' AS grounding
      FROM base
    ),
    neighbor_rows AS (
      SELECT *
      FROM (
        SELECT
          'code' AS kind,
          nsrc.entity_name AS title,
          nsrc.file_path,
          nsrc.stable_symbol_id,
          nsrc.symbol_kind,
          round(COALESCE(sc2.pagerank, 0) * 1e4, 3) AS score,
          COALESCE(sc2.posture, 'unknown') || ' · caller of ' || g.symbol AS signal,
          'caller' AS neighbor_kind,
          e.bind_method AS edge_bind_method,
          'graph-expanded' AS grounding
        FROM gated g
        LEFT JOIN edges e
          ON e.target_stable_id = g.stable_symbol_id AND e.relation = 'calls'
        LEFT JOIN nodes nsrc
          ON nsrc.stable_symbol_id = e.source_stable_id
        LEFT JOIN v_symbol_scorecard sc2
          ON sc2.stable_symbol_id = nsrc.stable_symbol_id
        WHERE nsrc.file_path NOT LIKE '.%'
          AND nsrc.file_path NOT LIKE '%/tests/%'
        UNION ALL
        SELECT
          'code' AS kind,
          ndst.entity_name AS title,
          ndst.file_path,
          ndst.stable_symbol_id,
          ndst.symbol_kind,
          round(COALESCE(sc3.pagerank, 0) * 1e4, 3) AS score,
          COALESCE(sc3.posture, 'unknown') || ' · callee of ' || g.symbol AS signal,
          'callee' AS neighbor_kind,
          e.bind_method AS edge_bind_method,
          'graph-expanded' AS grounding
        FROM gated g
        LEFT JOIN edges e
          ON e.source_stable_id = g.stable_symbol_id AND e.relation = 'calls'
        LEFT JOIN nodes ndst
          ON ndst.stable_symbol_id = e.target_stable_id
        LEFT JOIN v_symbol_scorecard sc3
          ON sc3.stable_symbol_id = ndst.stable_symbol_id
        WHERE ndst.file_path NOT LIKE '.%'
          AND ndst.file_path NOT LIKE '%/tests/%'
      )
      QUALIFY row_number() OVER (PARTITION BY file_path, stable_symbol_id ORDER BY score DESC) <= 2
    )
    SELECT * FROM primary_rows
    UNION ALL
    SELECT * FROM neighbor_rows
  )
  ORDER BY
    CASE neighbor_kind WHEN 'primary' THEN 0 WHEN 'caller' THEN 1 ELSE 2 END,
    score DESC NULLS LAST
  LIMIT 40;
