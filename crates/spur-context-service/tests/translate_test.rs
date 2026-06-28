use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use spur_context_service::catalog::{
    compact_gold_and_export_snapshot, CatalogResolver, FrozenSnapshotManifest,
    SnapshotCleanupOptions,
};
use spur_context_service::knowledge::{
    query_knowledge_context, KnowledgeContextOptions, KnowledgeScope,
};
use spur_context_service::medallion::{SilverManifest, SilverManifestFile};
use spur_context_service::query::read_symbol;
use spur_context_service::translate::{
    translate_artifact_to_ducklake, TranslateLineage, TranslateOptions,
};
use std::time::Duration;

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "demo";
const REVISION: &str = "1.2.3";
const DIMENSIONS: usize = 768;
const EMBEDDING_MODEL: &str = "EmbeddingGemma300M";
const EMBED_TEXT_VERSION: &str = "v4-embeddinggemma-300m-titled";

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
        artifact_manifest: None,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: false,
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
        "SELECT source, package, revision, revision_kind, semver_major, semver_minor, semver_patch FROM gold.nodes",
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
        "SELECT COUNT(*) FROM gold.package_catalog WHERE source = ? AND package = ? AND revision = ? AND index_status = 'complete'",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(package_catalog_count, 1);

    let embeddings_status: String = conn.query_row(
        "SELECT embeddings_status FROM gold.package_catalog WHERE source = ? AND package = ? AND revision = ?",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(embeddings_status, "complete");

    let latest_revision: String = conn.query_row(
        "SELECT revision FROM gold.refs WHERE source = ? AND package = ? AND ref_name = 'latest'",
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
        assert_eq!(
            table_row_count(&conn, &format!("gold.{table}"))?,
            1,
            "row count for gold.{table}"
        );
    }

    let (section_vector_len, section_model, section_input_hash, section_text_version): (
        i64,
        String,
        String,
        String,
    ) = conn.query_row(
        r"
        SELECT array_length(vector), embedding_model, embedding_input_hash, embed_text_version
        FROM gold.section_bodies
        ",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!(section_vector_len, DIMENSIONS as i64);
    assert_eq!(section_model, EMBEDDING_MODEL);
    assert_eq!(section_input_hash, "section-embed-hash");
    assert_eq!(section_text_version, EMBED_TEXT_VERSION);

    Ok(())
}

#[test]
fn translate_publishes_schema_qualified_gold_generation_with_lineage_and_snapshot() -> Result<()> {
    let root = unique_temp_dir("translate-gold-generation")?;
    let artifact_dir = root.join("artifact");
    let source_root = root.join("source");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(source_root.join("src")).context("create source src dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;
    fs::write(source_root.join("src/lib.rs"), "pub fn alpha() {}\n")
        .context("write source file")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;

    let stats = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: Some(source_root),
        catalog_dsn: catalog_dsn.clone(),
        lineage: Some(translate_lineage()),
        allow_missing_embeddings: false,
    })?;

    assert!(stats.snapshot_id >= 0);

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    assert_eq!(table_row_count(&conn, "nodes")?, 0);
    assert_eq!(table_row_count(&conn, "package_catalog")?, 0);
    assert_eq!(table_row_count(&conn, "gold.nodes")?, 1);
    assert_eq!(table_row_count(&conn, "gold.symbol_embeddings")?, 1);
    assert_eq!(table_row_count(&conn, "gold.section_bodies")?, 1);

    let (
        generation,
        bronze_content_sha256,
        silver_graph_content_hash,
        builder_version,
        translate_schema_version,
    ): (i64, String, String, String, String) = conn.query_row(
        r"
        SELECT
            generation,
            bronze_content_sha256,
            silver_graph_content_hash,
            builder_version,
            translate_schema_version
        FROM gold.package_catalog
        WHERE source = ? AND package = ? AND revision = ?
        ",
        params![SOURCE, PACKAGE, REVISION],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    assert!(generation > 0);
    assert_eq!(bronze_content_sha256, "bronze-sha256");
    assert_eq!(silver_graph_content_hash, "graph-hash-123");
    assert_eq!(builder_version, "builder-v1");
    assert_eq!(translate_schema_version, "translate-v1");

    for table in [
        "nodes",
        "edges",
        "edges_unresolved",
        "files",
        "file_manifests",
        "section_bodies",
        "symbol_embeddings",
    ] {
        let generation_count: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM gold.{table} WHERE source = ? AND package = ? AND revision = ? AND generation = ?"
            ),
            params![SOURCE, PACKAGE, REVISION, generation],
            |row| row.get(0),
        )?;
        assert_eq!(
            generation_count, 1,
            "gold.{table} must be stamped with generation"
        );
    }

    let snapshot_path = catalog_snapshot_path(&data_path)?;
    assert!(
        snapshot_path.is_file(),
        "translate must export frozen snapshot to {}",
        snapshot_path.display()
    );

    let snapshot_resolver =
        CatalogResolver::new_with_data_path(&snapshot_path.display().to_string(), &data_path)?;
    let resolved = snapshot_resolver.resolve(SOURCE, PACKAGE, REVISION)?;
    assert_eq!(resolved.revision, REVISION);

    let source = read_symbol(
        snapshot_resolver.connection(),
        "pkg:demo@1.2.3::demo::alpha",
        0,
    )?
    .context("snapshot should serve translated symbol")?;
    assert_eq!(source.file_path, "src/lib.rs");
    Ok(())
}

