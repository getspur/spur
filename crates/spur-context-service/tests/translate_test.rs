use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use spur_context_service::knowledge::{
    query_knowledge_context, KnowledgeContextOptions, KnowledgeScope,
};
use spur_context_service::query::read_symbol;
use spur_context_service::translate::{translate_artifact_to_ducklake, TranslateOptions};

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "demo";
const REVISION: &str = "1.2.3";
const DIMENSIONS: usize = 768;

#[test]
fn translates_spur_graph_artifact_into_ducklake_tables() -> Result<()> {
    let root = unique_temp_dir("translate")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;

    let stats = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir: artifact_dir.clone(),
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
    })?;

    assert!(stats.snapshot_id >= 0);
    assert_eq!(stats.rows_inserted.get("nodes"), Some(&1));
    assert_eq!(stats.rows_inserted.get("edges"), Some(&1));
    assert_eq!(stats.rows_inserted.get("edges_unresolved"), Some(&1));
    assert_eq!(stats.rows_inserted.get("files"), Some(&1));
    assert_eq!(stats.rows_inserted.get("file_manifests"), Some(&1));
    assert_eq!(stats.rows_inserted.get("symbol_embeddings"), Some(&1));
    assert_eq!(stats.rows_inserted.get("section_bodies"), Some(&1));

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let (source, package, revision, revision_kind, major, minor, patch): (
        String,
        String,
        String,
        String,
        i32,
        i32,
        i32,
    ) = conn.query_row(
        "SELECT source, package, revision, revision_kind, semver_major, semver_minor, semver_patch FROM nodes",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    assert_eq!(source, SOURCE);
    assert_eq!(package, PACKAGE);
    assert_eq!(revision, REVISION);
    assert_eq!(revision_kind, "semver");
    assert_eq!((major, minor, patch), (1, 2, 3));

    let package_catalog_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_catalog WHERE source = ? AND package = ? AND revision = ? AND index_status = 'complete'",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(package_catalog_count, 1);

    let embeddings_status: String = conn.query_row(
        "SELECT embeddings_status FROM package_catalog WHERE source = ? AND package = ? AND revision = ?",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(embeddings_status, "complete");

    let latest_revision: String = conn.query_row(
        "SELECT revision FROM refs WHERE source = ? AND package = ? AND ref_name = 'latest'",
        params![SOURCE, PACKAGE],
        |row| row.get(0),
    )?;
    assert_eq!(latest_revision, REVISION);

    for table in [
        "nodes",
        "edges",
        "edges_unresolved",
        "files",
        "file_manifests",
        "section_bodies",
        "symbol_embeddings",
    ] {
        assert_eq!(table_row_count(&conn, table)?, 1, "row count for {table}");
    }

    Ok(())
}

#[test]
fn translated_artifact_read_symbol_returns_source_from_package_tree() -> Result<()> {
    let root = unique_temp_dir("translate-read")?;
    let artifact_dir = root.join("artifact");
    let source_root = root.join("source");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(source_root.join("src")).context("create source src dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let source_text = "pub fn alpha() {}\n";
    fs::write(source_root.join("src/lib.rs"), source_text).context("write source file")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;

    translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        source_root: Some(source_root),
        catalog_dsn: catalog_dsn.clone(),
    })?;

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let source = read_symbol(&conn, "pkg:demo@1.2.3::demo::alpha", 0)?
        .context("expected translated alpha source")?;

    assert_eq!(source.file_path, "src/lib.rs");
    assert_eq!(source.line_range, [1, 1]);
    assert_eq!(source.source, source_text);
    Ok(())
}

#[test]
fn translated_artifact_vector_search_returns_ranked_symbol() -> Result<()> {
    let root = unique_temp_dir("translate-vector-search")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;

    translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
    })?;

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let result = query_knowledge_context(
        &conn,
        &KnowledgeContextOptions {
            query: "unmatched lexical query".to_owned(),
            source: SOURCE.to_owned(),
            package: PACKAGE.to_owned(),
            revision: REVISION.to_owned(),
            limit: 3,
            scope: KnowledgeScope::Code,
            query_vec: Some(unit_vector(0)),
        },
    )?;

    let top = result
        .primary_evidence
        .first()
        .context("expected vector evidence from translated artifact")?;
    assert_eq!(
        top.stable_symbol_id.as_deref(),
        Some("pkg:demo@1.2.3::demo::alpha")
    );
    assert_eq!(top.grounding, "hybrid-code");
    assert!(top.score > 0.99, "expected near-identical vector score");
    assert!(result.supporting_docs.is_empty());
    Ok(())
}

