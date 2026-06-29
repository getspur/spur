use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use aws_sdk_s3::primitives::ByteStream;
use duckdb::{params, Connection};
use sha2::{Digest, Sha256};

pub(crate) const DUCKLAKE_DATA_PATH_ENV: &str = "SPUR_CONTEXT_DUCKLAKE_DATA_PATH";
const CATALOG_PASSWORD_ENV: &str = "SPUR_CATALOG_PASSWORD";
const SNAPSHOT_POINTER_RELATIVE_PATH: &str = "gold/catalog-snapshot/current.json";
const SNAPSHOT_GENERATIONS_RELATIVE_DIR: &str = "gold/catalog-snapshot/generations";
const SNAPSHOT_FILE_NAME: &str = "spur_context.ducklake";
const SNAPSHOT_MANIFEST_FILE_NAME: &str = "manifest.json";
const SNAPSHOT_MANIFEST_SCHEMA_VERSION: u32 = 1;
const SNAPSHOT_PUBLISHED_STATUS: &str = "published";
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrozenSnapshotManifest {
    pub schema_version: u32,
    pub generation: i64,
    pub snapshot_uri: String,
    pub data_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub status: String,
}

impl FrozenSnapshotManifest {
    pub fn published(
        generation: i64,
        snapshot_uri: String,
        data_path: String,
        sha256: String,
        bytes: u64,
    ) -> Self {
        Self {
            schema_version: SNAPSHOT_MANIFEST_SCHEMA_VERSION,
            generation,
            snapshot_uri,
            data_path,
            sha256,
            bytes,
            status: SNAPSHOT_PUBLISHED_STATUS.to_owned(),
        }
    }

