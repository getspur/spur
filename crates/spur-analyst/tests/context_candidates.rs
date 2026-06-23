#![allow(clippy::needless_raw_string_hashes)]

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use spur_analyst::{
    query_context_candidates, query_context_paths, query_graph_candidates,
    query_symbol_risk_community, KnowledgePathEngine, KnowledgePathOptions, KnowledgePathStatus,
    KnowledgeQueryOptions, KnowledgeSearchScope, SymbolEvidenceStatus, MAX_CONTEXT_PATHS,
    MAX_CONTEXT_PATH_HOPS,
};
use spur_graph::store::{write_sections_dataset, SECTIONS_DATASET_DIR};
use spur_graph::{artifact_from_facts, build_facts, EMBEDDING_VECTOR_DIMENSIONS};

const INIT_SEARCH_SQL: &str = include_str!("../../spur-context/analyst/init_search.sql");
const SECTION_EMBED_SKIP_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn context_candidates_return_stable_ids_for_docs_and_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;")
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
    conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;")
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
    conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;")
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
    conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;")
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

#[test]
fn symbol_risk_community_reads_materialized_views() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_symbol_risk_fixture(&conn);
    drop(conn);

    let result =
        query_symbol_risk_community(&db_path, &["sym-b", "sym-a"]).expect("query enrichment");

    assert_eq!(
        result.graph_content_hash.as_deref(),
        Some("risk-fixture-hash")
    );
    assert_eq!(result.max_symbols, 40);
    assert!(!result.truncated);
    assert!(result.caveats.is_empty(), "{:#?}", result.caveats);
    assert_eq!(result.risk_scorecard.len(), 2);
    assert_eq!(result.community_context.len(), 2);

    let risk_b = &result.risk_scorecard[0];
    assert_eq!(risk_b.input_index, 0);
    assert_eq!(risk_b.stable_symbol_id, "sym-b");
    assert_eq!(risk_b.status, SymbolEvidenceStatus::Available);
    assert_eq!(risk_b.entity_name.as_deref(), Some("beta"));
    assert_eq!(risk_b.qualified_name.as_deref(), Some("fixture::beta"));
    assert_eq!(risk_b.file_path.as_deref(), Some("src/b.rs"));
    assert_eq!(risk_b.symbol_kind.as_deref(), Some("function"));
    assert_eq!(risk_b.pagerank, Some(0.42));
    assert_eq!(risk_b.in_degree, Some(7));
    assert_eq!(risk_b.out_degree, Some(3));
    assert_eq!(risk_b.callers, Some(11));
    assert_eq!(risk_b.importers, Some(5));
    assert_eq!(risk_b.inbound_total, Some(19));
    assert_eq!(risk_b.churn_90d, Some(13));
    assert_eq!(risk_b.last_touched.as_deref(), Some("2026-06-15 12:00:00"));
    assert_eq!(risk_b.blast_radius_score, Some(8.5));
    assert_eq!(risk_b.posture.as_deref(), Some("hot-central"));
    assert!(risk_b.caveats.is_empty(), "{risk_b:#?}");

    let community_b = &result.community_context[0];
    assert_eq!(community_b.input_index, 0);
    assert_eq!(community_b.stable_symbol_id, "sym-b");
    assert_eq!(community_b.status, SymbolEvidenceStatus::Available);
    assert_eq!(community_b.component_id, Some(2));
    assert_eq!(community_b.component_size, Some(9));
    assert_eq!(community_b.community_id, Some(20));
    assert!(community_b.caveats.is_empty(), "{community_b:#?}");

    let metrics = result.graph_metrics.expect("graph metrics");
    assert_eq!(metrics.calls_edges, Some(100));
    assert_eq!(metrics.connected_nodes, Some(80));
    assert_eq!(metrics.components, Some(4));
    assert_eq!(metrics.largest_component, Some(25));
    assert_eq!(metrics.communities, Some(6));
    assert_eq!(metrics.density, Some(0.125));
}

