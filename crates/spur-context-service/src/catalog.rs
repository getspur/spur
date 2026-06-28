use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use aws_sdk_s3::primitives::ByteStream;
use duckdb::{params, Connection};

pub const DEFAULT_DATA_PATH: &str = "s3://spur-context/gold/data/";
const SNAPSHOT_RELATIVE_PATH: &str = "gold/catalog-snapshot/spur_context.ducklake";
const SNAPSHOT_INDEXES: &[(&str, &str, &str)] = &[
    (
        "ducklake_data_file_data_file_id",
        "ducklake_data_file",
        "data_file_id",
    ),
    (
        "ducklake_delete_file_delete_file_id",
        "ducklake_delete_file",
        "delete_file_id",
    ),
    ("ducklake_schema_schema_id", "ducklake_schema", "schema_id"),
    (
        "ducklake_snapshot_snapshot_id",
        "ducklake_snapshot",
        "snapshot_id",
    ),
    (
        "ducklake_snapshot_changes_snapshot_id",
        "ducklake_snapshot_changes",
        "snapshot_id",
    ),
];

pub(crate) fn is_remote_catalog(catalog_dsn: &str) -> bool {
    catalog_dsn.starts_with("s3://")
        || catalog_dsn.starts_with("https://")
        || catalog_dsn.starts_with("http://")
}