    pub fn ensure_published(&self) -> Result<()> {
        if self.schema_version != SNAPSHOT_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported frozen snapshot manifest schema version {}",
                self.schema_version
            );
        }
        if self.status != SNAPSHOT_PUBLISHED_STATUS {
            bail!(
                "frozen snapshot manifest for generation {} is not published: {}",
                self.generation,
                self.status
            );
        }
        if self.generation <= 0 {
            bail!("frozen snapshot manifest generation must be positive");
        }
        if self.snapshot_uri.trim().is_empty() {
            bail!("frozen snapshot manifest snapshot_uri must be non-empty");
        }
        if self.data_path.trim().is_empty() {
            bail!("frozen snapshot manifest data_path must be non-empty");
        }
        if self.sha256.trim().is_empty() {
            bail!("frozen snapshot manifest sha256 must be non-empty");
        }
        Ok(())
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self> {
        let manifest: Self =
            serde_json::from_slice(bytes).context("failed to parse frozen snapshot manifest")?;
        manifest.ensure_published()?;
        Ok(manifest)
    }

    fn to_json_bytes(&self) -> Result<Vec<u8>> {
        self.ensure_published()?;
        let mut bytes =
            serde_json::to_vec_pretty(self).context("failed to encode frozen snapshot manifest")?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl CatalogResolver {
    pub fn new(catalog_dsn: &str) -> Result<Self> {
        let data_path = ducklake_data_path(catalog_dsn)?;
        Self::new_with_data_path(catalog_dsn, &data_path)
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
    let data_path = ducklake_data_path(catalog_dsn)?;
    connect_ducklake_with_data_path(catalog_dsn, &data_path)
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
    let catalog_dsn = catalog_dsn_with_env_password(catalog_dsn);
    let conn = Connection::open_in_memory().context("failed to open in-memory DuckDB")?;
    load_ducklake_extensions(&conn, &catalog_dsn)?;

    if data_path.starts_with("s3://") && !is_remote_catalog(&catalog_dsn) {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
        conn.execute_batch(&format!(
            "INSTALL httpfs; LOAD httpfs; \
             CREATE OR REPLACE SECRET s3_creds (TYPE s3, PROVIDER credential_chain, REGION '{region}');",
        ))
            .context("failed to load httpfs for S3 data path")?;
    }

    if is_remote_catalog(&catalog_dsn) {
        conn.execute_batch("SET unsafe_disable_etag_checks = true;")
            .context("failed to disable etag checks for remote catalog")?;
    }

    attach_ducklake(&conn, &catalog_dsn, data_path)?;

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

    let generation = current_snapshot_generation(&conn)?;
    export_frozen_snapshot(catalog_dsn, data_path, generation)
}

pub(crate) fn export_frozen_snapshot(
    catalog_dsn: &str,
    data_path: &str,
    generation: i64,
) -> Result<PathBuf> {
    let location = SnapshotLocation::for_data_path_and_generation(data_path, generation)?;
    if let Some(parent) = location.local_staging_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create snapshot dir `{}`", parent.display()))?;
    }
    if location.s3_uri.is_some() {
        if let Ok(manifest) = read_snapshot_manifest(&location) {
            if manifest.generation != generation || manifest.snapshot_uri != location.snapshot_uri {
                bail!(
                    "existing S3 frozen snapshot manifest does not match generation {generation}: `{}`",
                    location.local_manifest_path.display()
                );
            }
            publish_snapshot_pointer_if_current_not_newer(&location, &manifest)?;
            return Ok(location.local_path);
        }
    } else if location.local_path.exists() || location.local_manifest_path.exists() {
        if location.local_path.is_file() && location.local_manifest_path.is_file() {
            let manifest = read_snapshot_manifest(&location)?;
            if manifest.generation != generation || manifest.snapshot_uri != location.snapshot_uri {
                bail!(
                    "existing frozen snapshot manifest does not match generation {generation}: `{}`",
                    location.local_manifest_path.display()
                );
            }
            publish_snapshot_pointer_if_current_not_newer(&location, &manifest)?;
            return Ok(location.local_path);
        }
        bail!(
            "partial immutable frozen snapshot exists for generation {generation}: snapshot={}, manifest={}",
            location.local_path.display(),
            location.local_manifest_path.display()
        );
    }
    if location.local_staging_path.exists() {
        fs::remove_file(&location.local_staging_path).with_context(|| {
            format!(
                "failed to remove stale snapshot staging file `{}`",
                location.local_staging_path.display()
            )
        })?;
    }

    copy_ducklake_metadata_tables(catalog_dsn, &location.local_staging_path)?;
    replay_snapshot_indexes(&location.local_staging_path)?;
    validate_snapshot_attaches(&location.local_staging_path, data_path)?;
    verify_snapshot_referenced_files_exist(&location.local_staging_path, data_path)?;

    let (sha256, bytes) = file_sha256_and_len(&location.local_staging_path)?;
    let manifest = FrozenSnapshotManifest::published(
        generation,
        location.snapshot_uri.clone(),
        data_path.to_owned(),
        sha256,
        bytes,
    );

    publish_snapshot_file(&location)?;
    publish_snapshot_manifest(&location, &manifest)?;
    publish_snapshot_pointer_if_current_not_newer(&location, &manifest)?;

    Ok(location.local_path)
}

pub fn rollback_frozen_snapshot_pointer(
    data_path: &str,
    generation: i64,
) -> Result<FrozenSnapshotManifest> {
    let location = SnapshotLocation::for_data_path_and_generation(data_path, generation)?;
    let manifest = read_snapshot_manifest(&location)?;
    manifest.ensure_published()?;

    if let Some(uri) = &location.s3_uri {
        head_s3_object(uri)?;
    } else if !location.local_path.is_file() {
        bail!(
            "cannot roll back to missing frozen snapshot `{}`",
            location.local_path.display()
        );
    }

    publish_snapshot_pointer(&location, &manifest)?;
    Ok(manifest)
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

pub(crate) fn ducklake_data_path(catalog_dsn: &str) -> Result<String> {
    if let Ok(path) = std::env::var(DUCKLAKE_DATA_PATH_ENV) {
        if !path.trim().is_empty() {
            create_local_data_path_if_needed(&path)?;
            return Ok(path);
        }
    }

    if let Some(sqlite_path) = sqlite_catalog_path(catalog_dsn) {
        let path = sqlite_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("data");
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create DuckLake data path `{}`", path.display()))?;
        return Ok(path.display().to_string());
    }

    bail!("{DUCKLAKE_DATA_PATH_ENV} must be set for non-local DuckLake catalogs")
}

fn create_local_data_path_if_needed(path: &str) -> Result<()> {
    if path.contains("://") || path == ":memory:" {
        return Ok(());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create DuckLake data path `{path}`"))?;
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
    local_staging_path: PathBuf,
    local_manifest_path: PathBuf,
    local_pointer_path: PathBuf,
    snapshot_uri: String,
    s3_uri: Option<String>,
    s3_manifest_uri: Option<String>,
    s3_pointer_uri: Option<String>,
}

impl SnapshotLocation {
    fn for_data_path_and_generation(data_path: &str, generation: i64) -> Result<Self> {
        if generation <= 0 {
            bail!("frozen snapshot generation must be positive");
        }

        let generation_dir = format!("{generation:020}");
        if data_path.starts_with("s3://") {
            let uri = snapshot_s3_uri(data_path, generation);
            let manifest_uri = snapshot_manifest_s3_uri(data_path, generation);
            let pointer_uri = snapshot_pointer_s3_uri(data_path);
            let mut local_path = std::env::temp_dir();
            local_path.push(format!(
                "spur_context_snapshot_{}.ducklake",
                uuid::Uuid::new_v4()
            ));
            let local_manifest_path = local_path.with_extension("manifest.json");
            let local_pointer_path = local_path.with_extension("current.json");
            Ok(Self {
                local_staging_path: local_path.clone(),
                local_path,
                local_manifest_path,
                local_pointer_path,
                snapshot_uri: uri.clone(),
                s3_uri: Some(uri),
                s3_manifest_uri: Some(manifest_uri),
                s3_pointer_uri: Some(pointer_uri),
            })
        } else {
            let snapshot_dir = Path::new(data_path)
                .join(SNAPSHOT_GENERATIONS_RELATIVE_DIR)
                .join(generation_dir);
            let local_path = snapshot_dir.join(SNAPSHOT_FILE_NAME);
            let local_staging_path = snapshot_dir.join(format!(
                ".{SNAPSHOT_FILE_NAME}.{}.tmp",
                uuid::Uuid::new_v4()
            ));
            let local_manifest_path = snapshot_dir.join(SNAPSHOT_MANIFEST_FILE_NAME);
            let local_pointer_path = Path::new(data_path).join(SNAPSHOT_POINTER_RELATIVE_PATH);
            Ok(Self {
                snapshot_uri: local_path.display().to_string(),
                local_path,
                local_staging_path,
                local_manifest_path,
                local_pointer_path,
                s3_uri: None,
                s3_manifest_uri: None,
                s3_pointer_uri: None,
            })
        }
    }
}

fn snapshot_s3_uri(data_path: &str, generation: i64) -> String {
    format!(
        "{}/{}/{:020}/{}",
        snapshot_base_uri(data_path).trim_end_matches('/'),
        SNAPSHOT_GENERATIONS_RELATIVE_DIR,
        generation,
        SNAPSHOT_FILE_NAME
    )
}

fn snapshot_manifest_s3_uri(data_path: &str, generation: i64) -> String {
    format!(
        "{}/{}/{:020}/{}",
        snapshot_base_uri(data_path).trim_end_matches('/'),
        SNAPSHOT_GENERATIONS_RELATIVE_DIR,
        generation,
        SNAPSHOT_MANIFEST_FILE_NAME
    )
}

fn snapshot_pointer_s3_uri(data_path: &str) -> String {
    format!(
        "{}/{}",
        snapshot_base_uri(data_path).trim_end_matches('/'),
        SNAPSHOT_POINTER_RELATIVE_PATH
    )
}

fn snapshot_base_uri(data_path: &str) -> String {
    let trimmed = data_path.trim_end_matches('/');
    if let Some(base) = trimmed.strip_suffix("/gold/data") {
        base.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn current_snapshot_generation(conn: &Connection) -> Result<i64> {
    let generation: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(generation), MAX(snapshot_id), 0)::BIGINT FROM gold.package_catalog",
            [],
            |row| row.get(0),
        )
        .context("failed to read current published snapshot generation")?;
    if generation <= 0 {
        bail!("cannot export frozen snapshot before a gold generation is published");
    }
    Ok(generation)
}

fn publish_snapshot_file(location: &SnapshotLocation) -> Result<()> {
    if let Some(uri) = &location.s3_uri {
        upload_file_to_s3(&location.local_staging_path, uri)?;
        return Ok(());
    }

    if let Some(parent) = location.local_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create snapshot dir `{}`", parent.display()))?;
    }
    if location.local_path.exists() {
        bail!(
            "immutable frozen snapshot already exists: `{}`",
            location.local_path.display()
        );
    }
    fs::rename(&location.local_staging_path, &location.local_path).with_context(|| {
        format!(
            "failed to publish frozen snapshot `{}`",
            location.local_path.display()
        )
    })
}

fn publish_snapshot_manifest(
    location: &SnapshotLocation,
    manifest: &FrozenSnapshotManifest,
) -> Result<()> {
    let bytes = manifest.to_json_bytes()?;
    if let Some(uri) = &location.s3_manifest_uri {
        put_s3_object(uri, bytes, "application/json", false)?;
    } else {
        write_json_file_atomic(&location.local_manifest_path, &bytes, false)?;
    }
    Ok(())
}

fn publish_snapshot_pointer(
    location: &SnapshotLocation,
    manifest: &FrozenSnapshotManifest,
) -> Result<()> {
    let bytes = manifest.to_json_bytes()?;
    if let Some(uri) = &location.s3_pointer_uri {
        put_s3_object(uri, bytes, "application/json", true)?;
    } else {
        publish_local_snapshot_pointer(&location.local_pointer_path, manifest)?;
    }
    Ok(())
}

fn publish_snapshot_pointer_if_current_not_newer(
    location: &SnapshotLocation,
    manifest: &FrozenSnapshotManifest,
) -> Result<()> {
    if let Some(current) = read_snapshot_pointer(location)? {
        current.ensure_published()?;
        if current.generation > manifest.generation {
            return Ok(());
        }
        if current.generation == manifest.generation
            && current.snapshot_uri != manifest.snapshot_uri
        {
            bail!(
                "live frozen snapshot pointer for generation {} points at `{}`, not `{}`",
                current.generation,
                current.snapshot_uri,
                manifest.snapshot_uri
            );
        }
    }
    publish_snapshot_pointer(location, manifest)
}

fn read_snapshot_pointer(location: &SnapshotLocation) -> Result<Option<FrozenSnapshotManifest>> {
    let bytes = if let Some(uri) = &location.s3_pointer_uri {
        match get_s3_object_bytes(uri) {
            Ok(bytes) => bytes,
            Err(error) if is_s3_not_found_error(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    } else {
        match fs::read(&location.local_pointer_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to read frozen snapshot pointer `{}`",
                        location.local_pointer_path.display()
                    )
                });
            }
        }
    };
    FrozenSnapshotManifest::from_json_slice(&bytes).map(Some)
}