#[test]
fn symbol_risk_community_returns_caveats_when_views_are_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('missing-views-hash');
        "#,
    )
    .expect("create incomplete fixture schema");
    drop(conn);

    let result =
        query_symbol_risk_community(&db_path, &["sym-a"]).expect("query missing-view enrichment");

    assert_eq!(
        result.graph_content_hash.as_deref(),
        Some("missing-views-hash")
    );
    assert_eq!(result.risk_scorecard.len(), 1);
    assert_eq!(
        result.risk_scorecard[0].status,
        SymbolEvidenceStatus::Unavailable
    );
    assert!(result.risk_scorecard[0].caveats.iter().any(|caveat| {
        caveat.code == "scorecard_unavailable" && caveat.message.contains("v_symbol_scorecard")
    }));
    assert_eq!(result.community_context.len(), 1);
    assert_eq!(
        result.community_context[0].status,
        SymbolEvidenceStatus::Unavailable
    );
    assert!(result.community_context[0].caveats.iter().any(|caveat| {
        caveat.code == "community_unavailable" && caveat.message.contains("v_symbol_component")
    }));
    assert!(result.graph_metrics.is_none());
    assert!(result
        .caveats
        .iter()
        .any(|caveat| caveat.code == "graph_metrics_unavailable"));
}

#[test]
fn symbol_risk_community_enriches_scorecard_with_timestamptz_arithmetic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    // The real v_symbol_scorecard view chain (via v_symbol_churn_90d) filters on
    // `now() - INTERVAL '90 day'`. now() is TIMESTAMP WITH TIME ZONE, so this
    // subtraction only binds when DuckDB's ICU extension is loaded. Load it here
    // so CREATE VIEW succeeds; the read-only query path under test must load it
    // too, or scorecard enrichment fails with a binder error.
    conn.execute_batch("INSTALL icu; LOAD icu;")
        .expect("load icu for fixture view creation");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('tz-fixture-hash');

        CREATE OR REPLACE VIEW v_symbol_scorecard AS
        SELECT
            'sym-a' AS stable_symbol_id,
            'alpha' AS entity_name,
            'fixture::alpha' AS qualified_name,
            'function' AS symbol_kind,
            'src/a.rs' AS file_path,
            0.12 AS pagerank,
            CAST(2 AS BIGINT) AS in_degree,
            CAST(1 AS BIGINT) AS out_degree,
            CAST(4 AS BIGINT) AS callers,
            CAST(1 AS BIGINT) AS importers,
            CAST(6 AS BIGINT) AS inbound_total,
            CAST(13 AS BIGINT) AS churn_90d,
            -- TIMESTAMPTZ - INTERVAL: only binds when ICU is loaded.
            now() - INTERVAL '90 day' AS last_touched,
            1.25 AS blast_radius_score,
            'load-bearing wall' AS posture;
        "#,
    )
    .expect("create timestamptz scorecard fixture");
    drop(conn);

    let result = query_symbol_risk_community(&db_path, &["sym-a"]).expect("query enrichment");

    assert_eq!(result.risk_scorecard.len(), 1);
    let row = &result.risk_scorecard[0];
    assert_eq!(
        row.status,
        SymbolEvidenceStatus::Available,
        "scorecard enrichment must load ICU for TIMESTAMPTZ arithmetic; caveats: {:#?}",
        result.caveats
    );
    assert!(
        !result
            .caveats
            .iter()
            .chain(row.caveats.iter())
            .any(|caveat| caveat.code == "scorecard_unavailable"),
        "unexpected scorecard_unavailable caveat: {:#?}",
        result.caveats
    );
    assert_eq!(row.churn_90d, Some(13));
    assert_eq!(row.entity_name.as_deref(), Some("alpha"));
    assert!(row.last_touched.is_some());
}

#[test]
fn symbol_risk_community_accepts_empty_candidate_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('empty-fixture-hash');
        "#,
    )
    .expect("create empty fixture schema");
    drop(conn);

    let result = query_symbol_risk_community::<&str>(&db_path, &[]).expect("query empty list");

    assert_eq!(
        result.graph_content_hash.as_deref(),
        Some("empty-fixture-hash")
    );
    assert!(result.risk_scorecard.is_empty());
    assert!(result.community_context.is_empty());
    assert!(result.caveats.is_empty(), "{:#?}", result.caveats);
    assert!(result.graph_metrics.is_none());
}

