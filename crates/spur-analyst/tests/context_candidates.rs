use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use spur_analyst::{
    query_context_candidates, query_graph_candidates, KnowledgeQueryOptions, KnowledgeSearchScope,
};
use spur_graph::store::{write_sections_dataset, SECTIONS_DATASET_DIR};
use spur_graph::{artifact_from_facts, build_facts, EMBEDDING_VECTOR_DIMENSIONS};

const INIT_SEARCH_SQL: &str = include_str!("../../spur-context/poc/duckdb-analyst/init_search.sql");
const SECTION_EMBED_SKIP_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
            component_size BIGINT,
            callers BIGINT
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-1', 0.01, 3, 'stable', 1, 2);

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );
        INSERT INTO v_symbol_inbound VALUES ('sym-1', 2);
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
        KnowledgeQueryOptions {
            limit: 10,
            ..KnowledgeQueryOptions::default()
        },
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

#[test]
fn context_candidates_accept_query_vector_and_degrade_to_bm25() {
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
            component_size BIGINT,
            callers BIGINT
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-1', 0.01, 3, 'stable', 1, 2);

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );
        INSERT INTO v_symbol_inbound VALUES ('sym-1', 2);
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

    let baseline = query_context_candidates(
        &db_path,
        "knowledge context candidate",
        KnowledgeSearchScope::All,
        KnowledgeQueryOptions {
            limit: 10,
            ..KnowledgeQueryOptions::default()
        },
    )
    .expect("query baseline context candidates");

    let result = query_context_candidates(
        &db_path,
        "knowledge context candidate",
        KnowledgeSearchScope::All,
        KnowledgeQueryOptions {
            limit: 10,
            // Correct-dimensional vectors exercise the real hybrid error path
            // here: this fixture defines the macro but does not provide the
            // Lance sidecar it points at, so Rust falls back to BM25.
            query_vec: Some(vec![0.0; EMBEDDING_VECTOR_DIMENSIONS]),
            ..KnowledgeQueryOptions::default()
        },
    )
    .expect("query context candidates");

    assert_eq!(
        candidate_brief(&result.candidates),
        candidate_brief(&baseline.candidates)
    );
    assert!(result
        .candidates
        .iter()
        .any(|candidate| candidate.grounding.starts_with("bm25")));
}

#[test]
fn context_candidates_surface_semantic_only_docs_via_hybrid_fusion() {
    let _lock = env_lock();
    let _skip = EnvGuard::set(SECTION_EMBED_SKIP_ENV, "1");

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/lexical-one.md"),
        "# Lexical One\n\nranking beacon ranking beacon ranking beacon.\n",
    )
    .expect("write lexical fixture one");
    fs::write(
        root.join("docs/lexical-two.md"),
        "# Lexical Two\n\nranking beacon ranking beacon ranking beacon.\n",
    )
    .expect("write lexical fixture two");
    fs::write(
        root.join("docs/semantic-only.md"),
        "# Semantic Only\n\nvector manifold cosine neighborhood.\n",
    )
    .expect("write semantic fixture");

    let facts = build_facts(&root, None).expect("build fixture facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("build fixture artifact");
    let artifact_dir = dir.path().join("artifact");
    write_sections_dataset(&artifact, &root, &artifact_dir).expect("write Lance sidecar");

    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu; INSTALL lance; LOAD lance;")
        .expect("load fixture extensions");
    conn.execute_batch(&format!(
        "ATTACH '{}' AS lance_ns (TYPE LANCE);",
        sql_escape_path(&artifact_dir.join(SECTIONS_DATASET_DIR))
    ))
    .expect("attach Lance sections");

    let query_vec = semantic_query_vec();
    seed_section_vectors(
        &conn,
        &[("docs/semantic-only.md", query_vec.as_slice())],
        &[],
    );

    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('hybrid-fixture-hash');

        CREATE TABLE sections_search AS
        SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
        FROM lance_ns.main.section_bodies;

        CREATE TABLE symbol_text (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR,
            doc_text VARCHAR
        );

        CREATE TABLE v_symbol_scorecard (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR,
            pagerank DOUBLE,
            churn_90d BIGINT,
            posture VARCHAR,
            component_size BIGINT,
            callers BIGINT
        );

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );
        "#,
    )
    .expect("create hybrid fixture schema");
    conn.execute_batch(
        r#"
        PRAGMA create_fts_index('main.sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');
        PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);
        "#,
    )
    .expect("create hybrid fixture indexes");
    let macro_sql = context_candidate_macro_sql_with_artifact_dir(&artifact_dir);
    conn.execute_batch(&macro_sql)
        .expect("define context candidate macros");
    drop(conn);

    let bm25_only = query_context_candidates(
        &db_path,
        "ranking beacon",
        KnowledgeSearchScope::Docs,
        KnowledgeQueryOptions {
            limit: 2,
            ..KnowledgeQueryOptions::default()
        },
    )
    .expect("query bm25-only candidates");
    assert!(
        !bm25_only
            .candidates
            .iter()
            .any(|candidate| candidate.file_path == "docs/semantic-only.md"),
        "semantic-only doc should not appear in BM25-only results: {:?}",
        candidate_brief(&bm25_only.candidates)
    );

    let hybrid = query_context_candidates(
        &db_path,
        "ranking beacon",
        KnowledgeSearchScope::Docs,
        KnowledgeQueryOptions {
            limit: 2,
            query_vec: Some(query_vec),
            ..KnowledgeQueryOptions::default()
        },
    )
    .expect("query hybrid candidates");

    assert!(
        hybrid.candidates.iter().any(|candidate| {
            candidate.file_path == "docs/semantic-only.md" && candidate.grounding == "hybrid-doc"
        }),
        "hybrid fusion should surface the semantic-only doc: {:?}",
        candidate_brief(&hybrid.candidates)
    );
}

