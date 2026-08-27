use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use spur_context_service::catalog::{
    compact_gold_and_export_snapshot, publish_generation, CatalogResolver, GenerationPublication,
    PointerPrecondition, PublicationObject, PublicationStore, PublicationStoreError,
    SnapshotCleanupOptions,
};
use spur_context_service::serving_registry::ServingRegistry;

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "serde";

#[test]
fn catalog_tables_sql_creates_medallion_schemas_and_gold_catalog_columns() -> Result<()> {
    let root = unique_temp_dir("catalog-tables-sql")?;
    fs::create_dir_all(root.join("data")).context("create ducklake data dir")?;
    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = root.join("data").display().to_string();

    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    attach_ducklake(&conn, &catalog_dsn, &data_path)?;
    conn.execute_batch(include_str!("../sql/catalog_tables.sql"))
        .context("execute catalog tables sql")?;

    assert_eq!(
        columns_for(&conn, "gold", "nodes")?,
        [
            "stable_symbol_id",
            "package",
            "source",
            "revision",
            "revision_kind",
            "semver_major",
            "semver_minor",
            "semver_patch",
            "file_path",
            "byte_range_start",
            "byte_range_end",
            "line_start",
            "line_end",
            "entity_name",
            "qualified_name",
            "symbol_kind",
            "anchor_hash",
            "enclosing_scope",
            "generation",
        ]
    );

    assert_eq!(table_exists(&conn, "bronze", "raw_sources")?, 1);
    assert_eq!(table_exists(&conn, "silver", "graph_artifacts")?, 1);
    assert_eq!(table_exists(&conn, "gold", "package_catalog")?, 1);

    assert_eq!(
        columns_for(&conn, "bronze", "raw_sources")?,
        [
            "source",
            "package",
            "version",
            "revision_kind",
            "semver_major",
            "semver_minor",
            "semver_patch",
            "source_kind",
            "source_url",
            "s3_uri",
            "content_sha256",
            "bytes",
            "fetched_at",
            "fetch_status",
        ]
    );

    for column in [
        "generation",
        "bronze_content_sha256",
        "silver_graph_content_hash",
        "builder_version",
        "translate_schema_version",
        "embed_text_version",
    ] {
        assert_eq!(
            nullable_column_count(&conn, "gold", "package_catalog", column)?,
            1,
            "gold.package_catalog must have nullable column {column}"
        );
    }

    for schema in ["main", "gold"] {
        for column in [
            "graph_manifest_uri",
            "graph_manifest_sha256",
            "graph_manifest_bytes",
            "source_sidecar_uri",
            "source_sidecar_sha256",
            "source_sidecar_bytes",
        ] {
            assert_eq!(
                nullable_column_count(&conn, schema, "package_catalog", column)?,
                1,
                "{schema}.package_catalog must have nullable serving-artifact column {column}"
            );
        }
    }

    for table in [
        "nodes",
        "edges",
        "edges_unresolved",
        "files",
        "file_manifests",
        "section_bodies",
        "symbol_embeddings",
        "commits",
        "symbol_snapshots",
        "temporal_edges",
    ] {
        assert_eq!(
            nullable_column_count(&conn, "gold", table, "generation")?,
            1,
            "gold.{table} must carry generation for immutable publish builds"
        );
    }
    Ok(())
}

#[test]
fn snapshot_cleanup_fence_rejects_windows_shorter_than_republish_lag() -> Result<()> {
    let root = unique_temp_dir("cleanup-fence")?;
    fs::create_dir_all(root.join("data")).context("create ducklake data dir")?;
    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = root.join("data").display().to_string();
    initialize_catalog(&catalog_dsn, &data_path)?;

    let err = compact_gold_and_export_snapshot(
        &catalog_dsn,
        &data_path,
        SnapshotCleanupOptions {
            older_than: Duration::from_secs(60),
            republish_lag: Duration::from_secs(300),
        },
    )
    .expect_err("cleanup older_than shorter than republish lag must be rejected");

    assert!(
        format!("{err:#}").contains("older_than must be >= republish_lag"),
        "unexpected error: {err:#}"
    );
    Ok(())
}

#[test]
fn duplicate_init_catalog_sql_is_removed_to_prevent_schema_drift() {
    assert!(
        !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("sql/init_catalog.sql")
            .exists(),
        "catalog DDL must live only in sql/catalog_tables.sql"
    );
}