#[test]
fn symbol_risk_community_preserves_caller_order_deterministically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_symbol_risk_fixture(&conn);
    drop(conn);

    let result = query_symbol_risk_community(&db_path, &["sym-c", "sym-a", "sym-b", "sym-a"])
        .expect("query ordered enrichment");

    let risk_order = result
        .risk_scorecard
        .iter()
        .map(|row| (row.input_index, row.stable_symbol_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        risk_order,
        vec![(0, "sym-c"), (1, "sym-a"), (2, "sym-b"), (3, "sym-a")]
    );

    let community_order = result
        .community_context
        .iter()
        .map(|row| (row.input_index, row.stable_symbol_id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        community_order,
        vec![(0, "sym-c"), (1, "sym-a"), (2, "sym-b"), (3, "sym-a")]
    );
    assert_eq!(
        result.community_context[0].status,
        SymbolEvidenceStatus::MissingSymbol
    );
    assert!(result.community_context[0]
        .caveats
        .iter()
        .any(|caveat| caveat.code == "community_missing_symbol"));
}

#[test]
fn context_paths_return_bounded_shortest_path_rows_via_sql_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_path_fixture(&conn);
    drop(conn);

    let result = query_context_paths(
        &db_path,
        "sym-a",
        "sym-d",
        KnowledgePathOptions {
            max_hops: 4,
            max_paths: 2,
            undirected: false,
        },
    )
    .expect("query context paths");

    assert_eq!(
        result.graph_content_hash.as_deref(),
        Some("path-fixture-hash")
    );
    assert_eq!(result.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(result.status, KnowledgePathStatus::PathFound);
    assert_eq!(result.max_hops, 4);
    assert_eq!(result.max_paths, 2);
    assert_eq!(result.rows.len(), 4, "{:#?}", result.rows);
    assert!(result.rows.iter().all(|row| {
        row.engine == KnowledgePathEngine::RecursiveSql
            && row.status == KnowledgePathStatus::PathFound
            && row.caveat.is_none()
    }));

    let first_path = result
        .rows
        .iter()
        .filter(|row| row.path_index == 0)
        .map(|row| {
            (
                row.hop_index,
                row.source_stable_id.as_str(),
                row.target_stable_id.as_str(),
                row.relation.as_deref(),
                row.edge_kind.as_deref(),
                row.confidence.as_deref(),
                row.bind_method.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        first_path,
        vec![
            (
                0,
                "sym-a",
                "sym-b",
                Some("calls"),
                Some("calls"),
                Some("syntax_exact"),
                Some("singleton")
            ),
            (
                1,
                "sym-b",
                "sym-d",
                Some("calls"),
                Some("calls_dyn"),
                Some("heuristic"),
                Some("type_inference")
            ),
        ]
    );

    let path_indexes = result
        .rows
        .iter()
        .map(|row| row.path_index)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(path_indexes.into_iter().collect::<Vec<_>>(), vec![0, 1]);
}

#[test]
fn context_paths_count_parallel_edges_as_one_directed_node_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_parallel_edge_path_fixture(&conn);
    drop(conn);

    let result = query_context_paths(
        &db_path,
        "sym-a",
        "sym-b",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 5,
            undirected: false,
        },
    )
    .expect("query directed parallel-edge path");

    assert_eq!(result.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(result.status, KnowledgePathStatus::PathFound);
    assert_eq!(result.rows.len(), 1, "{:#?}", result.rows);
    let row = &result.rows[0];
    assert_eq!(row.path_index, 0);
    assert_eq!(row.hop_index, 0);
    assert_eq!(row.source_stable_id, "sym-a");
    assert_eq!(row.target_stable_id, "sym-b");
    assert_eq!(row.direction, None);
}

#[test]
fn undirected_context_paths_count_parallel_edges_as_one_node_sequence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_parallel_edge_path_fixture(&conn);
    drop(conn);

    let result = query_context_paths(
        &db_path,
        "sym-b",
        "sym-a",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 5,
            undirected: true,
        },
    )
    .expect("query undirected parallel-edge path");

    assert_eq!(result.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(result.status, KnowledgePathStatus::PathFound);
    assert_eq!(result.rows.len(), 1, "{:#?}", result.rows);
    let row = &result.rows[0];
    assert_eq!(row.path_index, 0);
    assert_eq!(row.hop_index, 0);
    assert_eq!(row.source_stable_id, "sym-b");
    assert_eq!(row.target_stable_id, "sym-a");
    assert_eq!(row.direction.as_deref(), Some("reverse"));
}

#[test]
fn contains_hops_are_not_traversable_in_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_contains_only_path_fixture(&conn);
    drop(conn);

    let undirected = query_context_paths(
        &db_path,
        "sym-source",
        "sym-target",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 3,
            undirected: true,
        },
    )
    .expect("query undirected containment path");
    assert_eq!(undirected.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(undirected.rows.len(), 0, "{undirected:#?}");
    assert_eq!(undirected.status, KnowledgePathStatus::NoPath);

    let directed = query_context_paths(
        &db_path,
        "sym-parent",
        "sym-source",
        KnowledgePathOptions {
            max_hops: 1,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query directed containment path");
    assert_eq!(directed.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(directed.rows.len(), 0, "{directed:#?}");
    assert_eq!(directed.status, KnowledgePathStatus::NoPath);
}

#[test]
fn external_import_hub_is_not_traversable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_external_import_hub_path_fixture(&conn);
    drop(conn);

    let undirected = query_context_paths(
        &db_path,
        "sym-a",
        "sym-b",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 3,
            undirected: true,
        },
    )
    .expect("query undirected external import hub path");
    assert_eq!(undirected.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(undirected.rows.len(), 0, "{undirected:#?}");
    assert_eq!(undirected.status, KnowledgePathStatus::NoPath);

    let directed = query_context_paths(
        &db_path,
        "sym-a",
        "sym-hub",
        KnowledgePathOptions {
            max_hops: 1,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query directed external import path");
    assert_eq!(directed.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(directed.rows.len(), 0, "{directed:#?}");
    assert_eq!(directed.status, KnowledgePathStatus::NoPath);
}

#[test]
fn real_calls_path_still_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_real_dependency_path_fixture(&conn);
    drop(conn);

    let calls = query_context_paths(
        &db_path,
        "sym-a",
        "sym-b",
        KnowledgePathOptions {
            max_hops: 1,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query directed calls path");
    assert_eq!(calls.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(calls.status, KnowledgePathStatus::PathFound);
    assert_eq!(calls.rows.len(), 1, "{calls:#?}");
    assert_eq!(calls.rows[0].edge_kind.as_deref(), Some("calls"));

    let first_party_import = query_context_paths(
        &db_path,
        "sym-a",
        "sym-c",
        KnowledgePathOptions {
            max_hops: 1,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query directed first-party import path");
    assert_eq!(first_party_import.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(first_party_import.status, KnowledgePathStatus::PathFound);
    assert_eq!(first_party_import.rows.len(), 1, "{first_party_import:#?}");
    let import_row = &first_party_import.rows[0];
    assert_eq!(import_row.relation.as_deref(), Some("imports"));
    assert_eq!(import_row.bind_method.as_deref(), Some("singleton"));
}

#[test]
fn context_paths_return_no_path_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_path_fixture(&conn);
    drop(conn);

    let result = query_context_paths(
        &db_path,
        "sym-d",
        "sym-a",
        KnowledgePathOptions {
            max_hops: 3,
            max_paths: 4,
            undirected: false,
        },
    )
    .expect("query context paths");

    assert_eq!(result.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(result.status, KnowledgePathStatus::NoPath);
    assert_eq!(result.max_hops, 3);
    assert_eq!(result.max_paths, 4);
    assert!(result.rows.is_empty());
    assert!(
        result
            .caveat
            .as_deref()
            .is_some_and(|caveat| caveat.contains("no path")),
        "{result:#?}"
    );
}

#[test]
fn context_paths_support_undirected_reverse_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_simple_directed_path_fixture(&conn);
    drop(conn);

    let forward = query_context_paths(
        &db_path,
        "sym-a",
        "sym-c",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query forward directed path");
    assert_eq!(forward.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(forward.status, KnowledgePathStatus::PathFound);
    assert_eq!(forward.rows.len(), 2, "{forward:#?}");
    assert!(
        forward.rows.iter().all(|row| row.direction.is_none()),
        "directed traversal should not populate direction: {forward:#?}"
    );

    let forward_undirected = query_context_paths(
        &db_path,
        "sym-a",
        "sym-c",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 1,
            undirected: true,
        },
    )
    .expect("query forward undirected path");
    assert_eq!(forward_undirected.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(forward_undirected.status, KnowledgePathStatus::PathFound);
    let forward_path = forward_undirected
        .rows
        .iter()
        .map(|row| {
            (
                row.hop_index,
                row.source_stable_id.as_str(),
                row.target_stable_id.as_str(),
                row.direction.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        forward_path,
        vec![
            (0, "sym-a", "sym-b", Some("forward")),
            (1, "sym-b", "sym-c", Some("forward")),
        ]
    );

    let reverse_directed = query_context_paths(
        &db_path,
        "sym-c",
        "sym-a",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 1,
            undirected: false,
        },
    )
    .expect("query reverse directed path");
    assert_eq!(reverse_directed.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(reverse_directed.status, KnowledgePathStatus::NoPath);
    assert!(reverse_directed.rows.is_empty(), "{reverse_directed:#?}");

    let reverse_undirected = query_context_paths(
        &db_path,
        "sym-c",
        "sym-a",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 1,
            undirected: true,
        },
    )
    .expect("query reverse undirected path");
    assert_eq!(reverse_undirected.engine, KnowledgePathEngine::RecursiveSql);
    assert_eq!(reverse_undirected.status, KnowledgePathStatus::PathFound);
    let reverse_path = reverse_undirected
        .rows
        .iter()
        .map(|row| {
            (
                row.hop_index,
                row.source_stable_id.as_str(),
                row.target_stable_id.as_str(),
                row.direction.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reverse_path,
        vec![
            (0, "sym-c", "sym-b", Some("reverse")),
            (1, "sym-b", "sym-a", Some("reverse")),
        ]
    );
}

#[test]
fn context_paths_clamp_max_hops_and_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    create_path_fixture(&conn);
    drop(conn);

    let lower = query_context_paths(
        &db_path,
        "sym-a",
        "sym-b",
        KnowledgePathOptions {
            max_hops: 0,
            max_paths: 0,
            undirected: false,
        },
    )
    .expect("query lower-clamped context paths");
    assert_eq!(lower.max_hops, 1);
    assert_eq!(lower.max_paths, 1);
    assert_eq!(lower.rows.len(), 1);

    let upper = query_context_paths(
        &db_path,
        "sym-a",
        "sym-d",
        KnowledgePathOptions {
            max_hops: usize::MAX,
            max_paths: usize::MAX,
            undirected: false,
        },
    )
    .expect("query upper-clamped context paths");
    assert_eq!(upper.max_hops, MAX_CONTEXT_PATH_HOPS);
    assert_eq!(upper.max_paths, MAX_CONTEXT_PATHS);
}

#[test]
fn context_paths_return_unavailable_row_when_edges_schema_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("analyst.duckdb");
    let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('path-fixture-hash');
        "#,
    )
    .expect("create incomplete fixture schema");
    drop(conn);

    let result = query_context_paths(
        &db_path,
        "sym-a",
        "sym-b",
        KnowledgePathOptions {
            max_hops: 2,
            max_paths: 2,
            undirected: false,
        },
    )
    .expect("query context paths");

    assert_eq!(result.engine, KnowledgePathEngine::Unavailable);
    assert_eq!(result.status, KnowledgePathStatus::Unavailable);
    assert_eq!(result.rows.len(), 1, "{:#?}", result.rows);
    let row = &result.rows[0];
    assert_eq!(row.source_stable_id, "sym-a");
    assert_eq!(row.target_stable_id, "sym-b");
    assert_eq!(row.engine, KnowledgePathEngine::Unavailable);
    assert_eq!(row.status, KnowledgePathStatus::Unavailable);
    assert!(row.relation.is_none());
    assert!(
        row.caveat
            .as_deref()
            .is_some_and(|caveat| caveat.contains("unavailable")),
        "{row:#?}"
    );
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

fn create_symbol_risk_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('risk-fixture-hash');

        CREATE TABLE v_symbol_scorecard (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            file_path VARCHAR,
            pagerank DOUBLE,
            in_degree BIGINT,
            out_degree BIGINT,
            callers BIGINT,
            importers BIGINT,
            inbound_total BIGINT,
            churn_90d BIGINT,
            last_touched TIMESTAMP,
            blast_radius_score DOUBLE,
            posture VARCHAR
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-b', 'beta', 'fixture::beta', 'function', 'src/b.rs',
             0.42, 7, 3, 11, 5, 19, 13, TIMESTAMP '2026-06-15 12:00:00', 8.5, 'hot-central'),
            ('sym-a', 'alpha', 'fixture::alpha', 'function', 'src/a.rs',
             0.12, 2, 1, 4, 1, 6, 0, NULL, 1.25, 'load-bearing wall');

        CREATE TABLE v_symbol_component (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            component_id BIGINT,
            component_size BIGINT
        );
        INSERT INTO v_symbol_component VALUES
            ('sym-a', 1, 1, 3),
            ('sym-b', 2, 2, 9);

        CREATE TABLE v_symbol_community (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            community_id BIGINT
        );
        INSERT INTO v_symbol_community VALUES
            ('sym-a', 1, 10),
            ('sym-b', 2, 20);

        CREATE TABLE v_graph_metrics (
            calls_edges BIGINT,
            connected_nodes BIGINT,
            components BIGINT,
            largest_component BIGINT,
            communities BIGINT,
            density DOUBLE
        );
        INSERT INTO v_graph_metrics VALUES (100, 80, 4, 25, 6, 0.125);
        "#,
    )
    .expect("create symbol risk fixture schema");
}

fn create_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-a', 1, 'src/a.rs', 'a', 'fixture::a', 'function'),
            ('sym-b', 2, 'src/b.rs', 'b', 'fixture::b', 'function'),
            ('sym-c', 3, 'src/c.rs', 'c', 'fixture::c', 'function'),
            ('sym-d', 4, 'src/d.rs', 'd', 'fixture::d', 'function'),
            ('sym-e', 5, 'src/e.rs', 'e', 'fixture::e', 'function'),
            ('sym-f', 6, 'src/f.rs', 'f', 'fixture::f', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-a', 'sym-b', 1, 2, 'b', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-b', 'sym-d', 2, 4, 'd', 'calls', 'heuristic', 0.6, 'calls_dyn', 'type_inference'),
            ('sym-a', 'sym-c', 1, 3, 'c', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-c', 'sym-d', 3, 4, 'd', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-a', 'sym-e', 1, 5, 'e', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-e', 'sym-f', 5, 6, 'f', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-f', 'sym-d', 6, 4, 'd', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton');
        "#,
    )
    .expect("create path fixture schema");
}

fn create_parallel_edge_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('parallel-path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-a', 1, 'src/a.rs', 'a', 'fixture::a', 'function'),
            ('sym-b', 2, 'src/b.rs', 'b', 'fixture::b', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-a', 'sym-b', 1, 2, 'b', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-a', 'sym-b', 1, 2, 'b', 'references_other', 'syntax_exact', 0.8, 'references_other', 'name_resolution'),
            ('sym-a', 'sym-b', 1, 2, 'b', 'imports', 'syntax_exact', 0.7, 'imports', 'import_resolution');
        "#,
    )
    .expect("create parallel edge path fixture schema");
}

fn create_contains_only_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('contains-path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-parent', 1, 'src/parent.rs', 'parent', 'fixture::parent', 'module'),
            ('sym-source', 2, 'src/parent.rs', 'source', 'fixture::parent::source', 'function'),
            ('sym-target', 3, 'src/parent.rs', 'target', 'fixture::parent::target', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-parent', 'sym-source', 1, 2, 'source', 'contains', 'syntax_exact', 1.0, 'references_other', 'scope'),
            ('sym-parent', 'sym-target', 1, 3, 'target', 'contains', 'syntax_exact', 1.0, 'references_other', 'scope');
        "#,
    )
    .expect("create contains-only path fixture schema");
}

fn create_external_import_hub_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('external-import-path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-a', 1, 'src/a.rs', 'a', 'fixture::a', 'function'),
            ('sym-b', 2, 'src/b.rs', 'b', 'fixture::b', 'function'),
            ('sym-hub', 3, 'external/serde.rs', 'serde', 'serde', 'module');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-a', 'sym-hub', 1, 3, 'serde', 'imports', 'syntax_exact', 1.0, 'references_other', 'external'),
            ('sym-b', 'sym-hub', 2, 3, 'serde', 'imports', 'syntax_exact', 1.0, 'references_other', 'external');
        "#,
    )
    .expect("create external-import hub path fixture schema");
}

fn create_real_dependency_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('real-dependency-path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-a', 1, 'src/a.rs', 'a', 'fixture::a', 'function'),
            ('sym-b', 2, 'src/b.rs', 'b', 'fixture::b', 'function'),
            ('sym-c', 3, 'src/c.rs', 'c', 'fixture::c', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-a', 'sym-b', 1, 2, 'b', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-a', 'sym-c', 1, 3, 'c', 'imports', 'syntax_exact', 1.0, 'references_other', 'singleton');
        "#,
    )
    .expect("create real-dependency path fixture schema");
}

fn create_simple_directed_path_fixture(conn: &duckdb::Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('simple-path-fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-a', 1, 'src/a.rs', 'a', 'fixture::a', 'function'),
            ('sym-b', 2, 'src/b.rs', 'b', 'fixture::b', 'function'),
            ('sym-c', 3, 'src/c.rs', 'c', 'fixture::c', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-a', 'sym-b', 1, 2, 'b', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton'),
            ('sym-b', 'sym-c', 2, 3, 'c', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton');
        "#,
    )
    .expect("create simple path fixture schema");
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
