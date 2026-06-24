use anyhow::{anyhow, Context as _, Result};
use duckdb::{params, Connection};

const DEFAULT_DATA_PATH: &str = "s3://spur-context/data/";
const INDEX_JOBS_SQL: &str = include_str!("../sql/index_jobs.sql");

fn is_remote_catalog(catalog_dsn: &str) -> bool {
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
        let resolver = Self { conn };
        match ensure_index_jobs_table(&resolver.conn) {
            Ok(()) => eprintln!("[catalog] ensured index_jobs table exists"),
            Err(error) => {
                eprintln!("[catalog] warning: failed to ensure index_jobs table exists: {error:#}");
            }
        }
        resolver
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
        let mut stmt = self
            .conn
            .prepare(
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
                FROM package_catalog
                WHERE source = ? AND package = ?
                ORDER BY
                    CASE WHEN revision_kind = 'semver' THEN 0 ELSE 1 END,
                    semver_major DESC NULLS LAST,
                    semver_minor DESC NULLS LAST,
                    semver_patch DESC NULLS LAST,
                    indexed_at DESC NULLS LAST,
                    revision DESC
                ",
            )
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
        optional_no_rows(
            self.conn.query_row(
                r"
                SELECT revision
                FROM refs
                WHERE source = ? AND package = ? AND ref_name = ?
                LIMIT 1
                ",
                params![source, package, ref_name],
                |row| row.get(0),
            ),
            "failed to resolve catalog ref",
        )
    }

    fn lookup_revision(
        &self,
        source: &str,
        package: &str,
        revision: &str,
    ) -> Result<Option<ResolvedRevision>> {
        optional_no_rows(
            self.conn.query_row(
                r"
                SELECT source, package, revision, revision_kind, snapshot_id
                FROM package_catalog
                WHERE source = ? AND package = ? AND revision = ?
                LIMIT 1
                ",
                params![source, package, revision],
                |row| {
                    Ok(ResolvedRevision {
                        source: row.get(0)?,
                        package: row.get(1)?,
                        revision: row.get(2)?,
                        revision_kind: row.get(3)?,
                        snapshot_id: row.get(4)?,
                    })
                },
            ),
            "failed to resolve catalog revision",
        )
    }
}

pub fn connect_ducklake(catalog_dsn: &str) -> Result<Connection> {
    connect_ducklake_with_data_path(catalog_dsn, DEFAULT_DATA_PATH)
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

    attach_ducklake(&conn, catalog_dsn, data_path)?;
    Ok(conn)
}

pub fn ensure_index_jobs_table(conn: &Connection) -> Result<()> {
    // The DuckLake catalog may be attached READ_ONLY; index_jobs is an
    // operational table that lives in the local in-memory database, not in
    // the attached catalog. Switch to the default database before creating,
    // then switch back to the catalog.
    let _ = conn.execute_batch("USE memory;");

    let result = match conn.execute_batch(INDEX_JOBS_SQL) {
        Ok(()) => Ok(()),
        Err(error) if is_ducklake_constraint_error(&error) => {
            eprintln!(
                "[catalog] warning: bundled index_jobs DDL failed; retrying without DuckLake-unsupported constraints: {error:#}"
            );
            let fallback_sql = ducklake_compatible_index_jobs_sql();
            conn.execute_batch(&fallback_sql)
                .context("failed to execute DuckLake-compatible index_jobs DDL")
        }
        Err(error) => Err(error).context("failed to execute index_jobs DDL"),
    };

    // Restore the catalog as the default database for query tools.
    let _ = conn.execute_batch("USE spur_context;");
    result
}

fn is_ducklake_constraint_error(error: &duckdb::Error) -> bool {
    error
        .to_string()
        .contains("PRIMARY KEY/UNIQUE constraints are not supported in DuckLake")
}

fn ducklake_compatible_index_jobs_sql() -> String {
    INDEX_JOBS_SQL
        .replace("job_id TEXT PRIMARY KEY,", "job_id TEXT,")
        .replace(
            ",\n    UNIQUE(source, package, revision, source_url_hash)",
            "",
        )
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

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}