#[test]
fn resolves_exact_revision() -> Result<()> {
    let fixture = CatalogFixture::new("exact")?;

    let resolved = fixture
        .resolver
        .resolve(SOURCE, PACKAGE, "1.0.193")
        .context("resolve exact revision")?;

    assert_eq!(resolved.source, SOURCE);
    assert_eq!(resolved.package, PACKAGE);
    assert_eq!(resolved.revision, "1.0.193");
    assert_eq!(resolved.revision_kind, "semver");
    assert_eq!(resolved.snapshot_id, 193);
    Ok(())
}

#[test]
fn resolves_ref_name_to_revision() -> Result<()> {
    let fixture = CatalogFixture::new("ref")?;

    let resolved = fixture
        .resolver
        .resolve(SOURCE, PACKAGE, "latest")
        .context("resolve latest ref")?;

    assert_eq!(resolved.revision, "1.0.194");
    assert_eq!(resolved.snapshot_id, 194);
    Ok(())
}

#[test]
fn resolve_latest_uses_latest_ref() -> Result<()> {
    let fixture = CatalogFixture::new("latest")?;

    let resolved = fixture
        .resolver
        .resolve_latest(SOURCE, PACKAGE)
        .context("resolve latest")?;

    assert_eq!(resolved.revision, "1.0.194");
    Ok(())
}

#[test]
fn lists_revisions_in_semver_order() -> Result<()> {
    let fixture = CatalogFixture::new("list")?;

    let revisions = fixture
        .resolver
        .list_revisions(SOURCE, PACKAGE)
        .context("list revisions")?;

    let names: Vec<_> = revisions
        .iter()
        .map(|revision| revision.revision.as_str())
        .collect();
    assert_eq!(names, ["1.0.194", "1.0.193"]);
    assert_eq!(revisions[0].semver_major, Some(1));
    assert_eq!(revisions[0].index_status, "complete");
    assert_eq!(revisions[0].embeddings_status, "complete");
    Ok(())
}