fn is_s3_not_found_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("NoSuchKey")
        || message.contains("NotFound")
        || message.contains("Not Found")
        || message.contains("404")
}

fn read_snapshot_manifest(location: &SnapshotLocation) -> Result<FrozenSnapshotManifest> {
    let bytes = if let Some(uri) = &location.s3_manifest_uri {
        get_s3_object_bytes(uri)?
    } else {
        fs::read(&location.local_manifest_path).with_context(|| {
            format!(
                "failed to read frozen snapshot manifest `{}`",
                location.local_manifest_path.display()
            )
        })?
    };
    FrozenSnapshotManifest::from_json_slice(&bytes)
}

fn publish_local_snapshot_pointer(
    pointer_path: &Path,
    manifest: &FrozenSnapshotManifest,
) -> Result<()> {
    let bytes = manifest.to_json_bytes()?;
    write_json_file_atomic(pointer_path, &bytes, true)
}

#[cfg(test)]
fn read_local_snapshot_pointer(pointer_path: &Path) -> Result<FrozenSnapshotManifest> {
    let bytes = fs::read(pointer_path).with_context(|| {
        format!(
            "failed to read snapshot pointer `{}`",
            pointer_path.display()
        )
    })?;
    FrozenSnapshotManifest::from_json_slice(&bytes)
}

