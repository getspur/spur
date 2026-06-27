use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use spur_context_service::knowledge::{
    query_knowledge_context, KnowledgeContextOptions, KnowledgeScope,
};

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "demo";
const REVISION: &str = "1.0.0";
const DIMENSIONS: usize = 768;
const EMBEDDING_MODEL: &str = "EmbeddingGemma300M";
const EMBED_TEXT_VERSION: &str = "v3-embeddinggemma-300m";
const LEGACY_EMBEDDING_MODEL: &str = "LegacyEmbeddingModel";
const LEGACY_EMBED_TEXT_VERSION: &str = "v0-legacy";

#[test]
fn bm25_only_search_returns_ranked_code_and_docs() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;

    let result = query_knowledge_context(
        &fixture.conn,
        &knowledge_options("parse config loader", KnowledgeScope::All, None, 8),
    )?;

    assert!(result.answerable);
    assert_eq!(result.graph_content_hash.as_deref(), Some("fixture-hash"));
    assert!(result
        .primary_evidence
        .iter()
        .any(|evidence| evidence.stable_symbol_id.as_deref()
            == Some("pkg:demo@1.0.0::demo::parse_config_loader")
            && evidence.grounding == "bm25-code"));
    assert!(result.supporting_docs.iter().any(|evidence| {
        evidence.stable_symbol_id.as_deref() == Some("doc-parse")
            && evidence.grounding == "bm25-doc"
    }));
    assert_eq!(result.candidates.total_code, result.primary_evidence.len());
    assert_eq!(result.candidates.total_docs, result.supporting_docs.len());
    Ok(())
}

#[test]
fn vector_only_search_surfaces_semantic_code_without_bm25_hits() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;

    let result = query_knowledge_context(
        &fixture.conn,
        &knowledge_options(
            "unmatched lexical query",
            KnowledgeScope::Code,
            Some(unit_vector(0)),
            3,
        ),
    )?;

    let top = result
        .primary_evidence
        .first()
        .context("expected vector evidence")?;
    assert_eq!(
        top.stable_symbol_id.as_deref(),
        Some("pkg:demo@1.0.0::demo::runtime::task_spawner")
    );
    assert_eq!(top.grounding, "hybrid-code");
    assert!(top.score > 0.99, "expected near-identical vector score");
    assert!(result.supporting_docs.is_empty());
    Ok(())
}

#[test]
fn vector_search_ignores_embeddings_from_other_models() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;
    insert_embedding_with_model(
        &fixture.conn,
        "sym-legacy",
        "src/aaa_legacy.rs",
        "legacy_task_spawner",
        "demo::legacy_task_spawner",
        unit_vector(0),
        LEGACY_EMBEDDING_MODEL,
        LEGACY_EMBED_TEXT_VERSION,
    )?;

    let result = query_knowledge_context(
        &fixture.conn,
        &knowledge_options(
            "unmatched lexical query",
            KnowledgeScope::Code,
            Some(unit_vector(0)),
            3,
        ),
    )?;

    let top = result
        .primary_evidence
        .first()
        .context("expected Gemma vector evidence")?;
    assert_eq!(
        top.stable_symbol_id.as_deref(),
        Some("pkg:demo@1.0.0::demo::runtime::task_spawner")
    );
    assert_ne!(
        top.stable_symbol_id.as_deref(),
        Some("pkg:demo@1.0.0::demo::legacy_task_spawner")
    );
    Ok(())
}

#[test]
fn hybrid_search_deduplicates_bm25_and_vector_symbol_hits() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;

    let result = query_knowledge_context(
        &fixture.conn,
        &knowledge_options(
            "parse config loader",
            KnowledgeScope::All,
            Some(unit_vector(1)),
            8,
        ),
    )?;

    let parse_hits = result
        .primary_evidence
        .iter()
        .filter(|evidence| {
            evidence.stable_symbol_id.as_deref()
                == Some("pkg:demo@1.0.0::demo::parse_config_loader")
        })
        .collect::<Vec<_>>();
    assert_eq!(parse_hits.len(), 1);
    assert_eq!(parse_hits[0].grounding, "hybrid-code");
    assert!(result
        .supporting_docs
        .iter()
        .any(|evidence| evidence.stable_symbol_id.as_deref() == Some("doc-parse")));
    Ok(())
}