#[test]
fn failed_republish_leaves_previous_gold_generation_readable() -> Result<()> {
    let root = unique_temp_dir("translate-no-half-publish")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    seed_published_gold_symbol(&conn, 41)?;
    drop(conn);

    write_artifact_fixture(&artifact_dir)?;
    write_invalid_symbol_sidecar_without_embedding(
        &artifact_dir.join("code_symbols.lance").join("part.parquet"),
    )?;

    let error = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: Some(translate_lineage()),
        allow_missing_embeddings: false,
    })
    .expect_err("invalid sidecar must reject the new generation");

    assert!(
        format!("{error:#}").contains("missing vector/embedding"),
        "unexpected error: {error:#}"
    );

    let resolver = CatalogResolver::new_with_data_path(&catalog_dsn, &data_path)?;
    let resolved = resolver.resolve(SOURCE, PACKAGE, REVISION)?;
    assert_eq!(resolved.snapshot_id, 4100);
    let source = read_symbol(resolver.connection(), "pkg:demo@1.2.3::demo::alpha", 0)?
        .context("previous generation should remain readable")?;
    assert_eq!(source.source, "pub fn old_alpha() {}\n");
    Ok(())
}

#[test]
fn exported_snapshot_serves_after_compaction_cleanup_fence_cycle() -> Result<()> {
    let root = unique_temp_dir("translate-snapshot-cleanup")?;
    let artifact_dir = root.join("artifact");
    let source_root = root.join("source");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(source_root.join("src")).context("create source src dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;
    fs::write(source_root.join("src/lib.rs"), "pub fn alpha() {}\n")
        .context("write source file")?;

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
        artifact_manifest: None,
        source_root: Some(source_root),
        catalog_dsn: catalog_dsn.clone(),
        lineage: Some(translate_lineage()),
        allow_missing_embeddings: false,
    })?;

    compact_gold_and_export_snapshot(
        &catalog_dsn,
        &data_path,
        SnapshotCleanupOptions {
            older_than: Duration::from_secs(3600),
            republish_lag: Duration::from_secs(300),
        },
    )?;

    let snapshot_path = catalog_snapshot_path(&data_path)?;
    let snapshot_resolver =
        CatalogResolver::new_with_data_path(&snapshot_path.display().to_string(), &data_path)?;
    let resolved = snapshot_resolver.resolve_latest(SOURCE, PACKAGE)?;
    assert_eq!(resolved.revision, REVISION);
    let source = read_symbol(
        snapshot_resolver.connection(),
        "pkg:demo@1.2.3::demo::alpha",
        0,
    )?
    .context("snapshot should still serve after cleanup fence")?;
    assert_eq!(source.file_path, "src/lib.rs");
    Ok(())
}