fn write_artifact_fixture(artifact_dir: &Path) -> Result<()> {
    fs::create_dir_all(artifact_dir.join("code_symbols.lance"))
        .context("create code symbol sidecar dir")?;
    fs::create_dir_all(artifact_dir.join("sections.lancedb"))
        .context("create sections sidecar dir")?;

    let conn = Connection::open_in_memory().context("open artifact writer duckdb")?;
    let nodes_path = sql_path(&artifact_dir.join("nodes.parquet"));
    let edges_path = sql_path(&artifact_dir.join("edges.parquet"));
    let unresolved_path = sql_path(&artifact_dir.join("edges_unresolved.parquet"));
    let files_path = sql_path(&artifact_dir.join("files.parquet"));
    let manifests_path = sql_path(&artifact_dir.join("file_manifests.parquet"));
    let symbols_path = sql_path(&artifact_dir.join("code_symbols.lance").join("part.parquet"));
    let sections_path = sql_path(&artifact_dir.join("sections.lancedb").join("part.parquet"));

    conn.execute_batch(&format!(
        r#"
        COPY (
            SELECT
                'sym-alpha' AS stable_symbol_id,
                1::BIGINT AS node_id,
                'src/lib.rs' AS file_path,
                0::BIGINT AS byte_range_start,
                18::BIGINT AS byte_range_end,
                1::INTEGER AS line_start,
                1::INTEGER AS line_end,
                'alpha' AS entity_name,
                'demo::alpha' AS qualified_name,
                'function' AS symbol_kind,
                'anchor-alpha' AS anchor_hash,
                NULL::VARCHAR AS enclosing_scope
        ) TO '{nodes_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'sym-alpha' AS source_stable_id,
                'sym-beta' AS target_stable_id,
                1::BIGINT AS src_id,
                2::BIGINT AS dst_id,
                'demo::beta' AS target_label,
                'calls' AS relation,
                'syntax_exact' AS confidence,
                1.0::FLOAT AS confidence_score,
                'calls' AS edge_kind,
                'singleton' AS bind_method,
                NULL::VARCHAR AS import_path,
                NULL::VARCHAR AS receiver_text,
                NULL::VARCHAR AS scope_text
        ) TO '{edges_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'sym-alpha' AS source_stable_id,
                1::BIGINT AS src_id,
                'external::Thing' AS target_label,
                'calls' AS relation,
                'unresolved' AS confidence,
                0.4::FLOAT AS confidence_score,
                'calls' AS edge_kind,
                NULL::VARCHAR AS bind_method,
                'external-crate' AS import_path,
                NULL::VARCHAR AS receiver_text,
                NULL::VARCHAR AS scope_text
        ) TO '{unresolved_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'file-1' AS stable_file_id,
                1::BIGINT AS node_id,
                'src/lib.rs' AS file_path
        ) TO '{files_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'file-1' AS stable_file_id,
                'src/lib.rs' AS path,
                'blob-1' AS content_oid,
                [1::BIGINT] AS node_ids
        ) TO '{manifests_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'sym-alpha' AS stable_symbol_id,
                'src/lib.rs' AS file_path,
                'demo::alpha' AS qualified_name,
                'alpha' AS entity_name,
                'function' AS symbol_kind,
                'pub fn alpha() {{}}' AS embed_text,
                list_transform(range(0, 768), x -> CASE WHEN x = 0 THEN 1.0::FLOAT ELSE 0.0::FLOAT END) AS vector,
                'code-hash' AS content_hash,
                'embed-hash' AS embedding_input_hash,
                'JinaEmbeddingsV2BaseCode' AS embedding_model
        ) TO '{symbols_path}' (FORMAT PARQUET);

        COPY (
            SELECT
                'section-alpha' AS stable_symbol_id,
                'src/lib.rs' AS file_path,
                'demo::alpha' AS qualified_name,
                2::UTINYINT AS heading_level,
                'Alpha documentation body' AS body_text,
                0::UBIGINT AS body_byte_start,
                24::UBIGINT AS body_byte_end,
                0::UINTEGER AS child_count,
                NULL::VARCHAR AS parent_stable_id,
                'section-hash' AS content_hash,
                list_transform(range(0, 768), x -> 0.0::FLOAT) AS vector,
                'section-embed-hash' AS embedding_input_hash,
                'JinaEmbeddingsV2BaseCode' AS embedding_model
        ) TO '{sections_path}' (FORMAT PARQUET);
        "#
    ))
    .context("write parquet artifact fixture")?;

    Ok(())
}

fn initialize_catalog(catalog_dsn: &str, data_path: &str) -> Result<()> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    let sql = include_str!("../sql/init_catalog.sql")
        .replace("INSTALL postgres;", "INSTALL sqlite;")
        .replace("LOAD postgres;", "LOAD sqlite;")
        .replace("__CATALOG_DSN__", &escape_sql_literal(catalog_dsn))
        .replace("s3://spur-context/data/", &escape_sql_literal(data_path));
    conn.execute_batch(&sql).context("execute init catalog sql")
}

fn attach_ducklake(catalog_dsn: &str, data_path: &str) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    conn.execute_batch("INSTALL ducklake; INSTALL sqlite; LOAD ducklake; LOAD sqlite;")
        .context("load ducklake/sqlite extensions")?;
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{}' AS spur_context (DATA_PATH '{}'); USE spur_context;",
        escape_sql_literal(catalog_dsn),
        escape_sql_literal(data_path)
    ))
    .context("attach ducklake")?;
    Ok(conn)
}

fn unique_temp_dir(name: &str) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_nanos();
    path.push(format!(
        "spur-context-service-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).with_context(|| format!("create temp dir {}", path.display()))?;
    Ok(path)
}

fn sql_path(path: &Path) -> String {
    escape_sql_literal(&path.display().to_string())
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn table_row_count(conn: &Connection, table: &str) -> Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .with_context(|| format!("count rows in {table}"))
}

fn unit_vector(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    vector[index] = 1.0;
    vector
}
