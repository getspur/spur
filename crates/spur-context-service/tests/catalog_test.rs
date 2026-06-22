use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::Connection;
use spur_context_service::catalog::CatalogResolver;

const SOURCE: &str = "registry:crates-io";
const PACKAGE: &str = "serde";

#[test]
fn init_catalog_sql_creates_expected_nodes_schema() -> Result<()> {
    let root = unique_temp_dir("init-sql")?;
    fs::create_dir_all(root.join("data")).context("create ducklake data dir")?;
    let catalog_dsn = format!("sqlite:{}", root.join("catalog.sqlite").display());
    let data_path = root.join("data").display().to_string();

    let sql = include_str!("../sql/init_catalog.sql")
        .replace("INSTALL postgres;", "INSTALL sqlite;")
        .replace("LOAD postgres;", "LOAD sqlite;")
        .replace("__CATALOG_DSN__", &escape_sql_literal(&catalog_dsn))
        .replace("s3://spur-context/data/", &escape_sql_literal(&data_path));

    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    conn.execute_batch(&sql)
        .context("execute init catalog sql")?;

    let mut stmt = conn
        .prepare("PRAGMA table_info('nodes')")
        .context("inspect nodes schema")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(
        columns,
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
        ]
    );
    Ok(())
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