#[test]
fn missing_revision_reports_not_found() -> Result<()> {
    let fixture = CatalogFixture::new("missing")?;

    let error = fixture
        .resolver
        .resolve(SOURCE, PACKAGE, "0.9.0")
        .expect_err("unknown revision should be missing");

    assert!(
        format!("{error:#}").contains("revision not found"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn catalog_resolver_does_not_create_memory_index_jobs() -> Result<()> {
    let root = unique_temp_dir("index-jobs")?;
    fs::create_dir_all(root.join("data")).context("create ducklake data dir")?;

    let catalog_path = root.join("catalog.sqlite");
    let data_path = root.join("data");
    let catalog_dsn = format!("sqlite:{}", catalog_path.display());
    let data_path = data_path.display().to_string();

    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    attach_ducklake(&conn, &catalog_dsn, &data_path)?;

    let resolver = CatalogResolver::from_connection(conn);

    let error = resolver
        .connection()
        .query_row("SELECT COUNT(*) FROM memory.index_jobs", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect_err("catalog initialization must not create memory.index_jobs");
    assert!(
        error.to_string().contains("index_jobs") || error.to_string().contains("does not exist"),
        "unexpected error after querying absent memory.index_jobs: {error}"
    );
    Ok(())
}

const GRAPH_MANIFEST_URI: &str = "s3://silver-test/silver/serde/1.0.194/builder-v1/manifest.json";
const SOURCE_SIDECAR_URI: &str =
    "s3://silver-test/silver/serde/1.0.194/builder-v1/source_files.parquet";
const OLDER_GRAPH_MANIFEST_URI: &str =
    "s3://silver-test/silver/serde_json/1.0.120/builder-v1/manifest.json";
const OLDER_SOURCE_SIDECAR_URI: &str =
    "s3://silver-test/silver/serde_json/1.0.120/builder-v1/source_files.parquet";
const SNAPSHOT_URI: &str =
    "s3://catalog-test/gold/catalog-snapshot/generations/00000000000000000007/spur_context.ducklake";
const SNAPSHOT_MANIFEST_URI: &str =
    "s3://catalog-test/gold/catalog-snapshot/generations/00000000000000000007/manifest.json";
const SERVING_REGISTRY_URI: &str =
    "s3://catalog-test/gold/catalog-snapshot/generations/00000000000000000007/serving-registry.json";
const POINTER_URI: &str = "s3://catalog-test/gold/catalog-snapshot/current.json";
const GRAPH_MANIFEST_BYTES: &[u8] = br#"{"schema_version":1,"files":[]}"#;
const SOURCE_SIDECAR_BYTES: &[u8] = b"PAR1 source sidecar fixture";
const OLDER_GRAPH_MANIFEST_BYTES: &[u8] = br#"{"schema_version":1,"files":["older"]}"#;
const OLDER_SOURCE_SIDECAR_BYTES: &[u8] = b"PAR1 older source sidecar fixture";
const SNAPSHOT_BYTES: &[u8] = b"DuckLake snapshot fixture";

#[test]
fn incomplete_registry_does_not_advance_pointer() -> Result<()> {
    let mut row = complete_publication_row(7);
    row.index_status = "building".to_owned();
    let conn = publication_catalog(&[row])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("an incomplete exact generation must not publish");

    assert_eq!(error.code(), "incomplete_serving_generation");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn missing_lineage_on_unchanged_older_package_does_not_advance_pointer() -> Result<()> {
    let mut older = older_complete_publication_row(6);
    older.graph_manifest_uri = None;
    let conn = publication_catalog(&[older, complete_publication_row(7)])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("missing lineage on any current package must reject the global view");

    assert_eq!(error.code(), "incomplete_serving_generation");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn null_generation_on_current_package_does_not_advance_pointer() -> Result<()> {
    let mut legacy = older_complete_publication_row(6);
    legacy.generation = None;
    let conn = publication_catalog(&[legacy, complete_publication_row(7)])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a current package with NULL generation must not be skipped");

    assert_eq!(error.code(), "incomplete_serving_generation");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn publication_generation_must_equal_frozen_catalog_maximum() -> Result<()> {
    let conn = publication_catalog(&[
        older_complete_publication_row(6),
        complete_publication_row(7),
    ])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 5, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(6))
        .expect_err("a publication below the catalog MAX generation must be rejected");

    assert_eq!(error.code(), "catalog_generation_mismatch");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn missing_source_sidecar_does_not_advance_pointer() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = FakeS3::default();
    store.seed(GRAPH_MANIFEST_URI, GRAPH_MANIFEST_BYTES);
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a missing source sidecar must not publish");

    assert_eq!(error.code(), "missing_source_sidecar");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn artifact_hash_mismatch_does_not_advance_pointer() -> Result<()> {
    let mut row = complete_publication_row(7);
    row.source_sidecar_sha256 = Some("0".repeat(64));
    let conn = publication_catalog(&[row])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a mismatched source-sidecar hash must not publish");

    assert_eq!(error.code(), "artifact_hash_mismatch");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn artifact_byte_mismatch_does_not_advance_pointer() -> Result<()> {
    let mut row = complete_publication_row(7);
    row.source_sidecar_bytes = Some(SOURCE_SIDECAR_BYTES.len() as i64 + 1);
    let conn = publication_catalog(&[row])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a mismatched source-sidecar byte length must not publish");

    assert_eq!(error.code(), "artifact_byte_mismatch");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn stale_older_generation_does_not_advance_pointer() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();
    let previous = seed_pointer(
        &store,
        8,
        "s3://catalog-test/gold/catalog-snapshot/generations/00000000000000000008/spur_context.ducklake",
        "s3://catalog-test/gold/catalog-snapshot/generations/00000000000000000008/serving-registry.json",
    );

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("an older generation must not replace the live pointer");

    assert_eq!(error.code(), "stale_generation");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn same_generation_conflict_does_not_advance_pointer() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();
    let previous = seed_pointer(
        &store,
        7,
        "s3://catalog-test/conflicting/spur_context.ducklake",
        "s3://catalog-test/conflicting/serving-registry.json",
    );

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a conflicting writer for the same generation must be rejected");

    assert_eq!(error.code(), "same_generation_conflict");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 0);
    Ok(())
}

#[test]
fn stale_expected_pointer_does_not_advance_pointer() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();
    let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);
    store.fail_next_conditional_write();

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("a stale observed pointer must fail its conditional write");

    assert_eq!(error.code(), "stale_pointer");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert_eq!(store.pointer_write_count(), 1);
    Ok(())
}

#[test]
fn exact_complete_generation_publishes_once_in_order() -> Result<()> {
    let conn = publication_catalog(&[
        older_complete_publication_row(6),
        complete_publication_row(7),
    ])?;
    let store = complete_publication_store();

    publish_generation(&store, &conn, publication_request(7))?;
    publish_generation(&store, &conn, publication_request(7))?;

    assert_eq!(
        store.write_uris(),
        [
            SNAPSHOT_URI,
            SNAPSHOT_MANIFEST_URI,
            SERVING_REGISTRY_URI,
            POINTER_URI,
        ]
    );
    assert_eq!(store.pointer_write_count(), 1);

    let registry: ServingRegistry = serde_json::from_slice(
        &store
            .object(SERVING_REGISTRY_URI)
            .context("serving registry was not written")?
            .bytes,
    )?;
    assert_eq!(registry.generation, 7);
    assert_eq!(registry.packages.len(), 2);
    assert!(registry
        .packages
        .iter()
        .all(|package| package.generation == 7));
    assert_eq!(
        registry
            .packages
            .iter()
            .map(|package| package.package.as_str())
            .collect::<Vec<_>>(),
        ["serde", "serde_json"]
    );
    Ok(())
}

#[test]
fn live_pointer_payload_contains_snapshot_and_registry_strong_refs() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();

    let manifest = publish_generation(&store, &conn, publication_request(7))?;

    let pointer = store.object(POINTER_URI).context("live pointer missing")?;
    let payload: serde_json::Value = serde_json::from_slice(&pointer.bytes)?;
    let registry = store
        .object(SERVING_REGISTRY_URI)
        .context("serving registry missing")?;
    assert_eq!(payload["snapshot_uri"], SNAPSHOT_URI);
    assert_eq!(payload["sha256"], sha256_bytes(SNAPSHOT_BYTES));
    assert_eq!(payload["bytes"], SNAPSHOT_BYTES.len() as u64);
    assert_eq!(payload["serving_registry_uri"], SERVING_REGISTRY_URI);
    assert_eq!(
        payload["serving_registry_sha256"],
        sha256_bytes(&registry.bytes)
    );
    assert_eq!(
        payload["serving_registry_bytes"],
        registry.bytes.len() as u64
    );
    assert_eq!(
        manifest.serving_registry_uri,
        Some(SERVING_REGISTRY_URI.to_owned())
    );
    Ok(())
}

#[test]
fn pointer_write_uses_observed_etag_precondition() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();
    seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);
    let observed = store.object(POINTER_URI).context("seed pointer missing")?;

    publish_generation(&store, &conn, publication_request(7))?;

    let preconditions = store.pointer_preconditions();
    assert_eq!(preconditions.len(), 1);
    match &preconditions[0] {
        PointerPrecondition::Matches { etag, .. } => assert_eq!(etag, &observed.etag),
        PointerPrecondition::Absent => panic!("an observed pointer requires If-Match"),
    }
    Ok(())
}

#[test]
fn same_generation_with_different_data_path_is_a_conflict() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();
    publish_generation(&store, &conn, publication_request(7))?;

    let mut pointer = store.object(POINTER_URI).context("live pointer missing")?;
    let mut payload: serde_json::Value = serde_json::from_slice(&pointer.bytes)?;
    payload["data_path"] = json!("s3://catalog-test/conflicting/gold/data");
    pointer.bytes = serde_json::to_vec_pretty(&payload)?;
    store.seed(POINTER_URI, &pointer.bytes);
    let previous = store.object(POINTER_URI).unwrap().bytes;
    store.clear_events();

    let error = publish_generation(&store, &conn, publication_request(7))
        .expect_err("data_path changes serving behavior and must conflict");

    assert_eq!(error.code(), "same_generation_conflict");
    assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    assert!(store.write_uris().is_empty());
    Ok(())
}

#[test]
fn idempotent_publication_rejects_each_missing_immutable_output() -> Result<()> {
    for uri in [SNAPSHOT_URI, SNAPSHOT_MANIFEST_URI, SERVING_REGISTRY_URI] {
        let conn = publication_catalog(&[complete_publication_row(7)])?;
        let store = complete_publication_store();
        publish_generation(&store, &conn, publication_request(7))?;
        let previous = store
            .object(POINTER_URI)
            .context("live pointer missing")?
            .bytes;
        store.remove(uri);
        store.clear_events();

        let error = publish_generation(&store, &conn, publication_request(7))
            .expect_err("idempotence must verify every referenced immutable output exists");

        assert_eq!(error.code(), "missing_immutable_output", "URI {uri}");
        assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
        assert!(store.write_uris().is_empty());
    }
    Ok(())
}

#[test]
fn idempotent_publication_rejects_each_corrupt_immutable_output() -> Result<()> {
    for uri in [SNAPSHOT_URI, SNAPSHOT_MANIFEST_URI, SERVING_REGISTRY_URI] {
        let conn = publication_catalog(&[complete_publication_row(7)])?;
        let store = complete_publication_store();
        publish_generation(&store, &conn, publication_request(7))?;
        let previous = store
            .object(POINTER_URI)
            .context("live pointer missing")?
            .bytes;
        store.seed(uri, b"corrupt immutable output");
        store.clear_events();

        let error = publish_generation(&store, &conn, publication_request(7))
            .expect_err("idempotence must verify every referenced immutable output exactly");

        assert_eq!(error.code(), "immutable_output_mismatch", "URI {uri}");
        assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
        assert!(store.write_uris().is_empty());
    }
    Ok(())
}

#[test]
fn missing_live_pointer_is_the_absent_s3_precondition() -> Result<()> {
    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let store = complete_publication_store();

    publish_generation(&store, &conn, publication_request(7))?;

    assert_eq!(store.pointer_preconditions(), [PointerPrecondition::Absent]);
    Ok(())
}

#[test]
fn same_etag_with_different_version_satisfies_pointer_cas() -> Result<()> {
    let store = FakeS3::default();
    let observed = store.seed(POINTER_URI, b"old pointer");
    let precondition = PointerPrecondition::Matches {
        etag: observed.etag,
    };
    store.set_version_id(POINTER_URI, "diagnostic-version-changed");

    store.compare_and_swap_pointer(
        POINTER_URI,
        b"new pointer",
        "application/json",
        &precondition,
    )?;

    assert_eq!(store.object(POINTER_URI).unwrap().bytes, b"new pointer");
    Ok(())
}

#[test]
fn conditional_pointer_404_409_and_412_are_stale_outcomes() -> Result<()> {
    for status in [404, 409, 412] {
        let conn = publication_catalog(&[complete_publication_row(7)])?;
        let store = complete_publication_store();
        let previous = seed_pointer(&store, 6, SNAPSHOT_URI, SERVING_REGISTRY_URI);
        store.fail_next_conditional_status(status);

        let error = publish_generation(&store, &conn, publication_request(7))
            .expect_err("conditional S3 status must be classified before cause erasure");

        assert_eq!(error.code(), "stale_pointer", "HTTP status {status}");
        assert_eq!(store.object(POINTER_URI).unwrap().bytes, previous);
    }
    Ok(())
}

#[test]
fn public_storage_errors_are_stable_and_sanitized() -> Result<()> {
    const SECRET: &str =
        "bucket=private-bucket key=secret-key credential=AKIA_TEST request_id=req-secret";
    let store_error = PublicationStoreError::Storage;
    assert_eq!(store_error.code(), "storage_error");
    assert_eq!(
        store_error.to_string(),
        "publication storage operation failed"
    );
    assert!(!format!("{store_error:?}").contains(SECRET));

    let conn = publication_catalog(&[complete_publication_row(7)])?;
    let error = publish_generation(&SecretFailingStore, &conn, publication_request(7))
        .expect_err("secret-bearing storage cause must remain internal");
    assert_eq!(error.code(), "storage_error");
    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));
    Ok(())
}

