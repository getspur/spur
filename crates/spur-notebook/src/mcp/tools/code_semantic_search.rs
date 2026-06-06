//! `code_semantic_search` — BM25 retrieval over the analyst DuckDB's doc + code
//! corpora (`search()` / `search_docs()` / `search_code()` macros).
//!
//! Complements `code_symbol_search` (spur-mcp): that tool matches symbol *names*
//! lexically; this one ranks *content* (section bodies + symbol token text) by
//! relevance, and — for code hits — fuses the scorecard signal (centrality /
//! churn / posture). Reads `.spur/analyst.duckdb` read-only via the bundled
//! libduckdb. Two bundled extensions are needed at query time: `fts` (autoloads
//! for `match_bm25`) and `icu`. The temporal views behind the code scorecard
//! (`v_symbol_churn_90d` → `posture`) filter on `now() - INTERVAL '90 day'`
//! against a TIMESTAMP WITH TIME ZONE column; that `-(TIMESTAMPTZ, INTERVAL)`
//! overload lives in `icu`, NOT core, and is not autoloaded — so the `code`/`all`
//! scopes fail to bind read-only ("No function matches ... -(TIMESTAMP WITH TIME
//! ZONE, INTERVAL)") unless we `LOAD icu` first. icu is statically linked into
//! libduckdb-sys, so a plain `LOAD` (no network install) suffices. Other
//! community extensions used to BUILD the DB are irrelevant to the read path.

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

const METHOD: &str = "code_semantic_search";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
struct Params {
    query: String,
    /// "all" (docs + code), "docs", "code", or "graph". Defaults to "all".
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Optional explicit path to analyst.duckdb; otherwise discovered from cwd.
    #[serde(default)]
    db_path: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Semantic (BM25) search over the analyst index: ranks documentation/skill/plan section bodies AND code (symbol token text) by relevance to a natural-language query. Code hits carry their scorecard signal (pagerank/churn/posture). Use this for concept/content questions ('how does oauth refresh work', 'where is the review gate documented'); use code_symbol_search for exact symbol-name lookup and code_callers/code_callees for the call graph.",
        rmcp_object(json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "scope": { "type": "string", "enum": ["all", "docs", "code", "graph"], "default": "all" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT },
                "db_path": { "type": "string", "description": "Optional path to analyst.duckdb; discovered from cwd upward by default." }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    _deps: &crate::mcp::ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: Params = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ query, scope?, limit?, db_path? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.query.trim().is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} query must be non-empty"),
            None,
        ));
    }
    let scope = params.scope.as_deref().unwrap_or("all");
    if !matches!(scope, "all" | "docs" | "code" | "graph") {
        return Err(McpError::invalid_params(
            format!("{METHOD} scope must be one of all|docs|code|graph"),
            Some(json!({ "scope": scope })),
        ));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    run(&params.query, scope, limit, params.db_path.as_deref())
}

#[cfg(feature = "datasource-introspect")]
fn run(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    let (path, results, graph_hash) = search_rows(query, scope, limit, db_path)?;
    Ok(CallToolResult::structured(json!({
        "scope": scope,
        "query": query,
        "count": results.len(),
        "results": results,
        "graph_content_hash": graph_hash,
        "db_path": path,
    })))
}