#[test]
fn translates_artifact_without_symbol_vectors_as_bm25_only() -> Result<()> {
    let root = unique_temp_dir("translate-no-embeddings")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture_with_symbol_vector(&artifact_dir, "CAST(NULL AS FLOAT[])")?;

    let stats = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: true,
    })?;

    assert_eq!(stats.rows_inserted.get("nodes"), Some(&1));
    assert_eq!(stats.rows_inserted.get("symbol_embeddings"), Some(&0));
    assert_eq!(stats.rows_inserted.get("section_bodies"), Some(&1));

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let embeddings_status: String = conn.query_row(
        "SELECT embeddings_status FROM gold.package_catalog WHERE source = ? AND package = ? AND revision = ?",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(embeddings_status, "skipped");
    assert_eq!(table_row_count(&conn, "gold.symbol_embeddings")?, 0);

    let result = query_knowledge_context(
        &conn,
        &KnowledgeContextOptions {
            query: "alpha".to_owned(),
            source: SOURCE.to_owned(),
            package: PACKAGE.to_owned(),
            revision: REVISION.to_owned(),
            scope: KnowledgeScope::Code,
            limit: 3,
            query_vec: None,
        },
    )?;
    assert!(
        result
            .primary_evidence
            .iter()
            .any(|item| item.grounding == "bm25-code"),
        "expected BM25 code evidence without embeddings"
    );
    Ok(())
}

#[test]
fn expected_embeddings_fail_when_symbol_vectors_land_zero_rows() -> Result<()> {
    let root = unique_temp_dir("translate-expected-embeddings-zero")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture_with_symbol_vector(&artifact_dir, "CAST(NULL AS FLOAT[])")?;

    let error = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: None,
        catalog_dsn,
        lineage: None,
        allow_missing_embeddings: false,
    })
    .expect_err("expected embeddings should reject zero symbol embedding rows");

    assert!(
        format!("{error:#}").contains("expected symbol embeddings"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn expected_embeddings_fail_when_lance_sidecars_are_missing() -> Result<()> {
    let root = unique_temp_dir("translate-expected-embeddings-missing")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;
    fs::remove_file(artifact_dir.join("code_symbols.lance").join("part.parquet"))
        .context("remove code symbol sidecar parquet")?;
    fs::remove_file(artifact_dir.join("sections.lancedb").join("part.parquet"))
        .context("remove section sidecar parquet")?;

    let error = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: None,
        catalog_dsn,
        lineage: None,
        allow_missing_embeddings: false,
    })
    .expect_err("expected embeddings should reject missing Lance sidecars");

    assert!(
        format!("{error:#}").contains("expected symbol embeddings"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn skipped_lance_sidecars_do_not_poison_required_graph_commits() -> Result<()> {
    let root = unique_temp_dir("translate-skipped-lance-sidecars")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;
    fs::remove_file(artifact_dir.join("code_symbols.lance").join("part.parquet"))
        .context("remove code symbol sidecar parquet")?;
    fs::remove_file(artifact_dir.join("sections.lancedb").join("part.parquet"))
        .context("remove section sidecar parquet")?;

    let stats = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: None,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: true,
    })?;

    assert_eq!(stats.rows_inserted.get("nodes"), Some(&1));
    assert_eq!(stats.rows_inserted.get("symbol_embeddings"), Some(&0));
    assert_eq!(stats.rows_inserted.get("section_bodies"), Some(&0));

    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let durable_nodes: i64 = conn.query_row(
        "SELECT COUNT(*) FROM gold.nodes WHERE source = ? AND package = ? AND revision = ?",
        params![SOURCE, PACKAGE, REVISION],
        |row| row.get(0),
    )?;
    assert_eq!(durable_nodes, 1);
    Ok(())
}

#[test]
fn manifest_translate_ignores_unlisted_sidecar_files() -> Result<()> {
    let root = unique_temp_dir("translate-manifest-sidecars")?;
    let artifact_dir = root.join("artifact");
    let data_path = root.join("data");
    fs::create_dir_all(&artifact_dir).context("create artifact dir")?;
    fs::create_dir_all(&data_path).context("create ducklake data dir")?;

    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = data_path.display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;
    write_artifact_fixture(&artifact_dir)?;
    write_unlisted_symbol_sidecar_row(&artifact_dir.join("code_symbols.lance/stale.parquet"))?;
    let manifest = silver_manifest_for_fixture();

    let stats = translate_artifact_to_ducklake(&TranslateOptions {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: REVISION.to_owned(),
        revision_kind: "semver".to_owned(),
        artifact_dir,
        artifact_manifest: Some(manifest),
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: false,
    })?;

    assert_eq!(stats.rows_inserted.get("symbol_embeddings"), Some(&1));
    let conn = attach_ducklake(&catalog_dsn, &data_path)?;
    let embedded_symbols: Vec<String> = collect_strings(
        &conn,
        "SELECT stable_symbol_id FROM gold.symbol_embeddings ORDER BY stable_symbol_id",
    )?;
    assert_eq!(embedded_symbols, ["sym-alpha"]);
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
        artifact_manifest: None,
        source_root: Some(source_root),
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: false,
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
        artifact_manifest: None,
        source_root: None,
        catalog_dsn: catalog_dsn.clone(),
        lineage: None,
        allow_missing_embeddings: false,
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
    write_artifact_fixture_with_symbol_vector(
        artifact_dir,
        "list_transform(range(0, 768), x -> CASE WHEN x = 0 THEN 1.0::FLOAT ELSE 0.0::FLOAT END)",
    )
}

fn write_artifact_fixture_with_symbol_vector(
    artifact_dir: &Path,
    symbol_vector_expr: &str,
) -> Result<()> {
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
                {symbol_vector_expr} AS vector,
                'code-hash' AS content_hash,
                'embed-hash' AS embedding_input_hash,
                '{EMBEDDING_MODEL}' AS embedding_model
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
                '{EMBEDDING_MODEL}' AS embedding_model
        ) TO '{sections_path}' (FORMAT PARQUET);
        "#
    ))
    .context("write parquet artifact fixture")?;

    Ok(())
}