#[derive(Debug, Clone)]
struct PublicationCatalogRow {
    source: String,
    package: String,
    revision: String,
    generation: Option<i64>,
    index_status: String,
    graph_manifest_uri: Option<String>,
    graph_manifest_sha256: Option<String>,
    graph_manifest_bytes: Option<i64>,
    source_sidecar_uri: Option<String>,
    source_sidecar_sha256: Option<String>,
    source_sidecar_bytes: Option<i64>,
}

fn complete_publication_row(generation: i64) -> PublicationCatalogRow {
    PublicationCatalogRow {
        source: SOURCE.to_owned(),
        package: PACKAGE.to_owned(),
        revision: "1.0.194".to_owned(),
        generation: Some(generation),
        index_status: "complete".to_owned(),
        graph_manifest_uri: Some(GRAPH_MANIFEST_URI.to_owned()),
        graph_manifest_sha256: Some(sha256_bytes(GRAPH_MANIFEST_BYTES)),
        graph_manifest_bytes: Some(GRAPH_MANIFEST_BYTES.len() as i64),
        source_sidecar_uri: Some(SOURCE_SIDECAR_URI.to_owned()),
        source_sidecar_sha256: Some(sha256_bytes(SOURCE_SIDECAR_BYTES)),
        source_sidecar_bytes: Some(SOURCE_SIDECAR_BYTES.len() as i64),
    }
}