/// Core query: open analyst.duckdb read-only and run the chosen `search*` macro.
/// Returns (resolved_db_path, rows, graph_content_hash).
#[cfg(feature = "datasource-introspect")]
fn search_rows(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<(String, Vec<Value>, Option<String>), McpError> {
    let path = resolve_db_path(db_path)?;

    // Read-only so the tool coexists with the read-only spur-analyst MCP and never
    // takes a write lock on the live DB.
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .map_err(|e| internal("failed to configure read-only duckdb", &e))?;
    let conn = duckdb::Connection::open_with_flags(&path, config).map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not open analyst.duckdb (version skew between writer and bundled reader can cause this)"),
            Some(json!({ "db_path": path.display().to_string(), "error": e.to_string() })),
        )
    })?;

    // The temporal views behind the code scorecard (v_symbol_churn_90d → posture)
    // filter on `now() - INTERVAL '90 day'` over a TIMESTAMPTZ column; that
    // `-(TIMESTAMPTZ, INTERVAL)` overload lives in icu, not core, and is not
    // autoloaded — so binding `search_code`/`search` read-only fails without it.
    // icu is statically bundled, so LOAD (no network install) is enough.
    // Best-effort: the `docs` scope never touches the temporal views, and if icu
    // were genuinely unavailable the `code`/`all` prepare below still surfaces the
    // precise binder error — so a load failure here must not break `docs`.
    let _ = conn.execute_batch("LOAD icu;");

    // The macro arg flows into FTS match_bm25, which wants a constant — inline the
    // query as an escaped string literal rather than a bind parameter.
    let q = query.replace('\'', "''");
    let sql = match scope {
        "docs" => format!(
            "SELECT 'doc' AS kind, section AS title, file_path AS file, \
             round(bm25, 3) AS score, CAST(NULL AS VARCHAR) AS signal \
             FROM search_docs('{q}') LIMIT {limit}"
        ),
        "code" => format!(
            "SELECT 'code' AS kind, symbol AS title, file_path AS file, \
             round(bm25, 3) AS score, posture AS signal \
             FROM search_code('{q}') LIMIT {limit}"
        ),
        "graph" => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal, \
             neighbor_kind, edge_bind_method \
             FROM search_graph('{q}') LIMIT {limit}"
        ),
        _ => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal \
             FROM search('{q}') LIMIT {limit}"
        ),
    };

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal("failed to prepare search query", &e))?;
    let rows = stmt
        .query_map([], |row| {
            let mut obj = json!({
                "kind": row.get::<_, String>(0)?,
                "title": row.get::<_, String>(1)?,
                "file": row.get::<_, String>(2)?,
                "score": row.get::<_, f64>(3)?,
                "signal": row.get::<_, Option<String>>(4)?,
            });
            if scope == "graph" {
                obj["neighbor_kind"] = json!(row.get::<_, Option<String>>(5)?);
                obj["edge_bind_method"] = json!(row.get::<_, Option<String>>(6)?);
            }
            Ok(obj)
        })
        .map_err(|e| internal("failed to run search query", &e))?;
    let results: Vec<Value> = rows
        .collect::<Result<_, _>>()
        .map_err(|e| internal("failed to read search rows", &e))?;

    // Surface the indexed graph hash so callers can detect staleness vs code_* tools.
    let graph_hash: Option<String> = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |r| r.get(0))
        .ok();

    Ok((path.display().to_string(), results, graph_hash))
}