fn write_unlisted_symbol_sidecar_row(path: &Path) -> Result<()> {
    let conn = Connection::open_in_memory().context("open stale sidecar writer duckdb")?;
    let symbols_path = sql_path(path);
    conn.execute_batch(&format!(
        r#"
        COPY (
            SELECT
                'sym-stale' AS stable_symbol_id,
                'src/stale.rs' AS file_path,
                'demo::stale' AS qualified_name,
                'stale' AS entity_name,
                'function' AS symbol_kind,
                'pub fn stale() {{}}' AS embed_text,
                list_transform(range(0, 768), x -> 0.5::FLOAT) AS vector,
                'stale-code-hash' AS content_hash,
                'stale-embed-hash' AS embedding_input_hash,
                '{EMBEDDING_MODEL}' AS embedding_model
        ) TO '{symbols_path}' (FORMAT PARQUET);
        "#
    ))
    .context("write unlisted symbol sidecar row")
}

fn write_invalid_symbol_sidecar_without_embedding(path: &Path) -> Result<()> {
    let conn = Connection::open_in_memory().context("open invalid sidecar writer duckdb")?;
    let symbols_path = sql_path(path);
    conn.execute_batch(&format!(
        r#"
        COPY (
            SELECT
                'sym-alpha' AS stable_symbol_id,
                'src/lib.rs' AS file_path,
                'demo::alpha' AS qualified_name,
                'alpha' AS entity_name,
                'function' AS symbol_kind,
                'pub fn alpha() {{}}' AS embed_text
        ) TO '{symbols_path}' (FORMAT PARQUET);
        "#
    ))
    .context("write invalid symbol sidecar row")
}