fn older_complete_publication_row(generation: i64) -> PublicationCatalogRow {
    PublicationCatalogRow {
        package: "serde_json".to_owned(),
        revision: "1.0.120".to_owned(),
        graph_manifest_uri: Some(OLDER_GRAPH_MANIFEST_URI.to_owned()),
        graph_manifest_sha256: Some(sha256_bytes(OLDER_GRAPH_MANIFEST_BYTES)),
        graph_manifest_bytes: Some(OLDER_GRAPH_MANIFEST_BYTES.len() as i64),
        source_sidecar_uri: Some(OLDER_SOURCE_SIDECAR_URI.to_owned()),
        source_sidecar_sha256: Some(sha256_bytes(OLDER_SOURCE_SIDECAR_BYTES)),
        source_sidecar_bytes: Some(OLDER_SOURCE_SIDECAR_BYTES.len() as i64),
        ..complete_publication_row(generation)
    }
}

fn publication_catalog(rows: &[PublicationCatalogRow]) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("open publication catalog")?;
    conn.execute_batch(
        r#"
        CREATE SCHEMA gold;
        CREATE TABLE gold.package_catalog (
            source VARCHAR,
            package VARCHAR,
            revision VARCHAR,
            generation BIGINT,
            index_status VARCHAR,
            graph_manifest_uri VARCHAR,
            graph_manifest_sha256 VARCHAR,
            graph_manifest_bytes BIGINT,
            source_sidecar_uri VARCHAR,
            source_sidecar_sha256 VARCHAR,
            source_sidecar_bytes BIGINT
        );
        "#,
    )?;
    for row in rows {
        conn.execute(
            r#"
            INSERT INTO gold.package_catalog VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                row.source,
                row.package,
                row.revision,
                row.generation,
                row.index_status,
                row.graph_manifest_uri,
                row.graph_manifest_sha256,
                row.graph_manifest_bytes,
                row.source_sidecar_uri,
                row.source_sidecar_sha256,
                row.source_sidecar_bytes,
            ],
        )?;
    }
    Ok(conn)
}

