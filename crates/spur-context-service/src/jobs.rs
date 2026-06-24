//! index_jobs table CRUD and status transitions.

use std::{error::Error as StdError, fmt};

use duckdb::{params, Connection};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, JobsError>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
    Partial,
}

impl JobStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Partial => "partial",
        }
    }

    fn from_db(value: String, column: usize) -> duckdb::Result<Self> {
        match value.as_str() {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "partial" => Ok(Self::Partial),
            _ => Err(conversion_error(column, InvalidJobStatus(value))),
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobRow {
    pub job_id: String,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub status: JobStatus,
    pub execution_arn: Option<String>,
    pub error: Option<String>,
    pub snapshot_id: Option<i64>,
    pub row_counts: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("database error: {0}")]
    Db(#[from] duckdb::Error),
    #[error("conflicting index job")]
    Conflict,
    #[error("index job not found")]
    NotFound,
}

#[derive(Debug, Clone)]
pub struct InsertParams {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub execution_arn: Option<String>,
}

pub fn insert(conn: &Connection, params: InsertParams) -> Result<JobRow> {
    let job_id = Uuid::new_v4().to_string();

    let result = conn.execute(
        r"
        INSERT INTO index_jobs (
            job_id,
            source,
            package,
            revision,
            source_url,
            source_url_hash,
            status,
            execution_arn,
            error,
            snapshot_id,
            row_counts,
            created_at,
            updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ",
        params![
            job_id,
            params.source,
            params.package,
            params.revision,
            params.source_url,
            params.source_url_hash,
            JobStatus::Queued.as_str(),
            params.execution_arn,
        ],
    );

    match result {
        Ok(_) => lookup(conn, &job_id)?.ok_or(JobsError::NotFound),
        Err(error) if is_constraint_violation(&error) => Err(JobsError::Conflict),
        Err(error) => Err(error.into()),
    }
}

pub fn find_active(
    conn: &Connection,
    source: &str,
    package: &str,
    revision: &str,
    source_url_hash: &str,
) -> Result<Option<JobRow>> {
    optional_no_rows(
        conn.query_row(
            &format!(
                r"
                {}
                WHERE source = ?
                  AND package = ?
                  AND revision = ?
                  AND source_url_hash = ?
                  AND status IN ('queued', 'running')
                ORDER BY updated_at DESC
                LIMIT 1
                ",
                select_jobs_sql()
            ),
            params![source, package, revision, source_url_hash],
            job_row_from_row,
        ),
        "failed to find active index job",
    )
}

pub fn find_any(
    conn: &Connection,
    source: &str,
    package: &str,
    revision: &str,
    source_url_hash: &str,
) -> Result<Option<JobRow>> {
    optional_no_rows(
        conn.query_row(
            &format!(
                r"
                {}
                WHERE source = ?
                  AND package = ?
                  AND revision = ?
                  AND source_url_hash = ?
                ORDER BY updated_at DESC
                LIMIT 1
                ",
                select_jobs_sql()
            ),
            params![source, package, revision, source_url_hash],
            job_row_from_row,
        ),
        "failed to find index job",
    )
}

pub fn update_status(
    conn: &Connection,
    job_id: &str,
    status: JobStatus,
    snapshot_id: Option<i64>,
    error: Option<&str>,
    row_counts: Option<serde_json::Value>,
) -> Result<()> {
    let row_counts_json = row_counts.map(|value| value.to_string());
    let changed = conn.execute(
        r"
        UPDATE index_jobs
        SET status = ?,
            snapshot_id = ?,
            error = ?,
            row_counts = CAST(? AS JSON),
            updated_at = CURRENT_TIMESTAMP
        WHERE job_id = ?
        ",
        params![
            status.as_str(),
            snapshot_id,
            error,
            row_counts_json.as_deref(),
            job_id,
        ],
    )?;

    if changed == 0 {
        return Err(JobsError::NotFound);
    }

    Ok(())
}

pub fn lookup(conn: &Connection, job_id: &str) -> Result<Option<JobRow>> {
    optional_no_rows(
        conn.query_row(
            &format!(
                r"
                {}
                WHERE job_id = ?
                LIMIT 1
                ",
                select_jobs_sql()
            ),
            params![job_id],
            job_row_from_row,
        ),
        "failed to lookup index job",
    )
}

fn select_jobs_sql() -> &'static str {
    r"
    SELECT
        job_id,
        source,
        package,
        revision,
        source_url,
        source_url_hash,
        status,
        execution_arn,
        error,
        snapshot_id,
        CAST(row_counts AS VARCHAR),
        CAST(created_at AS VARCHAR),
        CAST(updated_at AS VARCHAR)
    FROM index_jobs
    "
}

fn job_row_from_row(row: &duckdb::Row<'_>) -> duckdb::Result<JobRow> {
    let status = JobStatus::from_db(row.get(6)?, 6)?;
    let row_counts = match row.get::<_, Option<String>>(10)? {
        Some(raw) => Some(serde_json::from_str(&raw).map_err(|error| conversion_error(10, error))?),
        None => None,
    };

    Ok(JobRow {
        job_id: row.get(0)?,
        source: row.get(1)?,
        package: row.get(2)?,
        revision: row.get(3)?,
        source_url: row.get(4)?,
        source_url_hash: row.get(5)?,
        status,
        execution_arn: row.get(7)?,
        error: row.get(8)?,
        snapshot_id: row.get(9)?,
        row_counts,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn optional_no_rows<T>(result: duckdb::Result<T>, _context: &'static str) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn is_constraint_violation(error: &duckdb::Error) -> bool {
    match error {
        duckdb::Error::DuckDBFailure(ffi_error, message) => {
            ffi_error.code == duckdb::ffi::ErrorCode::ConstraintViolation
                || message
                    .as_deref()
                    .is_some_and(|message| message.contains("Constraint Error"))
        }
        _ => false,
    }
}

fn conversion_error(column: usize, error: impl StdError + Send + Sync + 'static) -> duckdb::Error {
    duckdb::Error::FromSqlConversionFailure(column, duckdb::types::Type::Text, Box::new(error))
}

#[derive(Debug)]
struct InvalidJobStatus(String);

impl fmt::Display for InvalidJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid index job status: {}", self.0)
    }
}

impl StdError for InvalidJobStatus {}