#[derive(Debug)]
pub struct CatalogResolver {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedRevision {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub revision_kind: String,
    pub snapshot_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevisionInfo {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub revision_kind: String,
    pub semver_major: Option<i32>,
    pub semver_minor: Option<i32>,
    pub semver_patch: Option<i32>,
    pub snapshot_id: i64,
    pub index_status: String,
    pub embeddings_status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCleanupOptions {
    pub older_than: Duration,
    pub republish_lag: Duration,
}

impl CatalogResolver {
    pub fn new(catalog_dsn: &str) -> Result<Self> {
        Self::new_with_data_path(catalog_dsn, DEFAULT_DATA_PATH)
    }

    pub fn new_with_data_path(catalog_dsn: &str, data_path: &str) -> Result<Self> {
        Ok(Self::from_connection(connect_ducklake_with_data_path(
            catalog_dsn,
            data_path,
        )?))
    }

    pub fn from_connection(conn: Connection) -> Self {
        Self { conn }
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn resolve(
        &self,
        source: &str,
        package: &str,
        revision_or_ref: &str,
    ) -> Result<ResolvedRevision> {
        if let Some(revision) = self.lookup_ref_revision(source, package, revision_or_ref)? {
            return self.lookup_revision(source, package, &revision)?.ok_or_else(|| {
                anyhow!(
                    "revision not found after resolving ref: {source}/{package}@{revision_or_ref} -> {revision}"
                )
            });
        }

        self.lookup_revision(source, package, revision_or_ref)?
            .ok_or_else(|| anyhow!("revision not found: {source}/{package}@{revision_or_ref}"))
    }

    pub fn resolve_latest(&self, source: &str, package: &str) -> Result<ResolvedRevision> {
        self.resolve(source, package, "latest")
    }

    pub fn list_revisions(&self, source: &str, package: &str) -> Result<Vec<RevisionInfo>> {
        let package_catalog = readable_table(&self.conn, "package_catalog")?;
        let sql = format!(
            r"
            SELECT
                source,
                package,
                revision,
                revision_kind,
                semver_major,
                semver_minor,
                semver_patch,
                snapshot_id,
                index_status,
                embeddings_status
            FROM {package_catalog}
            WHERE source = ? AND package = ?
            ORDER BY
                CASE WHEN revision_kind = 'semver' THEN 0 ELSE 1 END,
                semver_major DESC NULLS LAST,
                semver_minor DESC NULLS LAST,
                semver_patch DESC NULLS LAST,
                indexed_at DESC NULLS LAST,
                revision DESC
            "
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .context("failed to prepare revision list query")?;

        stmt.query_map(params![source, package], revision_info_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list revisions")
    }

    fn lookup_ref_revision(
        &self,
        source: &str,
        package: &str,
        ref_name: &str,
    ) -> Result<Option<String>> {
        let refs = readable_table(&self.conn, "refs")?;
        let sql = format!(
            r"
            SELECT revision
            FROM {refs}
            WHERE source = ? AND package = ? AND ref_name = ?
            LIMIT 1
            "
        );
        optional_no_rows(
            self.conn
                .query_row(&sql, params![source, package, ref_name], |row| row.get(0)),
            "failed to resolve catalog ref",
        )
    }

    fn lookup_revision(
        &self,
        source: &str,
        package: &str,
        revision: &str,
    ) -> Result<Option<ResolvedRevision>> {
        let package_catalog = readable_table(&self.conn, "package_catalog")?;
        let sql = format!(
            r"
            SELECT source, package, revision, revision_kind, snapshot_id
            FROM {package_catalog}
            WHERE source = ? AND package = ? AND revision = ?
            LIMIT 1
            "
        );
        optional_no_rows(
            self.conn
                .query_row(&sql, params![source, package, revision], |row| {
                    Ok(ResolvedRevision {
                        source: row.get(0)?,
                        package: row.get(1)?,
                        revision: row.get(2)?,
                        revision_kind: row.get(3)?,
                        snapshot_id: row.get(4)?,
                    })
                }),
            "failed to resolve catalog revision",
        )
    }
}

pub fn connect_ducklake(catalog_dsn: &str) -> Result<Connection> {
    connect_ducklake_with_data_path(catalog_dsn, DEFAULT_DATA_PATH)
}

pub fn connect_frozen_snapshot(snapshot_path: &Path, data_path: &str) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    load_ducklake_extensions(&conn, &snapshot_path.display().to_string())?;

    if data_path.starts_with("s3://") {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
        conn.execute_batch(&format!(
            "INSTALL httpfs; LOAD httpfs; \
             CREATE OR REPLACE SECRET s3_creds (TYPE s3, PROVIDER credential_chain, REGION '{region}');",
        ))
        .context("failed to load httpfs for frozen snapshot S3 data path")?;
    }

    attach_frozen_snapshot(&conn, snapshot_path, data_path)?;
    Ok(conn)
}

pub fn connect_ducklake_with_data_path(catalog_dsn: &str, data_path: &str) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    load_ducklake_extensions(&conn, catalog_dsn)?;

    if data_path.starts_with("s3://") && !is_remote_catalog(catalog_dsn) {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
        conn.execute_batch(&format!(
            "INSTALL httpfs; LOAD httpfs; \
             CREATE OR REPLACE SECRET s3_creds (TYPE s3, PROVIDER credential_chain, REGION '{region}');",
        ))
        .context("failed to load httpfs for S3 data path")?;
    }

    if is_remote_catalog(catalog_dsn) {
        conn.execute_batch("SET unsafe_disable_etag_checks = true;")
            .context("failed to disable etag checks for remote catalog")?;
    }

    attach_ducklake(&conn, catalog_dsn, data_path)?;

    Ok(conn)
}

pub fn compact_gold_and_export_snapshot(
    catalog_dsn: &str,
    data_path: &str,
    options: SnapshotCleanupOptions,
) -> Result<PathBuf> {
    if options.older_than < options.republish_lag {
        bail!("cleanup older_than must be >= republish_lag");
    }

    let conn = connect_ducklake_with_data_path(catalog_dsn, data_path)?;
    run_optional_maintenance_call(&conn, "CALL ducklake_merge_adjacent_files('spur_context')")?;
    let older_than = interval_literal(options.older_than);
    run_optional_maintenance_call(
        &conn,
        &format!(
            "CALL ducklake_expire_snapshots('spur_context', older_than => CAST(now() AS TIMESTAMP) - INTERVAL '{older_than}')"
        ),
    )?;
    run_optional_maintenance_call(
        &conn,
        &format!(
            "CALL ducklake_cleanup_old_files('spur_context', older_than => CAST(now() AS TIMESTAMP) - INTERVAL '{older_than}')"
        ),
    )?;
    conn.execute("FORCE CHECKPOINT", [])
        .context("failed to checkpoint DuckLake before snapshot export")?;

    export_frozen_snapshot(catalog_dsn, data_path)
}

pub(crate) fn export_frozen_snapshot(catalog_dsn: &str, data_path: &str) -> Result<PathBuf> {
    let location = SnapshotLocation::for_data_path(data_path)?;
    if let Some(parent) = location.local_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create snapshot dir `{}`", parent.display()))?;
    }
    if location.local_path.exists() {
        fs::remove_file(&location.local_path).with_context(|| {
            format!(
                "failed to replace existing snapshot `{}`",
                location.local_path.display()
            )
        })?;
    }

    copy_ducklake_metadata_tables(catalog_dsn, &location.local_path)?;
    replay_snapshot_indexes(&location.local_path)?;
    validate_snapshot_attaches(&location.local_path, data_path)?;
    verify_snapshot_referenced_files_exist(&location.local_path, data_path)?;

    if let Some(uri) = location.s3_uri {
        upload_file_to_s3(&location.local_path, &uri)?;
    }

    Ok(location.local_path)
}

pub(crate) fn readable_table(conn: &Connection, table: &str) -> Result<String> {
    if table_exists_in_schema(conn, "gold", table)? {
        Ok(format!("gold.{table}"))
    } else {
        Ok(table.to_owned())
    }
}

pub(crate) fn gold_table(table: &str) -> String {
    format!("gold.{table}")
}

fn revision_info_from_row(row: &duckdb::Row<'_>) -> duckdb::Result<RevisionInfo> {
    Ok(RevisionInfo {
        source: row.get(0)?,
        package: row.get(1)?,
        revision: row.get(2)?,
        revision_kind: row.get(3)?,
        semver_major: row.get(4)?,
        semver_minor: row.get(5)?,
        semver_patch: row.get(6)?,
        snapshot_id: row.get(7)?,
        index_status: row.get(8)?,
        embeddings_status: row.get(9)?,
    })
}

fn optional_no_rows<T>(result: duckdb::Result<T>, context: &'static str) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context(context),
    }
}

fn load_ducklake_extensions(conn: &Connection, catalog_dsn: &str) -> Result<()> {
    conn.execute_batch("INSTALL ducklake; LOAD ducklake;")
        .context("failed to load ducklake extension")?;

    if is_remote_catalog(catalog_dsn) {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
        conn.execute_batch(&format!(
            "INSTALL httpfs; LOAD httpfs; \
             CREATE OR REPLACE SECRET s3_creds (TYPE s3, PROVIDER credential_chain, REGION '{region}');",
        ))
        .context("failed to load httpfs extension for remote DuckLake catalog")?;
    } else if catalog_dsn.starts_with("sqlite:") || catalog_dsn.starts_with("ducklake:sqlite:") {
        conn.execute_batch("INSTALL sqlite; LOAD sqlite;")
            .context("failed to load sqlite extension for DuckLake catalog")?;
    } else if catalog_dsn.starts_with("postgres:")
        || catalog_dsn.starts_with("postgresql:")
        || catalog_dsn.starts_with("postgresql://")
        || catalog_dsn.starts_with("ducklake:postgres:")
        || catalog_dsn.starts_with("ducklake:postgresql:")
        || catalog_dsn.starts_with("ducklake:postgresql://")
    {
        conn.execute_batch("INSTALL postgres; LOAD postgres;")
            .context("failed to load postgres extension for DuckLake catalog")?;
    }

    Ok(())
}

fn attach_ducklake(conn: &Connection, catalog_dsn: &str, data_path: &str) -> Result<()> {
    if is_remote_catalog(catalog_dsn) {
        conn.execute_batch(&format!(
            "ATTACH '{}' AS spur_context (TYPE ducklake); USE spur_context;",
            escape_sql_literal(catalog_dsn)
        ))
        .context("failed to attach remote DuckLake catalog")
    } else {
        let attach_uri = if catalog_dsn.starts_with("ducklake:") {
            catalog_dsn.to_owned()
        } else {
            format!("ducklake:{catalog_dsn}")
        };
        conn.execute_batch(&format!(
            "ATTACH '{}' AS spur_context (DATA_PATH '{}'); USE spur_context;",
            escape_sql_literal(&attach_uri),
            escape_sql_literal(data_path)
        ))
        .context("failed to attach DuckLake catalog")
    }
}

fn attach_frozen_snapshot(conn: &Connection, snapshot_path: &Path, data_path: &str) -> Result<()> {
    let attach_uri = format!("ducklake:{}", snapshot_path.display());
    conn.execute_batch(&format!(
        "ATTACH '{}' AS spur_context (DATA_PATH '{}', READ_ONLY); USE spur_context;",
        escape_sql_literal(&attach_uri),
        escape_sql_literal(data_path)
    ))
    .context("failed to attach frozen DuckLake snapshot")
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn table_exists_in_schema(conn: &Connection, schema: &str, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            r"
            SELECT COUNT(*)
            FROM information_schema.tables
            WHERE table_schema = ? AND table_name = ?
            ",
            params![schema, table],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to inspect table {schema}.{table}"))?;
    Ok(count > 0)
}

fn run_optional_maintenance_call(conn: &Connection, sql: &str) -> Result<()> {
    match conn.prepare(sql) {
        Ok(mut stmt) => {
            let mut rows = stmt
                .query([])
                .with_context(|| format!("failed to execute DuckLake maintenance `{sql}`"))?;
            while rows.next()?.is_some() {}
            Ok(())
        }
        Err(error) => {
            let message = error.to_string();
            if message.contains("does not exist") {
                eprintln!(
                    "[catalog] skipping unavailable DuckLake maintenance call `{sql}`: {message}"
                );
                Ok(())
            } else {
                Err(error)
                    .with_context(|| format!("failed to prepare DuckLake maintenance `{sql}`"))
            }
        }
    }
}

fn interval_literal(duration: Duration) -> String {
    format!("{} seconds", duration.as_secs())
}

#[derive(Debug)]
struct SnapshotLocation {
    local_path: PathBuf,
    s3_uri: Option<String>,
}

impl SnapshotLocation {
    fn for_data_path(data_path: &str) -> Result<Self> {
        if data_path.starts_with("s3://") {
            let uri = snapshot_s3_uri(data_path);
            let mut local_path = std::env::temp_dir();
            local_path.push(format!(
                "spur_context_snapshot_{}.ducklake",
                uuid::Uuid::new_v4()
            ));
            Ok(Self {
                local_path,
                s3_uri: Some(uri),
            })
        } else {
            Ok(Self {
                local_path: Path::new(data_path).join(SNAPSHOT_RELATIVE_PATH),
                s3_uri: None,
            })
        }
    }
}

fn snapshot_s3_uri(data_path: &str) -> String {
    let trimmed = data_path.trim_end_matches('/');
    if let Some(base) = trimmed.strip_suffix("/gold/data") {
        format!("{base}/{SNAPSHOT_RELATIVE_PATH}")
    } else {
        format!("{trimmed}/{SNAPSHOT_RELATIVE_PATH}")
    }
}

fn copy_ducklake_metadata_tables(catalog_dsn: &str, snapshot_path: &Path) -> Result<()> {
    let conn = Connection::open_in_memory().context("failed to open snapshot exporter DuckDB")?;
    conn.execute_batch("INSTALL sqlite; INSTALL postgres; LOAD sqlite; LOAD postgres;")
        .context("failed to load metadata backend extensions")?;
    let source = attach_metadata_catalog(&conn, catalog_dsn)?;
    conn.execute_batch(&format!(
        "ATTACH '{}' AS snap;",
        escape_sql_literal(&snapshot_path.display().to_string())
    ))
    .context("failed to attach snapshot DuckDB file")?;

    let tables = list_ducklake_metadata_tables(&conn, &source)?;
    if tables.is_empty() {
        bail!("catalog metadata backend has no ducklake_* tables");
    }
    copy_metadata_tables_in_transaction(&conn, &source, &tables)
}

fn copy_metadata_tables_in_transaction(
    conn: &Connection,
    source: &MetadataSource,
    tables: &[String],
) -> Result<()> {
    // Keep all attached metadata reads on one snapshot. Without this, each
    // CTAS can observe a different Postgres/Aurora catalog commit.
    conn.execute_batch("BEGIN TRANSACTION")
        .context("failed to begin consistent DuckLake metadata snapshot copy")?;
    let result = (|| {
        for table in tables {
            conn.execute_batch(&format!(
                "CREATE TABLE snap.\"{table}\" AS SELECT * FROM {}.\"{table}\";",
                source.select_prefix
            ))
            .with_context(|| format!("failed to copy DuckLake metadata table `{table}`"))?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .context("failed to commit DuckLake metadata snapshot copy"),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[derive(Debug)]
struct MetadataSource {
    select_prefix: String,
    list_sql: &'static str,
}

fn attach_metadata_catalog(conn: &Connection, catalog_dsn: &str) -> Result<MetadataSource> {
    if let Some(path) = sqlite_catalog_path(catalog_dsn) {
        conn.execute_batch(&format!(
            "ATTACH '{}' AS pg (TYPE sqlite);",
            escape_sql_literal(&path.display().to_string())
        ))
        .context("failed to attach sqlite DuckLake metadata catalog")?;
        return Ok(MetadataSource {
            select_prefix: "pg".to_owned(),
            list_sql: "SHOW TABLES FROM pg",
        });
    }

    if is_postgres_catalog(catalog_dsn) {
        let dsn = postgres_metadata_dsn(catalog_dsn);
        conn.execute_batch(&format!(
            "ATTACH '{}' AS pg (TYPE postgres);",
            escape_sql_literal(&dsn)
        ))
        .context("failed to attach postgres DuckLake metadata catalog")?;
        return Ok(MetadataSource {
            select_prefix: "pg.public".to_owned(),
            list_sql: "SHOW TABLES FROM pg.public",
        });
    }

    bail!("snapshot export supports sqlite and postgres DuckLake catalogs, got `{catalog_dsn}`")
}

fn list_ducklake_metadata_tables(
    conn: &Connection,
    source: &MetadataSource,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(source.list_sql)
        .context("failed to prepare DuckLake metadata table listing")?;
    let mut tables = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect DuckLake metadata table names")?;
    tables.retain(|table| table.starts_with("ducklake_"));
    tables.sort();
    Ok(tables)
}

fn replay_snapshot_indexes(snapshot_path: &Path) -> Result<()> {
    let conn = Connection::open(snapshot_path)
        .with_context(|| format!("failed to open snapshot `{}`", snapshot_path.display()))?;
    for (index_name, table, column) in SNAPSHOT_INDEXES {
        conn.execute_batch(&format!(
            "CREATE UNIQUE INDEX {index_name} ON \"{table}\"({column});"
        ))
        .with_context(|| format!("failed to replay DuckLake metadata index `{index_name}`"))?;
    }
    Ok(())
}

fn validate_snapshot_attaches(snapshot_path: &Path, data_path: &str) -> Result<()> {
    let conn = connect_ducklake_with_data_path(&snapshot_path.display().to_string(), data_path)?;
    let _count: i64 = conn
        .query_row(
            "SELECT COUNT(*)::BIGINT FROM information_schema.tables",
            [],
            |row| row.get(0),
        )
        .context("failed to query attached frozen snapshot")?;
    Ok(())
}

fn verify_snapshot_referenced_files_exist(snapshot_path: &Path, data_path: &str) -> Result<()> {
    let conn = Connection::open(snapshot_path)
        .with_context(|| format!("failed to open snapshot `{}`", snapshot_path.display()))?;
    for (table, id_column) in [
        ("ducklake_data_file", "data_file_id"),
        ("ducklake_delete_file", "delete_file_id"),
    ] {
        let sql = format!(
            r#"
            SELECT
                f.path,
                COALESCE(f.path_is_relative, 0)::BIGINT,
                t.path,
                COALESCE(t.path_is_relative, 0)::BIGINT,
                s.path,
                COALESCE(s.path_is_relative, 0)::BIGINT
            FROM "{table}" f
            JOIN ducklake_table t ON t.table_id = f.table_id
            JOIN ducklake_schema s ON s.schema_id = t.schema_id
            ORDER BY f.{id_column}
            "#
        );
        let mut stmt = conn
            .prepare(&sql)
            .with_context(|| format!("failed to prepare referenced-file query for {table}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ReferencedFile {
                    file_path: row.get(0)?,
                    file_path_is_relative: row.get::<_, i64>(1)? != 0,
                    table_path: row.get(2)?,
                    table_path_is_relative: row.get::<_, i64>(3)? != 0,
                    schema_path: row.get(4)?,
                    schema_path_is_relative: row.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("failed to collect referenced files from {table}"))?;
        for referenced in rows {
            verify_referenced_file(data_path, &referenced)?;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ReferencedFile {
    file_path: String,
    file_path_is_relative: bool,
    table_path: String,
    table_path_is_relative: bool,
    schema_path: String,
    schema_path_is_relative: bool,
}

fn verify_referenced_file(data_path: &str, referenced: &ReferencedFile) -> Result<()> {
    if data_path.starts_with("s3://") {
        let uri = referenced_s3_uri(data_path, referenced);
        return head_s3_object(&uri);
    }

    let path = referenced_local_path(data_path, referenced);
    if !path.is_file() {
        bail!("snapshot references missing data file `{}`", path.display());
    }
    Ok(())
}

fn referenced_local_path(data_path: &str, referenced: &ReferencedFile) -> PathBuf {
    if !referenced.file_path_is_relative {
        return PathBuf::from(&referenced.file_path);
    }
    let schema_base = if referenced.schema_path_is_relative {
        Path::new(data_path).join(&referenced.schema_path)
    } else {
        PathBuf::from(&referenced.schema_path)
    };
    let table_base = if referenced.table_path_is_relative {
        schema_base.join(&referenced.table_path)
    } else {
        PathBuf::from(&referenced.table_path)
    };
    table_base.join(&referenced.file_path)
}

fn referenced_s3_uri(data_path: &str, referenced: &ReferencedFile) -> String {
    if !referenced.file_path_is_relative {
        return referenced.file_path.clone();
    }
    let schema_base = if referenced.schema_path_is_relative {
        join_uri_path(data_path, &referenced.schema_path)
    } else {
        referenced.schema_path.clone()
    };
    let table_base = if referenced.table_path_is_relative {
        join_uri_path(&schema_base, &referenced.table_path)
    } else {
        referenced.table_path.clone()
    };
    join_uri_path(&table_base, &referenced.file_path)
}

fn join_uri_path(base: &str, child: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        child.trim_start_matches('/')
    )
}

fn upload_file_to_s3(local_path: &Path, uri: &str) -> Result<()> {
    let parsed = parse_s3_uri(uri)?;
    let bytes = fs::read(local_path)
        .with_context(|| format!("failed to read snapshot `{}`", local_path.display()))?;
    run_s3_blocking(move |client| async move {
        client
            .put_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .context("failed to upload frozen DuckLake snapshot to S3")?;
        Ok(())
    })
}

fn head_s3_object(uri: &str) -> Result<()> {
    let parsed = parse_s3_uri(uri)?;
    let uri = uri.to_owned();
    run_s3_blocking(move |client| async move {
        client
            .head_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .send()
            .await
            .with_context(|| format!("snapshot references missing S3 object `{uri}`"))?;
        Ok(())
    })
}

fn run_s3_blocking<F, Fut>(f: F) -> Result<()>
where
    F: FnOnce(aws_sdk_s3::Client) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().context("failed to create S3 runtime")?;
        runtime.block_on(async move {
            let client = s3_client_from_env();
            f(client).await
        })
    })
    .join()
    .map_err(|_| anyhow!("S3 helper thread panicked"))?
}

#[derive(Debug)]
struct ParsedS3Uri {
    bucket: String,
    key: String,
}

fn parse_s3_uri(uri: &str) -> Result<ParsedS3Uri> {
    let rest = uri
        .strip_prefix("s3://")
        .ok_or_else(|| anyhow!("not an s3 URI: {uri}"))?;
    let (bucket, key) = rest
        .split_once('/')
        .ok_or_else(|| anyhow!("s3 URI must include a key: {uri}"))?;
    if bucket.is_empty() || key.is_empty() {
        bail!("s3 URI must include non-empty bucket and key: {uri}");
    }
    Ok(ParsedS3Uri {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    })
}

fn s3_client_from_env() -> aws_sdk_s3::Client {
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_owned());
    let mut config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(region));
    if let (Ok(access_key), Ok(secret_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        config = config.credentials_provider(aws_sdk_s3::config::Credentials::new(
            access_key,
            secret_key,
            std::env::var("AWS_SESSION_TOKEN").ok(),
            None,
            "Env",
        ));
    }
    aws_sdk_s3::Client::from_conf(config.build())
}

fn postgres_metadata_dsn(catalog_dsn: &str) -> String {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    if let Some(rest) = dsn.strip_prefix("postgres:") {
        rest.to_owned()
    } else if let Some(rest) = dsn.strip_prefix("postgresql:") {
        format!("postgresql:{rest}")
    } else {
        dsn.to_owned()
    }
}

fn sqlite_catalog_path(catalog_dsn: &str) -> Option<PathBuf> {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    let path = dsn.strip_prefix("sqlite:")?;
    (path != ":memory:").then(|| PathBuf::from(path))
}

fn is_postgres_catalog(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.starts_with("postgres:")
        || dsn.starts_with("postgresql:")
        || dsn.starts_with("postgresql://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn metadata_copy_failure_does_not_leave_partial_snapshot_tables() -> Result<()> {
        let root = unique_temp_dir("metadata-copy-rollback")?;
        let catalog_path = root.join("catalog.sqlite");
        let snapshot_path = root.join("snapshot.ducklake");

        let setup = Connection::open_in_memory().context("open sqlite metadata setup DuckDB")?;
        setup
            .execute_batch("INSTALL sqlite; LOAD sqlite;")
            .context("load sqlite extension for setup")?;
        setup
            .execute_batch(&format!(
                r#"
                ATTACH '{}' AS pg (TYPE sqlite);
                CREATE TABLE pg.ducklake_a_valid (id BIGINT);
                INSERT INTO pg.ducklake_a_valid VALUES (1);
                CREATE TABLE pg."ducklake_z_bad""identifier" (id BIGINT);
                "#,
                escape_sql_literal(&catalog_path.display().to_string())
            ))
            .context("create sqlite metadata fixture")?;

        let err = copy_ducklake_metadata_tables(
            &format!("sqlite:{}", catalog_path.display()),
            &snapshot_path,
        )
        .expect_err("malformed metadata identifier should fail copy");
        assert!(
            format!("{err:#}").contains("ducklake_z_bad"),
            "unexpected error: {err:#}"
        );

        if snapshot_path.exists() {
            let snap = Connection::open(&snapshot_path)
                .with_context(|| format!("open snapshot `{}`", snapshot_path.display()))?;
            let copied_tables: i64 = snap
                .query_row(
                    "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'ducklake_a_valid'",
                    [],
                    |row| row.get(0),
                )
                .context("inspect partial snapshot tables")?;
            assert_eq!(
                copied_tables, 0,
                "metadata copy must roll back earlier tables if a later table fails"
            );
        }
        Ok(())
    }

    fn unique_temp_dir(prefix: &str) -> Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before epoch")?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{nanos}-{}", std::process::id()));
        fs::create_dir_all(&dir).with_context(|| format!("create temp dir `{}`", dir.display()))?;
        Ok(dir)
    }
}