fn publication_request(generation: i64) -> GenerationPublication {
    let generation_prefix =
        format!("s3://catalog-test/gold/catalog-snapshot/generations/{generation:020}");
    GenerationPublication {
        generation,
        data_path: "s3://catalog-test/gold/data".to_owned(),
        snapshot_uri: format!("{generation_prefix}/spur_context.ducklake"),
        snapshot_bytes: SNAPSHOT_BYTES.to_vec(),
        snapshot_manifest_uri: format!("{generation_prefix}/manifest.json"),
        serving_registry_uri: format!("{generation_prefix}/serving-registry.json"),
        pointer_uri: POINTER_URI.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PublicationEvent {
    PutImmutable(String),
    PutPointer {
        uri: String,
        precondition: PointerPrecondition,
    },
}

#[derive(Debug, Default)]
struct FakeS3 {
    state: Mutex<FakeS3State>,
}

#[derive(Debug, Default)]
struct FakeS3State {
    objects: BTreeMap<String, PublicationObject>,
    events: Vec<PublicationEvent>,
    next_version: u64,
    fail_next_conditional_status: Option<u16>,
}

impl FakeS3 {
    fn seed(&self, uri: &str, bytes: &[u8]) -> PublicationObject {
        let mut state = self.state.lock().unwrap();
        insert_fake_object(&mut state, uri, bytes)
    }

    fn object(&self, uri: &str) -> Option<PublicationObject> {
        self.state.lock().unwrap().objects.get(uri).cloned()
    }

    fn fail_next_conditional_write(&self) {
        self.fail_next_conditional_status(412);
    }

    fn fail_next_conditional_status(&self, status: u16) {
        self.state.lock().unwrap().fail_next_conditional_status = Some(status);
    }

    fn remove(&self, uri: &str) {
        self.state.lock().unwrap().objects.remove(uri);
    }

    fn clear_events(&self) {
        self.state.lock().unwrap().events.clear();
    }

    fn set_version_id(&self, uri: &str, version_id: &str) {
        self.state
            .lock()
            .unwrap()
            .objects
            .get_mut(uri)
            .expect("object must exist")
            .version_id = Some(version_id.to_owned());
    }

    fn write_uris(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .events
            .iter()
            .map(|event| match event {
                PublicationEvent::PutImmutable(uri) => uri.clone(),
                PublicationEvent::PutPointer { uri, .. } => uri.clone(),
            })
            .collect()
    }

    fn pointer_write_count(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| matches!(event, PublicationEvent::PutPointer { .. }))
            .count()
    }

    fn pointer_preconditions(&self) -> Vec<PointerPrecondition> {
        self.state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter_map(|event| match event {
                PublicationEvent::PutPointer { precondition, .. } => Some(precondition.clone()),
                PublicationEvent::PutImmutable(_) => None,
            })
            .collect()
    }
}

impl PublicationStore for FakeS3 {
    fn read_object(
        &self,
        uri: &str,
    ) -> std::result::Result<Option<PublicationObject>, PublicationStoreError> {
        Ok(self.state.lock().unwrap().objects.get(uri).cloned())
    }

    fn put_immutable_object(
        &self,
        uri: &str,
        bytes: &[u8],
        _content_type: &str,
    ) -> std::result::Result<(), PublicationStoreError> {
        let mut state = self.state.lock().unwrap();
        if state.objects.contains_key(uri) {
            return Err(PublicationStoreError::Storage);
        }
        state
            .events
            .push(PublicationEvent::PutImmutable(uri.to_owned()));
        insert_fake_object(&mut state, uri, bytes);
        Ok(())
    }

    fn compare_and_swap_pointer(
        &self,
        uri: &str,
        bytes: &[u8],
        _content_type: &str,
        precondition: &PointerPrecondition,
    ) -> std::result::Result<(), PublicationStoreError> {
        let mut state = self.state.lock().unwrap();
        state.events.push(PublicationEvent::PutPointer {
            uri: uri.to_owned(),
            precondition: precondition.clone(),
        });
        if let Some(status) = state.fail_next_conditional_status.take() {
            return Err(match status {
                404 | 409 | 412 => PublicationStoreError::PreconditionFailed,
                _ => PublicationStoreError::Storage,
            });
        }
        let matches = match (precondition, state.objects.get(uri)) {
            (PointerPrecondition::Absent, None) => true,
            (PointerPrecondition::Matches { etag }, Some(current)) => current.etag == *etag,
            _ => false,
        };
        if !matches {
            return Err(PublicationStoreError::PreconditionFailed);
        }
        insert_fake_object(&mut state, uri, bytes);
        Ok(())
    }
}

fn insert_fake_object(state: &mut FakeS3State, uri: &str, bytes: &[u8]) -> PublicationObject {
    state.next_version += 1;
    let object = PublicationObject {
        bytes: bytes.to_vec(),
        etag: format!("etag-{}", state.next_version),
        version_id: Some(state.next_version.to_string()),
    };
    state.objects.insert(uri.to_owned(), object.clone());
    object
}

fn complete_publication_store() -> FakeS3 {
    let store = FakeS3::default();
    store.seed(GRAPH_MANIFEST_URI, GRAPH_MANIFEST_BYTES);
    store.seed(SOURCE_SIDECAR_URI, SOURCE_SIDECAR_BYTES);
    store.seed(OLDER_GRAPH_MANIFEST_URI, OLDER_GRAPH_MANIFEST_BYTES);
    store.seed(OLDER_SOURCE_SIDECAR_URI, OLDER_SOURCE_SIDECAR_BYTES);
    store
}

struct SecretFailingStore;

impl PublicationStore for SecretFailingStore {
    fn read_object(
        &self,
        _uri: &str,
    ) -> std::result::Result<Option<PublicationObject>, PublicationStoreError> {
        let _internal_cause =
            "bucket=private-bucket key=secret-key credential=AKIA_TEST request_id=req-secret";
        Err(PublicationStoreError::Storage)
    }

    fn put_immutable_object(
        &self,
        _uri: &str,
        _bytes: &[u8],
        _content_type: &str,
    ) -> std::result::Result<(), PublicationStoreError> {
        unreachable!("read failure must stop publication")
    }

    fn compare_and_swap_pointer(
        &self,
        _uri: &str,
        _bytes: &[u8],
        _content_type: &str,
        _precondition: &PointerPrecondition,
    ) -> std::result::Result<(), PublicationStoreError> {
        unreachable!("read failure must stop publication")
    }
}

fn seed_pointer(
    store: &FakeS3,
    generation: i64,
    snapshot_uri: &str,
    registry_uri: &str,
) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "generation": generation,
        "snapshot_uri": snapshot_uri,
        "data_path": "s3://catalog-test/gold/data",
        "sha256": "a".repeat(64),
        "bytes": 10,
        "serving_registry_uri": registry_uri,
        "serving_registry_sha256": "b".repeat(64),
        "serving_registry_bytes": 20,
        "status": "published"
    }))
    .unwrap();
    store.seed(POINTER_URI, &bytes);
    bytes
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct CatalogFixture {
    resolver: CatalogResolver,
    _root: PathBuf,
}