#[test]
fn scope_filters_code_docs_and_all_results() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;

    let code = query_knowledge_context(
        &fixture.conn,
        &knowledge_options("parse config", KnowledgeScope::Code, None, 8),
    )?;
    assert!(!code.primary_evidence.is_empty());
    assert!(code.supporting_docs.is_empty());

    let docs = query_knowledge_context(
        &fixture.conn,
        &knowledge_options("parse config", KnowledgeScope::Docs, None, 8),
    )?;
    assert!(docs.primary_evidence.is_empty());
    assert!(!docs.supporting_docs.is_empty());

    let all = query_knowledge_context(
        &fixture.conn,
        &knowledge_options("parse config", KnowledgeScope::All, None, 8),
    )?;
    assert!(!all.primary_evidence.is_empty());
    assert!(!all.supporting_docs.is_empty());
    Ok(())
}

#[test]
fn confidence_rating_uses_score_thresholds_and_evidence_count() -> Result<()> {
    let fixture = KnowledgeFixture::new()?;

    let high = query_knowledge_context(
        &fixture.conn,
        &knowledge_options(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu",
            KnowledgeScope::All,
            None,
            8,
        ),
    )?;
    assert_eq!(high.confidence, "high");

    let low = query_knowledge_context(
        &fixture.conn,
        &knowledge_options("terms absent from fixture", KnowledgeScope::All, None, 8),
    )?;
    assert!(!low.answerable);
    assert_eq!(low.confidence, "low");
    Ok(())
}

struct KnowledgeFixture {
    conn: Connection,
}

impl KnowledgeFixture {
    fn new() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
        create_schema(&conn)?;
        seed_fixture(&conn)?;
        Ok(Self { conn })
    }
}

fn knowledge_options(
    query: &str,
    scope: KnowledgeScope,
    query_vec: Option<Vec<f32>>,
    limit: usize,
) -> KnowledgeContextOptions {
    KnowledgeContextOptions {
        query: query.to_owned(),
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        scope,
        limit,
        query_vec,
    }
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('fixture-hash');

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            byte_range_start INTEGER,
            byte_range_end INTEGER,
            line_start INTEGER,
            line_end INTEGER,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            anchor_hash VARCHAR,
            enclosing_scope VARCHAR
        );

        CREATE TABLE section_bodies (
            section_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            title VARCHAR,
            body_text VARCHAR,
            body_hash VARCHAR,
            token_count INTEGER
        );

        CREATE TABLE symbol_embeddings (
            stable_symbol_id VARCHAR,
            package VARCHAR,
            source VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            embedding FLOAT[],
            embedding_model VARCHAR,
            embedding_input_hash VARCHAR,
            embed_text_version VARCHAR
        );
        ",
    )
    .context("create knowledge schema")?;
    Ok(())
}