fn write_json_file_atomic(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create json dir `{}`", parent.display()))?;
    }
    if !overwrite && path.exists() {
        bail!("immutable json object already exists: `{}`", path.display());
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("snapshot.json");
    let tmp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&tmp_path, bytes)
        .with_context(|| format!("failed to write temp json `{}`", tmp_path.display()))?;
    if !overwrite && path.exists() {
        let _ = fs::remove_file(&tmp_path);
        bail!("immutable json object already exists: `{}`", path.display());
    }
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to atomically publish json `{}`", path.display()))
}

fn file_sha256_and_len(path: &Path) -> Result<(String, u64)> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read snapshot `{}`", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let sha256 = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((sha256, bytes.len() as u64))
}

fn copy_ducklake_metadata_tables(catalog_dsn: &str, snapshot_path: &Path) -> Result<()> {
    let catalog_dsn = catalog_dsn_with_env_password(catalog_dsn);
    let conn = Connection::open_in_memory().context("failed to open snapshot exporter DuckDB")?;
    conn.execute_batch("INSTALL sqlite; INSTALL postgres; LOAD sqlite; LOAD postgres;")
        .context("failed to load metadata backend extensions")?;
    let source = attach_metadata_catalog(&conn, &catalog_dsn)?;
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
    let bytes = fs::read(local_path)
        .with_context(|| format!("failed to read snapshot `{}`", local_path.display()))?;
    put_s3_object(uri, bytes, "application/octet-stream", false)
}