impl CatalogFixture {
    fn new(name: &str) -> Result<Self> {
        let root = unique_temp_dir(name)?;
        fs::create_dir_all(root.join("data")).context("create ducklake data dir")?;

        let catalog_path = root.join("catalog.sqlite");
        let data_path = root.join("data");
        let catalog_dsn = format!("sqlite:{}", catalog_path.display());
        let data_path = data_path.display().to_string();

        initialize_catalog(&catalog_dsn, &data_path)?;
        let resolver = CatalogResolver::new_with_data_path(&catalog_dsn, &data_path)?;

        Ok(Self {
            resolver,
            _root: root,
        })
    }
}

fn initialize_catalog(catalog_dsn: &str, data_path: &str) -> Result<()> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    attach_ducklake(&conn, catalog_dsn, data_path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE package_catalog (
            source VARCHAR,
            package VARCHAR,
            revision VARCHAR,
            revision_kind VARCHAR,
            semver_major INTEGER,
            semver_minor INTEGER,
            semver_patch INTEGER,
            snapshot_id BIGINT,
            indexed_at TIMESTAMP,
            index_status VARCHAR,
            embeddings_status VARCHAR,
            row_counts JSON
        );
        ALTER TABLE package_catalog SET PARTITIONED BY (source, package);

        CREATE TABLE refs (
            source VARCHAR,
            package VARCHAR,
            ref_name VARCHAR,
            revision VARCHAR,
            updated_at TIMESTAMP
        );
        ALTER TABLE refs SET PARTITIONED BY (source, package);

        INSERT INTO package_catalog VALUES
            ('registry:crates-io', 'serde', '1.0.193', 'semver',
             1, 0, 193, 193, TIMESTAMP '2026-06-22 00:00:00',
             'complete', 'complete', '{"nodes": 10}'),
            ('registry:crates-io', 'serde', '1.0.194', 'semver',
             1, 0, 194, 194, TIMESTAMP '2026-06-22 01:00:00',
             'complete', 'complete', '{"nodes": 11}');

        INSERT INTO refs VALUES
            ('registry:crates-io', 'serde', 'latest', '1.0.194',
             TIMESTAMP '2026-06-22 01:05:00'),
            ('registry:crates-io', 'serde', 'v1.0.193', '1.0.193',
             TIMESTAMP '2026-06-22 00:05:00');
        "#,
    )
    .context("seed catalog fixture")?;
    Ok(())
}