fn seed_published_gold_symbol(conn: &Connection, generation: i64) -> Result<()> {
    conn.execute_batch(&format!(
        r#"
        INSERT INTO gold.nodes (
            stable_symbol_id, package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            file_path, byte_range_start, byte_range_end,
            line_start, line_end, entity_name, qualified_name,
            symbol_kind, anchor_hash, enclosing_scope, generation
        )
        VALUES (
            'sym-alpha', '{PACKAGE}', '{SOURCE}', '{REVISION}', 'semver',
            1, 2, 3,
            'src/lib.rs', 0, 22,
            1, 1, 'alpha', 'demo::alpha',
            'function', 'old-anchor', NULL, {generation}
        );

        INSERT INTO gold.files (
            stable_file_id, file_path, source_text,
            package, source, revision, revision_kind,
            semver_major, semver_minor, semver_patch, generation
        )
        VALUES (
            'file-old', 'src/lib.rs', 'pub fn old_alpha() {{}}
',
            '{PACKAGE}', '{SOURCE}', '{REVISION}', 'semver',
            1, 2, 3, {generation}
        );

        INSERT INTO gold.package_catalog (
            source, package, revision, revision_kind,
            semver_major, semver_minor, semver_patch,
            snapshot_id, indexed_at, index_status, embeddings_status, row_counts,
            generation, bronze_content_sha256, silver_graph_content_hash,
            builder_version, translate_schema_version
        )
        VALUES (
            '{SOURCE}', '{PACKAGE}', '{REVISION}', 'semver',
            1, 2, 3,
            4100, CURRENT_TIMESTAMP, 'complete', 'skipped', CAST('{{"nodes":1}}' AS JSON),
            {generation}, 'old-bronze', 'old-graph', 'old-builder', 'old-translate'
        );

        INSERT INTO gold.refs (source, package, ref_name, revision, updated_at)
        VALUES ('{SOURCE}', '{PACKAGE}', 'latest', '{REVISION}', CURRENT_TIMESTAMP);
        "#
    ))
    .context("seed published gold generation")
}

fn translate_lineage() -> TranslateLineage {
    TranslateLineage {
        bronze_content_sha256: "bronze-sha256".to_owned(),
        silver_graph_content_hash: "graph-hash-123".to_owned(),
        builder_version: "builder-v1".to_owned(),
        translate_schema_version: "translate-v1".to_owned(),
        embed_text_version: EMBED_TEXT_VERSION.to_owned(),
    }
}

fn catalog_snapshot_path(data_path: &str) -> Result<PathBuf> {
    let pointer_path = PathBuf::from(data_path)
        .join("gold")
        .join("catalog-snapshot")
        .join("current.json");
    let pointer: FrozenSnapshotManifest = serde_json::from_slice(
        &fs::read(&pointer_path)
            .with_context(|| format!("read snapshot pointer {}", pointer_path.display()))?,
    )
    .context("parse snapshot pointer")?;
    Ok(PathBuf::from(pointer.snapshot_uri))
}

fn silver_manifest_for_fixture() -> SilverManifest {
    SilverManifest {
        schema_hash: "sha256:test-schema".to_owned(),
        files: [
            "nodes.parquet",
            "edges.parquet",
            "edges_unresolved.parquet",
            "files.parquet",
            "file_manifests.parquet",
            "code_symbols.lance/part.parquet",
            "sections.lancedb/part.parquet",
        ]
        .into_iter()
        .map(|path| SilverManifestFile {
            path: path.to_owned(),
            size_bytes: 1,
            etag: format!("\"{path}\""),
        })
        .collect(),
    }
}

fn initialize_catalog(catalog_dsn: &str, data_path: &str) -> Result<()> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    conn.execute_batch("INSTALL ducklake; INSTALL sqlite; LOAD ducklake; LOAD sqlite;")
        .context("load ducklake/sqlite extensions")?;
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{}' AS spur_context (DATA_PATH '{}'); USE spur_context;",
        escape_sql_literal(catalog_dsn),
        escape_sql_literal(data_path)
    ))
    .context("attach ducklake")?;
    conn.execute_batch(include_str!("../sql/catalog_tables.sql"))
        .context("execute catalog tables sql")
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

fn collect_strings(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql).context("prepare string collection")?;
    stmt.query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("collect strings")
}

fn unit_vector(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    vector[index] = 1.0;
    vector
}
