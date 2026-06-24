use anyhow::{Context as _, Result};
use duckdb::Connection;
use serde_json::json;
use spur_context_service::jobs::{
    find_active, find_any, insert, lookup, update_status, InsertParams, JobStatus, JobsError,
};

const SOURCE: &str = "git:custom";
const PACKAGE: &str = "serde";
const REVISION: &str = "main";
const SOURCE_URL: &str = "https://github.com/serde-rs/serde";
const SOURCE_URL_HASH: &str = "sha256:serde";

#[test]
fn insert_and_lookup_round_trips_job_row() -> Result<()> {
    let conn = setup_conn()?;

    let inserted = insert(&conn, insert_params()).context("insert job")?;
    let found = lookup(&conn, &inserted.job_id)
        .context("lookup job")?
        .context("job should exist")?;

    assert_eq!(found.job_id, inserted.job_id);
    assert_eq!(found.source, SOURCE);
    assert_eq!(found.package, PACKAGE);
    assert_eq!(found.revision, REVISION);
    assert_eq!(found.source_url, SOURCE_URL);
    assert_eq!(found.source_url_hash, SOURCE_URL_HASH);
    assert_eq!(found.status, JobStatus::Queued);
    assert_eq!(found.execution_arn.as_deref(), Some("arn:queued"));
    assert_eq!(found.error, None);
    assert_eq!(found.snapshot_id, None);
    assert_eq!(found.row_counts, None);
    assert!(!found.created_at.is_empty());
    assert!(!found.updated_at.is_empty());
    Ok(())
}

#[test]
fn find_active_returns_queued_job() -> Result<()> {
    let conn = setup_conn()?;
    let inserted = insert(&conn, insert_params()).context("insert job")?;

    let active = find_active(&conn, SOURCE, PACKAGE, REVISION, SOURCE_URL_HASH)
        .context("find active job")?
        .context("queued job should be active")?;

    assert_eq!(active.job_id, inserted.job_id);
    assert_eq!(active.status, JobStatus::Queued);
    Ok(())
}

#[test]
fn find_active_returns_none_for_complete_job() -> Result<()> {
    let conn = setup_conn()?;
    let inserted = insert(&conn, insert_params()).context("insert job")?;
    update_status(
        &conn,
        &inserted.job_id,
        JobStatus::Complete,
        Some(42),
        None,
        Some(json!({ "nodes": 3, "edges": 2 })),
    )
    .context("complete job")?;

    let active = find_active(&conn, SOURCE, PACKAGE, REVISION, SOURCE_URL_HASH)
        .context("find active job")?;
    let any = find_any(&conn, SOURCE, PACKAGE, REVISION, SOURCE_URL_HASH)
        .context("find any job")?
        .context("completed job should still be findable")?;

    assert_eq!(active, None);
    assert_eq!(any.status, JobStatus::Complete);
    assert_eq!(any.snapshot_id, Some(42));
    assert_eq!(any.row_counts, Some(json!({ "nodes": 3, "edges": 2 })));
    Ok(())
}

#[test]
fn duplicate_insert_returns_conflict_for_dedup_race() -> Result<()> {
    let conn = setup_conn()?;
    insert(&conn, insert_params()).context("insert initial job")?;

    let err = insert(&conn, insert_params()).expect_err("duplicate should conflict");

    assert!(
        matches!(err, JobsError::Conflict),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn update_status_transitions_job() -> Result<()> {
    let conn = setup_conn()?;
    let inserted = insert(&conn, insert_params()).context("insert job")?;

    update_status(
        &conn,
        &inserted.job_id,
        JobStatus::Running,
        None,
        None,
        None,
    )
    .context("mark running")?;
    assert_eq!(
        lookup(&conn, &inserted.job_id)
            .context("lookup running")?
            .context("running job should exist")?
            .status,
        JobStatus::Running
    );

    update_status(
        &conn,
        &inserted.job_id,
        JobStatus::Failed,
        None,
        Some("fetch: timeout"),
        Some(json!({ "attempts": 2 })),
    )
    .context("mark failed")?;

    let failed = lookup(&conn, &inserted.job_id)
        .context("lookup failed")?
        .context("failed job should exist")?;
    assert_eq!(failed.status, JobStatus::Failed);
    assert_eq!(failed.error.as_deref(), Some("fetch: timeout"));
    assert_eq!(failed.row_counts, Some(json!({ "attempts": 2 })));
    Ok(())
}

fn setup_conn() -> Result<Connection> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS index_jobs (
            job_id TEXT PRIMARY KEY,
            source TEXT,
            package TEXT,
            revision TEXT,
            source_url TEXT,
            source_url_hash TEXT,
            status TEXT,
            execution_arn TEXT,
            error TEXT,
            snapshot_id BIGINT,
            row_counts JSON,
            created_at TIMESTAMPTZ,
            updated_at TIMESTAMPTZ,
            UNIQUE(source, package, revision, source_url_hash)
        );
        "#,
    )
    .context("create index_jobs table")?;
    Ok(conn)
}

fn insert_params() -> InsertParams {
    InsertParams {
        source: SOURCE.to_string(),
        package: PACKAGE.to_string(),
        revision: REVISION.to_string(),
        source_url: SOURCE_URL.to_string(),
        source_url_hash: SOURCE_URL_HASH.to_string(),
        execution_arn: Some("arn:queued".to_string()),
    }
}