#[cfg(not(feature = "datasource-introspect"))]
fn run(
    _query: &str,
    _scope: &str,
    _limit: usize,
    _db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    Err(McpError::internal_error(
        format!(
            "{METHOD} requires the `datasource-introspect` feature (bundled duckdb) to be enabled"
        ),
        Some(json!({ "code": "duckdb_unavailable" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn resolve_db_path(explicit: Option<&str>) -> Result<std::path::PathBuf, McpError> {
    use std::path::PathBuf;
    if let Some(raw) = explicit {
        let pb = PathBuf::from(raw);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(McpError::invalid_params(
            format!("{METHOD} db_path does not exist: {raw}"),
            Some(json!({ "db_path": raw })),
        ));
    }
    let mut dir = std::env::current_dir().map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not read current directory"),
            Some(json!({ "error": e.to_string() })),
        )
    })?;
    loop {
        let candidate = dir.join(".spur").join("analyst.duckdb");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(McpError::internal_error(
        format!("{METHOD} found no .spur/analyst.duckdb from the working directory upward; run `spur-cli graph build`"),
        Some(json!({ "code": "analyst_db_not_found" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn internal(message: &str, error: &impl std::fmt::Display) -> McpError {
    McpError::internal_error(
        format!("{METHOD} {message}"),
        Some(json!({ "error": error.to_string() })),
    )
}

#[cfg(all(test, feature = "datasource-introspect"))]
mod tests {
    use super::*;

    // Rerank: a high-BM25 leaf CONSTANT must not outrank a lower-BM25, high-pagerank
    // FUNCTION. Mirrors the production search_code ORDER BY against a bundled-duckdb fixture
    // (the WORKTREE_CORE_ORPHAN_CLEANUP-over-cleanup_orphans inversion from the live eval).
    #[test]
    fn search_code_rerank_floats_impl_over_leaf_constant() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("a.duckdb");
        let conn = duckdb::Connection::open(&db)?;
        conn.execute_batch("INSTALL fts; LOAD fts;")?;
        conn.execute_batch(
            r#"
            CREATE TABLE symbol_text(stable_symbol_id VARCHAR, entity_name VARCHAR,
                symbol_kind VARCHAR, file_path VARCHAR, doc_text VARCHAR);
            INSERT INTO symbol_text VALUES
              ('c1','WORKTREE_CORE_ORPHAN_CLEANUP','constant',
               'crates/spur-license/src/policy/feature_key.rs',
               'worktree orphan cleanup worktree orphan cleanup'),
              ('f1','cleanup_orphans','function',
               'crates/spur-worktree/src/manager.rs',
               'worktree orphan cleanup sweep');
            -- scorecard: the function is a load-bearing wall (high pagerank), constant is a leaf.
            CREATE TABLE v_symbol_scorecard(stable_symbol_id VARCHAR, pagerank DOUBLE,
                churn_90d BIGINT, posture VARCHAR, component_size BIGINT);
            INSERT INTO v_symbol_scorecard VALUES
              ('c1', 0.0, 0, 'leaf', 1),
              ('f1', 0.02, 3, 'load-bearing wall', 50);
            "#,
        )?;
        conn.execute_batch(
            "PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text', overwrite=1);",
        )?;
        // The PRODUCTION search_code ORDER BY (kept in sync with init_search.sql).
        conn.execute_batch(
            r#"
            CREATE OR REPLACE MACRO search_code(q) AS TABLE
              SELECT * FROM (
                SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
                       fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                       sc.pagerank
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
            "#,
        )?;
        let first: String = conn.query_row(
            "SELECT symbol FROM search_code('worktree orphan cleanup') LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            first, "cleanup_orphans",
            "the load-bearing function must outrank the leaf constant after rerank"
        );
        Ok(())
    }

    // Diversity + dedup: vendored copies collapse to one, and one document cannot occupy
    // more than 2 of the top result slots.
    #[test]
    fn search_dedups_vendored_copies_and_caps_per_document() -> anyhow::Result<()> {
        const INIT_SEARCH_SQL: &str =
            include_str!("../../../../spur-context/poc/duckdb-analyst/init_search.sql");

        let dir = tempfile::tempdir()?;
        let db = dir.path().join("a.duckdb");
        let conn = duckdb::Connection::open(&db)?;
        conn.execute_batch("INSTALL fts; LOAD fts;")?;
        // sections raw: same skill body vendored 3x (differ only by SPUR-MANAGED header +
        // dot-dir path) plus one 3-section plan document.
        conn.execute_batch(
            r#"
            CREATE SCHEMA lance_ns;
            CREATE TABLE lance_ns.section_bodies(
                stable_symbol_id VARCHAR,
                parent_stable_id VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                heading_level BIGINT,
                child_count BIGINT,
                content_hash VARCHAR,
                body_byte_start BIGINT,
                body_text VARCHAR
            );
            INSERT INTO lance_ns.section_bodies VALUES
              ('a',NULL,'Brain Review Gate','.claude/skills/brain-review-gate/SKILL.md',
               1,0,'ha',0,
               '<!-- SPUR-MANAGED v=1 sha256=aaa -->\napprove or reject worker output gate'),
              ('b',NULL,'Brain Review Gate','.codex/skills/brain-review-gate/SKILL.md',
               1,0,'hb',0,
               '<!-- SPUR-MANAGED v=1 sha256=bbb -->\napprove or reject worker output gate'),
              ('c',NULL,'Brain Review Gate','crates/spur-core/src/skills/brain-review-gate/SKILL.md',
               1,0,'hc',0,
               '<!-- SPUR-MANAGED v=1 sha256=ccc -->\napprove or reject worker output gate'),
              ('p1',NULL,'Plan::S1','docs/superpowers/plans/p.md',
               2,0,'hp',0,'worker output review section one'),
              ('p2',NULL,'Plan::S2','docs/superpowers/plans/p.md',
               2,0,'hp',42,'worker output review section two'),
              ('p3',NULL,'Plan::S3','docs/superpowers/plans/p.md',
               2,0,'hp',84,'worker output review section three');

            CREATE TABLE nodes(
                stable_symbol_id VARCHAR,
                entity_name VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                symbol_kind VARCHAR
            );
            CREATE TABLE symbol_snapshots(stable_symbol_id VARCHAR, tokens VARCHAR[]);
            CREATE TABLE v_symbol_scorecard(
                stable_symbol_id VARCHAR,
                pagerank DOUBLE,
                churn_90d BIGINT,
                posture VARCHAR,
                component_size BIGINT
            );
            CREATE VIEW v_symbol_inbound AS
                SELECT stable_symbol_id, 0 AS callers FROM nodes;
            CREATE TABLE edges(
                source_stable_id VARCHAR,
                target_stable_id VARCHAR,
                relation VARCHAR,
                bind_method VARCHAR
            );
            "#,
        )?;
        let sections_fts =
            "PRAGMA create_fts_index('sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');";
        let symbol_fts =
            "PRAGMA create_fts_index('symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);";
        let (before_sections_fts, after_sections_fts) = INIT_SEARCH_SQL
            .split_once(sections_fts)
            .expect("init_search.sql should create the sections_search FTS index");
        let (before_symbol_fts, after_symbol_fts) = after_sections_fts
            .split_once(symbol_fts)
            .expect("init_search.sql should create the symbol_text FTS index");

        // create_fts_index starts its own internal transaction in bundled DuckDB,
        // so table creation must be committed before each PRAGMA runs.
        conn.execute_batch(before_sections_fts)?;
        conn.execute_batch(sections_fts)?;
        conn.execute_batch(before_symbol_fts)?;
        conn.execute_batch(symbol_fts)?;
        conn.execute_batch(after_symbol_fts)?;

        // 3 vendored copies must collapse to exactly 1.
        let copies: i64 = conn.query_row(
            "SELECT count(*) FROM sections_search WHERE qualified_name='Brain Review Gate'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            copies, 1,
            "vendored skill copies must dedup to one canonical row"
        );

        // Per-document cap: the 3-section plan must contribute at most 2 rows.
        let docs_plan_rows: i64 = conn.query_row(
            "SELECT count(*) FROM search_docs('worker output review') \
             WHERE file_path='docs/superpowers/plans/p.md'",
            [],
            |r| r.get(0),
        )?;
        assert!(
            docs_plan_rows <= 2,
            "one document must not exceed 2 search_docs rows"
        );

        let unified_plan_rows: i64 = conn.query_row(
            "SELECT count(*) FROM search('worker output review') \
             WHERE kind='doc' AND file='superpowers/plans/p.md'",
            [],
            |r| r.get(0),
        )?;
        assert!(
            unified_plan_rows <= 2,
            "one document must not exceed 2 unified search rows"
        );
        Ok(())
    }

    // Builds a tiny analyst-shaped DB with the BUNDLED duckdb (same lib the tool
    // reads with), creates the FTS index + search macro, and asserts the tool's
    // query path returns ranked rows. Proves fts + create_fts_index + the search
    // macro all work with the linked libduckdb — independent of the CLI version.
    #[test]
    fn search_docs_over_bundled_fixture_returns_ranked_rows() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("analyst.duckdb");
        {
            let conn = duckdb::Connection::open(&db)?;
            conn.execute_batch("INSTALL fts; LOAD fts;")?;
            // Separate statements: create_fts_index runs its own internal
            // transaction and must see `sections` already committed, so the
            // table population can't share a batch with the index build.
            conn.execute_batch(
                r#"
                CREATE TABLE sections(stable_symbol_id VARCHAR, section VARCHAR,
                                      file_path VARCHAR, body_text VARCHAR);
                INSERT INTO sections VALUES
                  ('s1','OAuth refresh','docs/oauth.md','how the oauth token refresh grant works'),
                  ('s2','Unrelated','docs/misc.md','notebook cells and kernels');
                CREATE TABLE _meta(graph_content_hash VARCHAR);
                INSERT INTO _meta VALUES ('deadbeef');
                "#,
            )?;
            conn.execute_batch(
                "PRAGMA create_fts_index('sections','stable_symbol_id','body_text', overwrite=1);",
            )?;
            conn.execute_batch(
                r#"
                CREATE OR REPLACE MACRO search_docs(q) AS TABLE
                  SELECT s.section AS section, s.file_path AS file_path,
                         fts_main_sections.match_bm25(s.stable_symbol_id, q) AS bm25
                  FROM sections s
                  WHERE fts_main_sections.match_bm25(s.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25 DESC;
                "#,
            )?;
            // Flush WAL → main file so the read-only reopen (which cannot replay
            // a WAL) sees the tables. The real analyst.duckdb is checkpointed by
            // the CLI on exit, so this only matters in-test.
            conn.execute_batch("CHECKPOINT;")?;
        }

        let (resolved, results, graph_hash) =
            search_rows("oauth token refresh", "docs", 10, db.to_str())
                .expect("semantic search over fixture");
        assert_eq!(resolved, db.display().to_string());
        assert_eq!(graph_hash.as_deref(), Some("deadbeef"));
        assert!(!results.is_empty(), "expected at least one ranked hit");
        assert_eq!(results[0]["title"], "OAuth refresh");
        // run() wraps the same rows into a CallToolResult without error.
        run("oauth token refresh", "docs", 10, db.to_str()).expect("run wraps result");
        Ok(())
    }

    #[test]
    fn search_graph_scope_returns_neighbor_kind_rows() {
        // Build a minimal fixture with FTS + scorecard + edges so search_graph can run.
        // Uses an in-memory DB with the same macro structure as the real analyst.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let conn = duckdb::Connection::open(&db_path).unwrap();

        conn.execute_batch(
            "
            INSTALL fts; LOAD fts; LOAD icu;

            -- Minimal nodes + symbol_text + scorecard + inbound + edges
            CREATE TABLE nodes (stable_symbol_id VARCHAR, node_id BIGINT,
              entity_name VARCHAR, qualified_name VARCHAR,
              file_path VARCHAR, symbol_kind VARCHAR,
              line_start INT, line_end INT);
            CREATE TABLE symbol_text (stable_symbol_id VARCHAR, entity_name VARCHAR,
              qualified_name VARCHAR, file_path VARCHAR, symbol_kind VARCHAR,
              doc_text VARCHAR);

            INSERT INTO nodes VALUES
              ('aa01', 1, 'handle_query', 'handle_query',
               'crates/spur-mcp/src/server.rs', 'function', 1, 20),
              ('aa02', 2, 'run_bm25_search', 'run_bm25_search',
               'crates/spur-mcp/src/search.rs', 'function', 1, 10);
            INSERT INTO symbol_text VALUES
              ('aa01', 'handle_query', 'handle_query',
               'crates/spur-mcp/src/server.rs', 'function',
               'handle_query search bm25 graph query'),
              ('aa02', 'run_bm25_search', 'run_bm25_search',
               'crates/spur-mcp/src/search.rs', 'function',
               'run_bm25_search search fts fulltext');
        ",
        )
        .unwrap();

        conn.execute_batch(
            "PRAGMA create_fts_index('symbol_text','stable_symbol_id','doc_text',overwrite=1);",
        )
        .unwrap();

        conn.execute_batch(
            "
            -- Minimal scorecard view
            CREATE VIEW v_symbol_scorecard AS
            SELECT stable_symbol_id,
                   0.001 AS pagerank, 0 AS churn_90d,
                   'leaf' AS posture, 1 AS component_size
            FROM nodes;

            -- Minimal inbound view (0 callers = safe to expand)
            CREATE VIEW v_symbol_inbound AS
            SELECT stable_symbol_id, 0 AS callers, 0 AS importers,
                   0 AS containers, 0 AS inbound_total
            FROM nodes;

            -- One call edge: aa01 calls aa02
            CREATE TABLE edges (source_stable_id VARCHAR, target_stable_id VARCHAR,
              relation VARCHAR, bind_method VARCHAR, edge_kind VARCHAR,
              src_id BIGINT, dst_id BIGINT, target_label VARCHAR,
              confidence VARCHAR, confidence_score DOUBLE);
            INSERT INTO edges VALUES
              ('aa01','aa02','calls','singleton','calls',1,2,'run_bm25_search','high',0.9);

            -- FTS macro (simplified version of real search_graph)
            CREATE OR REPLACE MACRO search_graph(q) AS TABLE
              SELECT kind, title, file, score, signal, neighbor_kind, edge_bind_method
              FROM (
                WITH base AS (
                  SELECT st.stable_symbol_id, st.entity_name AS symbol, st.symbol_kind,
                         st.file_path,
                         fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) AS bm25_raw,
                         sc.pagerank, sc.churn_90d, sc.posture,
                         COALESCE(vi.callers, 0) AS caller_count
                  FROM symbol_text st
                  JOIN v_symbol_scorecard sc USING (stable_symbol_id)
                  LEFT JOIN v_symbol_inbound vi USING (stable_symbol_id)
                  WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25_raw DESC LIMIT 5
                ),
                gated AS (
                  SELECT * FROM base
                  WHERE posture != 'load-bearing wall' OR caller_count <= 30
                ),
                primary_rows AS (
                  SELECT 'code' AS kind, symbol AS title,
                         regexp_replace(file_path,'^crates/','') AS file,
                         round(bm25_raw, 3) AS score,
                         posture AS signal, 'primary' AS neighbor_kind,
                         CAST(NULL AS VARCHAR) AS edge_bind_method
                  FROM base
                ),
                callee_rows AS (
                  SELECT 'code', ndst.entity_name,
                         regexp_replace(ndst.file_path,'^crates/',''),
                         0.0, 'leaf · callee of ' || g.symbol,
                         'callee', e.bind_method
                  FROM gated g
                  JOIN edges e ON e.source_stable_id = g.stable_symbol_id
                    AND e.relation = 'calls'
                  JOIN nodes ndst ON ndst.stable_symbol_id = e.target_stable_id
                  WHERE ndst.file_path NOT LIKE '.%'
                )
                SELECT * FROM primary_rows
                UNION ALL SELECT * FROM callee_rows
              )
              ORDER BY CASE neighbor_kind WHEN 'primary' THEN 0 ELSE 1 END, score DESC
              LIMIT 20;

            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('test-hash');
        ",
        )
        .unwrap();
        drop(conn);

        let (_, rows, _) = search_rows(
            "search bm25 graph",
            "graph",
            20,
            Some(db_path.to_str().unwrap()),
        )
        .unwrap();

        // Must have at least one primary and one callee row
        let has_primary = rows
            .iter()
            .any(|r| r["neighbor_kind"].as_str() == Some("primary"));
        let has_callee = rows
            .iter()
            .any(|r| r["neighbor_kind"].as_str() == Some("callee"));

        assert!(has_primary, "graph scope must return primary hits");
        assert!(has_callee, "graph scope must return callee neighbors");

        // All rows must have edge_bind_method field (may be null for primary)
        for row in &rows {
            assert!(
                row.get("edge_bind_method").is_some(),
                "every graph row must carry edge_bind_method key"
            );
        }
    }

    // Regression: the `code` scope reads `search_code`, which fuses the
    // `v_symbol_scorecard.posture` signal, which depends on v_symbol_churn_90d's
    // 90-day window — `now() - INTERVAL '90 day'` over a TIMESTAMP WITH TIME ZONE
    // column. That `-(TIMESTAMPTZ, INTERVAL)` overload lives in icu, which the
    // read path does not autoload, so without a `LOAD icu` the query fails to
    // prepare ("No function matches ... -(TIMESTAMP WITH TIME ZONE, INTERVAL)").
    // This fixture mirrors the production view chain with the real timestamptz
    // boundary and proves the `code` scope binds + returns the posture signal,
    // under the bundled libduckdb (fts + the statically-linked icu).
    #[test]
    fn search_code_scope_binds_temporal_views_via_icu() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("analyst.duckdb");
        {
            // The build connection mirrors the real build path (the `duckdb` CLI),
            // which has icu — needed to CREATE the aggregating timestamptz view.
            // The read path's icu load is exercised separately by search_rows below.
            let conn = duckdb::Connection::open(&db)?;
            conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu;")?;
            conn.execute_batch(
                r#"
                CREATE TABLE commits(sha VARCHAR, author_ts TIMESTAMP WITH TIME ZONE);
                INSERT INTO commits VALUES ('rec', now()), ('old', to_timestamp(0));
                CREATE TABLE temporal_edges(
                  target_stable_symbol_id VARCHAR, source_commit VARCHAR, change_kind VARCHAR);
                INSERT INTO temporal_edges VALUES
                  ('s1','rec','modified'), ('s1','rec','modified');
                CREATE TABLE symbol_text(
                  stable_symbol_id VARCHAR, entity_name VARCHAR, symbol_kind VARCHAR,
                  file_path VARCHAR, tokens VARCHAR);
                INSERT INTO symbol_text VALUES
                  ('s1','compress_response','function','crates/x.rs',
                   'code graph mcp response metadata compression payload'),
                  ('s2','unrelated','function','crates/y.rs','kernel notebook cells');
                CREATE TABLE _meta(graph_content_hash VARCHAR);
                INSERT INTO _meta VALUES ('cafebabe');
                "#,
            )?;
            // Production-shaped temporal chain — the churn boundary uses the exact
            // TIMESTAMPTZ `now() - INTERVAL` form from init_views.sql.
            conn.execute_batch(
                r#"
                CREATE VIEW v_symbol_churn_90d AS
                  SELECT t.target_stable_symbol_id AS stable_symbol_id, count(*) AS events
                  FROM temporal_edges t JOIN commits c ON c.sha = t.source_commit
                  WHERE c.author_ts > (now() - INTERVAL '90 day')
                  GROUP BY t.target_stable_symbol_id;
                CREATE VIEW v_symbol_scorecard AS
                  SELECT st.stable_symbol_id,
                         CASE
                           WHEN COALESCE(ch.events, 0) = 0 THEN 'load-bearing wall'
                           WHEN ch.events >= 10 THEN 'hot-central'
                           ELSE 'active'
                         END AS posture
                  FROM symbol_text st
                  LEFT JOIN v_symbol_churn_90d ch USING (stable_symbol_id);
                "#,
            )?;
            conn.execute_batch(
                "PRAGMA create_fts_index('symbol_text','stable_symbol_id','tokens', overwrite=1);",
            )?;
            conn.execute_batch(
                r#"
                CREATE OR REPLACE MACRO search_code(q) AS TABLE
                  SELECT st.entity_name AS symbol, st.symbol_kind, st.file_path,
                         round(fts_main_symbol_text.match_bm25(st.stable_symbol_id, q), 3) AS bm25,
                         sc.posture
                  FROM symbol_text st
                  JOIN v_symbol_scorecard sc USING (stable_symbol_id)
                  WHERE fts_main_symbol_text.match_bm25(st.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25 DESC;
                "#,
            )?;
            conn.execute_batch("CHECKPOINT;")?;
        }

        // search_rows reopens read-only and must LOAD icu so the TIMESTAMPTZ churn
        // boundary binds. Without that load this errors with the production
        // `-(TIMESTAMP WITH TIME ZONE, INTERVAL)` binder error.
        let (_path, results, graph_hash) =
            search_rows("response metadata compression", "code", 10, db.to_str())
                .expect("code-scope search must bind temporal views via icu");
        assert_eq!(graph_hash.as_deref(), Some("cafebabe"));
        assert!(!results.is_empty(), "expected at least one ranked code hit");
        assert_eq!(results[0]["title"], "compress_response");
        // posture flowed through as the `signal` column (active: churn==2, <10).
        assert_eq!(results[0]["signal"], "active");
        Ok(())
    }
}