fn seed_fixture(conn: &Connection) -> Result<()> {
    insert_node(
        conn,
        "sym-parse",
        "src/lib.rs",
        "parse_config_loader",
        "demo::parse_config_loader",
    )?;
    insert_node(
        conn,
        "sym-semantic",
        "src/semantic.rs",
        "task_spawner",
        "demo::runtime::task_spawner",
    )?;
    insert_node(
        conn,
        "sym-high-a",
        "src/high.rs",
        "alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu",
        "demo::alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu",
    )?;
    insert_node(
        conn,
        "sym-high-b",
        "src/high.rs",
        "alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu_helper",
        "demo::alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu_helper",
    )?;
    insert_other_revision_node(conn)?;

    insert_doc(
        conn,
        "doc-parse",
        "docs/parser.md",
        "Parser Guide",
        "The parse config loader reads config documents and validates loader inputs.",
    )?;
    insert_doc(
        conn,
        "doc-high",
        "docs/high.md",
        "High Confidence Guide",
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu",
    )?;
    insert_doc(
        conn,
        "doc-unrelated",
        "docs/other.md",
        "Other Guide",
        "unrelated packaging notes",
    )?;

    insert_embedding(
        conn,
        "sym-semantic",
        "src/semantic.rs",
        "task_spawner",
        "demo::runtime::task_spawner",
        unit_vector(0),
    )?;
    insert_embedding(
        conn,
        "sym-parse",
        "src/lib.rs",
        "parse_config_loader",
        "demo::parse_config_loader",
        unit_vector(1),
    )?;
    insert_embedding(
        conn,
        "sym-high-a",
        "src/high.rs",
        "alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu",
        "demo::alpha_beta_gamma_delta_epsilon_zeta_eta_theta_iota_kappa_lambda_mu",
        unit_vector(2),
    )?;
    Ok(())
}

fn insert_node(
    conn: &Connection,
    id: &str,
    file_path: &str,
    entity_name: &str,
    qualified_name: &str,
) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO nodes VALUES
            ($1, 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             $2, 0, 1, 1, 1, $3, $4, 'function', $5, NULL)
        ",
        params![
            id,
            file_path,
            entity_name,
            qualified_name,
            format!("anchor-{id}")
        ],
    )
    .with_context(|| format!("insert node {qualified_name}"))?;
    Ok(())
}

fn insert_other_revision_node(conn: &Connection) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO nodes VALUES
            ('sym-old', 'demo', 'registry:crates-io', '0.9.0', 'semver',
             0, 9, 0, 'src/lib.rs', 0, 1, 1, 1, 'parse_config_loader',
             'demo::parse_config_loader', 'function', 'anchor-old', NULL)
        ",
        [],
    )
    .context("insert old revision node")?;
    Ok(())
}

fn insert_doc(
    conn: &Connection,
    section_id: &str,
    file_path: &str,
    title: &str,
    body_text: &str,
) -> Result<()> {
    conn.execute(
        r"
        INSERT INTO section_bodies VALUES
            ($1, 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             $2, $3, $4, $5, 10)
        ",
        params![
            section_id,
            file_path,
            title,
            body_text,
            format!("hash-{section_id}")
        ],
    )
    .with_context(|| format!("insert doc {section_id}"))?;
    Ok(())
}

fn insert_embedding(
    conn: &Connection,
    id: &str,
    file_path: &str,
    entity_name: &str,
    qualified_name: &str,
    vector: Vec<f32>,
) -> Result<()> {
    insert_embedding_with_model(
        conn,
        id,
        file_path,
        entity_name,
        qualified_name,
        vector,
        EMBEDDING_MODEL,
        EMBED_TEXT_VERSION,
    )
}

fn insert_embedding_with_model(
    conn: &Connection,
    id: &str,
    file_path: &str,
    entity_name: &str,
    qualified_name: &str,
    vector: Vec<f32>,
    embedding_model: &str,
    embed_text_version: &str,
) -> Result<()> {
    let sql = format!(
        r"
        INSERT INTO symbol_embeddings VALUES
            ('{id}', 'demo', 'registry:crates-io', '1.0.0', 'semver', 1, 0, 0,
             '{file_path}', '{entity_name}', '{qualified_name}', 'function',
             {}, '{embedding_model}', 'hash-{id}', '{embed_text_version}')
        ",
        vector_sql(&vector)
    );
    conn.execute_batch(&sql)
        .with_context(|| format!("insert embedding {id}"))?;
    Ok(())
}

fn unit_vector(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    vector[index] = 1.0;
    vector
}

fn vector_sql(vector: &[f32]) -> String {
    let values = vector
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]::FLOAT[]")
}