fn put_s3_object(
    uri: &str,
    bytes: Vec<u8>,
    content_type: &str,
    allow_overwrite: bool,
) -> Result<()> {
    let parsed = parse_s3_uri(uri)?;
    let uri = uri.to_owned();
    let content_type = content_type.to_owned();
    run_s3_blocking(move |client| async move {
        let mut request = client
            .put_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .content_type(content_type)
            .body(ByteStream::from(bytes));
        if !allow_overwrite {
            request = request.if_none_match("*");
        }
        request
            .send()
            .await
            .with_context(|| format!("failed to upload frozen snapshot object `{uri}`"))?;
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

fn get_s3_object_bytes(uri: &str) -> Result<Vec<u8>> {
    let parsed = parse_s3_uri(uri)?;
    let uri = uri.to_owned();
    run_s3_blocking(move |client| async move {
        let output = client
            .get_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .send()
            .await
            .with_context(|| format!("failed to read S3 object `{uri}`"))?;
        let bytes = output
            .body
            .collect()
            .await
            .with_context(|| format!("failed to read S3 object body `{uri}`"))?;
        Ok(bytes.into_bytes().to_vec())
    })
}

fn run_s3_blocking<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(aws_sdk_s3::Client) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
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

pub(crate) fn postgres_metadata_dsn(catalog_dsn: &str) -> String {
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

pub(crate) fn catalog_dsn_with_env_password(catalog_dsn: &str) -> String {
    if !is_postgres_catalog(catalog_dsn) || postgres_dsn_has_password(catalog_dsn) {
        return catalog_dsn.to_owned();
    }

    let Ok(password) = std::env::var(CATALOG_PASSWORD_ENV) else {
        return catalog_dsn.to_owned();
    };
    if password.is_empty() {
        return catalog_dsn.to_owned();
    }

    if catalog_dsn.starts_with("postgres:") || catalog_dsn.starts_with("ducklake:postgres:") {
        format!(
            "{catalog_dsn} password='{}'",
            escape_libpq_keyword_value(&password)
        )
    } else {
        catalog_dsn.to_owned()
    }
}

fn postgres_dsn_has_password(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.contains(" password=")
        || dsn.starts_with("postgres:password=")
        || dsn.starts_with("postgresql:password=")
        || dsn.contains("://")
            && dsn.split_once("://").is_some_and(|(_, rest)| {
                rest.split_once('@')
                    .map(|(authority, _)| authority.contains(':'))
                    .unwrap_or(false)
            })
}

fn escape_libpq_keyword_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock should not be poisoned")
    }

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

    #[test]
    fn snapshot_location_uses_immutable_generation_path_and_live_pointer() -> Result<()> {
        let root = unique_temp_dir("snapshot-location")?;
        let data_path = root.join("data");
        let location =
            SnapshotLocation::for_data_path_and_generation(&data_path.display().to_string(), 42)?;

        assert_eq!(
            location.local_path,
            data_path
                .join("gold")
                .join("catalog-snapshot")
                .join("generations")
                .join("00000000000000000042")
                .join("spur_context.ducklake")
        );
        assert_eq!(
            location.local_manifest_path,
            data_path
                .join("gold")
                .join("catalog-snapshot")
                .join("generations")
                .join("00000000000000000042")
                .join("manifest.json")
        );
        assert_eq!(
            location.local_pointer_path,
            data_path
                .join("gold")
                .join("catalog-snapshot")
                .join("current.json")
        );
        assert_eq!(location.s3_uri, None);
        Ok(())
    }

    #[test]
    fn pointer_publish_refuses_incomplete_manifest_without_live_marker() -> Result<()> {
        let root = unique_temp_dir("snapshot-pointer-incomplete")?;
        let pointer_path = root.join("current.json");
        let manifest = FrozenSnapshotManifest {
            schema_version: 1,
            generation: 7,
            snapshot_uri: root.join("snapshot.ducklake").display().to_string(),
            data_path: root.join("data").display().to_string(),
            sha256: "abc123".to_owned(),
            bytes: 12,
            status: "staging".to_owned(),
        };

        let err = publish_local_snapshot_pointer(&pointer_path, &manifest)
            .expect_err("staging manifests must not become the live pointer");

        assert!(
            format!("{err:#}").contains("published"),
            "unexpected error: {err:#}"
        );
        assert!(
            !pointer_path.exists(),
            "failed publishes must not leave a pointer"
        );
        Ok(())
    }

    #[test]
    fn rollback_rewrites_pointer_to_previous_published_generation() -> Result<()> {
        let root = unique_temp_dir("snapshot-pointer-rollback")?;
        let pointer_path = root.join("current.json");
        let first = FrozenSnapshotManifest::published(
            10,
            root.join("generations/10/spur_context.ducklake")
                .display()
                .to_string(),
            root.join("data").display().to_string(),
            "sha10".to_owned(),
            10,
        );
        let second = FrozenSnapshotManifest::published(
            11,
            root.join("generations/11/spur_context.ducklake")
                .display()
                .to_string(),
            root.join("data").display().to_string(),
            "sha11".to_owned(),
            11,
        );

        publish_local_snapshot_pointer(&pointer_path, &second)?;
        publish_local_snapshot_pointer(&pointer_path, &first)?;

        let current = read_local_snapshot_pointer(&pointer_path)?;
        assert_eq!(current.generation, 10);
        assert_eq!(current.snapshot_uri, first.snapshot_uri);
        assert_eq!(current.status, "published");
        Ok(())
    }

    #[test]
    fn export_does_not_move_live_pointer_to_older_generation() -> Result<()> {
        let root = unique_temp_dir("snapshot-pointer-monotonic")?;
        let data_path = root.join("data");
        let data_path = data_path.display().to_string();
        let older_location = SnapshotLocation::for_data_path_and_generation(&data_path, 10)?;
        let newer_location = SnapshotLocation::for_data_path_and_generation(&data_path, 11)?;
        seed_published_snapshot(&older_location, &data_path, 10)?;
        seed_published_snapshot(&newer_location, &data_path, 11)?;

        let newer_manifest = read_snapshot_manifest(&newer_location)?;
        publish_local_snapshot_pointer(&newer_location.local_pointer_path, &newer_manifest)?;

        export_frozen_snapshot("sqlite:/unused/catalog.sqlite", &data_path, 10)?;

        let current = read_local_snapshot_pointer(&newer_location.local_pointer_path)?;
        assert_eq!(current.generation, 11);
        assert_eq!(current.snapshot_uri, newer_manifest.snapshot_uri);
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

    fn seed_published_snapshot(
        location: &SnapshotLocation,
        data_path: &str,
        generation: i64,
    ) -> Result<()> {
        if let Some(parent) = location.local_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create snapshot dir `{}`", parent.display()))?;
        }
        fs::write(
            &location.local_path,
            format!("snapshot generation {generation}"),
        )
        .with_context(|| format!("write snapshot `{}`", location.local_path.display()))?;
        let (sha256, bytes) = file_sha256_and_len(&location.local_path)?;
        let manifest = FrozenSnapshotManifest::published(
            generation,
            location.snapshot_uri.clone(),
            data_path.to_owned(),
            sha256,
            bytes,
        );
        publish_snapshot_manifest(location, &manifest)
    }

    #[test]
    fn postgres_catalog_dsn_uses_secret_password_env() {
        let _guard = lock_env();
        let previous = std::env::var_os("SPUR_CATALOG_PASSWORD");
        std::env::set_var("SPUR_CATALOG_PASSWORD", "sec'ret\\value");

        let dsn = catalog_dsn_with_env_password(
            "postgres:host=aurora.example port=5432 dbname=spur_context user=spur_context",
        );

        match previous {
            Some(value) => std::env::set_var("SPUR_CATALOG_PASSWORD", value),
            None => std::env::remove_var("SPUR_CATALOG_PASSWORD"),
        }

        assert_eq!(
            dsn,
            "postgres:host=aurora.example port=5432 dbname=spur_context user=spur_context password='sec\\'ret\\\\value'"
        );
    }

    #[test]
    fn postgres_catalog_dsn_keeps_existing_password() {
        let _guard = lock_env();
        let previous = std::env::var_os("SPUR_CATALOG_PASSWORD");
        std::env::set_var("SPUR_CATALOG_PASSWORD", "from-secret");

        let dsn = catalog_dsn_with_env_password("postgres:host=aurora password=already-present");

        match previous {
            Some(value) => std::env::set_var("SPUR_CATALOG_PASSWORD", value),
            None => std::env::remove_var("SPUR_CATALOG_PASSWORD"),
        }

        assert_eq!(dsn, "postgres:host=aurora password=already-present");
    }
}