#[test]
fn graph_candidates_return_primary_and_neighbor_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu;")
        .expect("load fixture extensions");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('graph-fixture-hash');

        CREATE TABLE symbol_text (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR,
            doc_text VARCHAR
        );
        INSERT INTO symbol_text VALUES
            ('sym-1', 'query_graph_candidates', 'spur_analyst::query_graph_candidates',
             'crates/spur-analyst/src/lib.rs', 'function', 'knowledge graph candidate retrieval');

        CREATE TABLE v_symbol_scorecard (
            stable_symbol_id VARCHAR,
            pagerank DOUBLE,
            churn_90d BIGINT,
            posture VARCHAR,
            component_size BIGINT
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-1', 0.01, 3, 'stable', 1),
            ('sym-2', 0.02, 1, 'important', 1);

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );
        INSERT INTO v_symbol_inbound VALUES ('sym-1', 1);

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-1', 'query_graph_candidates', 'spur_analyst::query_graph_candidates',
             'crates/spur-analyst/src/lib.rs', 'function'),
            ('sym-2', 'call_graph_neighbor', 'fixture::call_graph_neighbor',
             'crates/spur-analyst/src/neighbor.rs', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            relation VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES ('sym-2', 'sym-1', 'calls', 'resolved');
        "#,
    )
    .expect("create fixture schema");
    conn.execute_batch(
        "PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);",
    )
    .expect("create fixture fts index");
    let macro_sql = graph_candidate_macro_sql();
    conn.execute_batch(&macro_sql)
        .expect("define graph search macro");
    drop(conn);

    let result = query_graph_candidates(
        &db_path,
        "knowledge graph candidate",
        KnowledgeQueryOptions {
            limit: 10,
            ..KnowledgeQueryOptions::default()
        },
    )
    .expect("query graph candidates");

    assert_eq!(
        result.graph_content_hash.as_deref(),
        Some("graph-fixture-hash")
    );
    assert!(result
        .candidates
        .iter()
        .any(|candidate| candidate.kind == "code"
            && candidate.title == "query_graph_candidates"
            && candidate.neighbor_kind.as_deref() == Some("primary")
            && candidate.grounding == "bm25-graph"
            && candidate.score.is_finite()));
    assert!(result
        .candidates
        .iter()
        .any(|candidate| candidate.kind == "code"
            && candidate.title == "call_graph_neighbor"
            && candidate.stable_symbol_id.as_deref() == Some("sym-2")
            && candidate.neighbor_kind.as_deref() == Some("caller")
            && candidate.edge_bind_method.as_deref() == Some("resolved")));
}

fn context_candidate_macro_sql() -> String {
    INIT_SEARCH_SQL
        .split("CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE")
        .nth(1)
        .and_then(|rest| rest.split("-- Graph-augmented:").next())
        .map(|body| {
            let start = "CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE";
            format!("{start}{body}")
        })
        .expect("context candidate macro should be present in init_search.sql")
}

fn context_candidate_macro_sql_with_artifact_dir(artifact_dir: &Path) -> String {
    context_candidate_macro_sql().replace(
        "__SPUR_GRAPH_ARTIFACT_DIR__",
        &sql_escape_path(artifact_dir),
    )
}

fn graph_candidate_macro_sql() -> String {
    INIT_SEARCH_SQL
        .split("CREATE OR REPLACE MACRO search_graph(q, intent) AS TABLE")
        .nth(1)
        .map(|body| {
            let start = "CREATE OR REPLACE MACRO search_graph(q, intent) AS TABLE";
            format!("{start}{body}")
        })
        .expect("graph candidate macro should be present in init_search.sql")
}

fn candidate_brief(
    candidates: &[spur_analyst::KnowledgeCandidate],
) -> Vec<(String, String, String, String)> {
    candidates
        .iter()
        .map(|candidate| {
            (
                candidate.kind.clone(),
                candidate.title.clone(),
                candidate.file_path.clone(),
                candidate.grounding.clone(),
            )
        })
        .collect()
}

fn semantic_query_vec() -> Vec<f32> {
    let mut query_vec = vec![0.0; EMBEDDING_VECTOR_DIMENSIONS];
    query_vec[0] = 1.0;
    query_vec
}

fn seed_section_vectors(
    conn: &duckdb::Connection,
    semantic_rows: &[(&str, &[f32])],
    lexical_rows: &[(&str, &[f32])],
) {
    let overrides = semantic_rows
        .iter()
        .chain(lexical_rows.iter())
        .map(|(file_path, vector)| {
            format!(
                "('{}', {})",
                file_path.replace('\'', "''"),
                format_query_vec_sql(vector)
            )
        })
        .collect::<Vec<_>>();
    let sql = format!(
        r#"
        CREATE OR REPLACE TABLE lance_ns.main.section_bodies AS
        SELECT s.stable_symbol_id,
               s.file_path,
               s.qualified_name,
               s.heading_level,
               s.body_text,
               s.body_byte_start,
               s.body_byte_end,
               s.child_count,
               s.parent_stable_id,
               s.content_hash,
               o.vector
        FROM lance_ns.main.section_bodies AS s
        LEFT JOIN (
            SELECT col0 AS file_path, col1 AS vector
            FROM (VALUES {})
        ) AS o USING (file_path);
        "#,
        overrides.join(",\n                  ")
    );
    conn.execute_batch(&sql)
        .expect("seed fixture section vectors");
}

fn format_query_vec_sql(query_vec: &[f32]) -> String {
    let values = query_vec
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]::FLOAT[{EMBEDDING_VECTOR_DIMENSIONS}]")
}

fn sql_escape_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().expect("env lock")
}
