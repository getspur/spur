use spur_analyst::{query_context_candidates, KnowledgeQueryOptions, KnowledgeSearchScope};

const INIT_SEARCH_SQL: &str = include_str!("../../spur-context/poc/duckdb-analyst/init_search.sql");

#[test]
fn context_candidates_return_stable_ids_for_docs_and_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu;")
        .expect("load fixture extensions");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('fixture-hash');

        CREATE TABLE sections_search (
            stable_symbol_id VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            heading_level INTEGER,
            content_hash VARCHAR,
            body_text VARCHAR
        );
        INSERT INTO sections_search VALUES
            ('doc-1', 'Context Candidate Design', 'docs/context.md', 2, 'doc-hash',
             'knowledge context candidate retrieval');

        CREATE TABLE symbol_text (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR,
            doc_text VARCHAR
        );
        INSERT INTO symbol_text VALUES
            ('sym-1', 'query_context_candidates', 'spur_analyst::query_context_candidates',
             'crates/spur-analyst/src/lib.rs', 'function', 'knowledge context candidate retrieval');

        CREATE TABLE v_symbol_scorecard (
            stable_symbol_id VARCHAR,
            pagerank DOUBLE,
            churn_90d BIGINT,
            posture VARCHAR,
            component_size BIGINT
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-1', 0.01, 3, 'stable', 1);
        "#,
    )
    .expect("create fixture schema");
    conn.execute_batch(
        r#"
        PRAGMA create_fts_index('main.sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');
        PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);
        "#,
    )
    .expect("create fixture fts indexes");
    let macro_sql = context_candidate_macro_sql();
    conn.execute_batch(&macro_sql).expect("define search macro");
    drop(conn);

    let result = query_context_candidates(
        &db_path,
        "knowledge context candidate",
        KnowledgeSearchScope::All,
        KnowledgeQueryOptions { limit: 10 },
    )
    .expect("query context candidates");

    assert_eq!(result.graph_content_hash.as_deref(), Some("fixture-hash"));
    assert!(result
        .candidates
        .iter()
        .any(|candidate| candidate.kind == "doc"
            && candidate.stable_symbol_id.as_deref() == Some("doc-1")
            && candidate.score.is_finite()));
    assert!(result
        .candidates
        .iter()
        .any(|candidate| candidate.kind == "code"
            && candidate.stable_symbol_id.as_deref() == Some("sym-1")
            && candidate.score.is_finite()));
}

fn context_candidate_macro_sql() -> String {
    INIT_SEARCH_SQL
        .split("CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope) AS TABLE")
        .nth(1)
        .and_then(|rest| rest.split("-- Graph-augmented:").next())
        .map(|body| {
            let start =
                "CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope) AS TABLE";
            format!("{start}{body}")
        })
        .expect("context candidate macro should be present in init_search.sql")
}