fn attach_ducklake(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    let catalog_dsn = escape_sql_literal(catalog_dsn);
    let data_path = escape_sql_literal(data_path);
    conn.execute_batch("INSTALL ducklake; INSTALL sqlite; LOAD ducklake; LOAD sqlite;")
        .context("load ducklake/sqlite extensions")?;
    conn.execute_batch(&format!(
        "ATTACH 'ducklake:{catalog_dsn}' AS spur_context (DATA_PATH '{data_path}'); USE spur_context;"
    ))
    .context("attach ducklake")
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

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> Result<i64> {
    conn.query_row(
        r"
        SELECT COUNT(*)
        FROM information_schema.tables
        WHERE table_schema = ? AND table_name = ?
        ",
        [schema, table],
        |row| row.get(0),
    )
    .with_context(|| format!("check table exists {schema}.{table}"))
}

fn columns_for(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            r"
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = ? AND table_name = ?
            ORDER BY ordinal_position
            ",
        )
        .with_context(|| format!("inspect columns for {schema}.{table}"))?;
    stmt.query_map([schema, table], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("collect columns for {schema}.{table}"))
}

fn nullable_column_count(
    conn: &Connection,
    schema: &str,
    table: &str,
    column: &str,
) -> Result<i64> {
    conn.query_row(
        r"
        SELECT COUNT(*)
        FROM information_schema.columns
        WHERE table_schema = ?
          AND table_name = ?
          AND column_name = ?
          AND is_nullable = 'YES'
        ",
        [schema, table, column],
        |row| row.get(0),
    )
    .with_context(|| format!("check nullable column {schema}.{table}.{column}"))
}
