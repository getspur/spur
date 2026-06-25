use anyhow::{Context as _, Result};
use async_trait::async_trait;
use duckdb::Connection;
use serde_json::json;
use spur_context_service::jobs::{
    find_active, find_any, insert, lookup, update_status, CreateJobOutcome, CreateJobRequest,
    InsertParams, JobKey, JobRecord, JobStatus, JobStore, JobsError,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

const SOURCE: &str = "git:custom";
const PACKAGE: &str = "serde";
const REVISION: &str = "main";
const SOURCE_URL: &str = "https://github.com/serde-rs/serde";
const SOURCE_URL_HASH: &str = "sha256:serde";

#[test]
fn create_or_get_active_job_dedupes_identical_requests() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let first = store
            .create_or_get_active_job(create_job_request())
            .await
            .context("create first job")?;
        let second = store
            .create_or_get_active_job(create_job_request())
            .await
            .context("dedupe second job")?;

        let CreateJobOutcome::Created(first) = first else {
            anyhow::bail!("first create should create a job");
        };
        let CreateJobOutcome::Existing(second) = second else {
            anyhow::bail!("second create should return existing job");
        };

        assert_eq!(second.job_id, first.job_id);
        assert_eq!(second.status, JobStatus::Queued);
        Ok(())
    })
}

#[test]
fn failed_job_releases_dedupe_for_retry() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let first = expect_created(
            store
                .create_or_get_active_job(create_job_request())
                .await
                .context("create initial job")?,
        )?;

        store
            .mark_failed(&first.job_id, "fetch_failed", "repository unavailable")
            .await
            .context("mark failed")?;
        let retry = expect_created(
            store
                .create_or_get_active_job(create_job_request())
                .await
                .context("retry after failed job")?,
        )?;

        assert_ne!(retry.job_id, first.job_id);
        assert_eq!(retry.status, JobStatus::Queued);
        Ok(())
    })
}

#[test]
fn record_execution_started_persists_execution_arn() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let created = expect_created(
            store
                .create_or_get_active_job(create_job_request())
                .await
                .context("create job")?,
        )?;

        let updated = store
            .record_execution_started(&created.job_id, "arn:aws:states:execution/test")
            .await
            .context("record execution arn")?;
        let found = store
            .lookup_job(&created.job_id)
            .await
            .context("lookup job")?
            .context("job should exist")?;

        assert_eq!(
            updated.execution_arn.as_deref(),
            Some("arn:aws:states:execution/test")
        );
        assert_eq!(found.execution_arn, updated.execution_arn);
        Ok(())
    })
}

#[test]
fn complete_job_preserves_status_response_fields() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let created = expect_created(
            store
                .create_or_get_active_job(create_job_request())
                .await
                .context("create job")?,
        )?;

        let completed = store
            .mark_complete(&created.job_id, 42, json!({ "nodes": 3, "edges": 2 }))
            .await
            .context("mark complete")?;

        assert_eq!(completed.job_id, created.job_id);
        assert_eq!(completed.status, JobStatus::Complete);
        assert_eq!(completed.source, SOURCE);
        assert_eq!(completed.package, PACKAGE);
        assert_eq!(completed.revision, REVISION);
        assert_eq!(completed.snapshot_id, Some(42));
        assert_eq!(
            completed.row_counts,
            Some(json!({ "nodes": 3, "edges": 2 }))
        );
        assert_eq!(completed.error_code, None);
        assert_eq!(completed.error_detail, None);
        Ok(())
    })
}

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

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn expect_created(outcome: CreateJobOutcome) -> Result<JobRecord> {
    match outcome {
        CreateJobOutcome::Created(record) => Ok(record),
        CreateJobOutcome::Existing(record) => {
            anyhow::bail!("expected newly created job, got {}", record.job_id)
        }
    }
}

fn create_job_request() -> CreateJobRequest {
    CreateJobRequest {
        source: SOURCE.to_string(),
        package: PACKAGE.to_string(),
        revision: REVISION.to_string(),
        source_url: SOURCE_URL.to_string(),
        source_url_hash: SOURCE_URL_HASH.to_string(),
        source_kind: "git".to_string(),
        caller_id: "jobs-test".to_string(),
    }
}

#[derive(Default)]
struct FakeJobStore {
    next_id: AtomicU64,
    state: Mutex<FakeJobState>,
}

#[derive(Default)]
struct FakeJobState {
    jobs: HashMap<String, JobRecord>,
    dedupe: HashMap<JobKey, String>,
}

#[async_trait]
impl JobStore for FakeJobStore {
    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> spur_context_service::jobs::Result<CreateJobOutcome> {
        let key = request.key();
        let mut state = self.state.lock().expect("fake store lock");
        if let Some(job_id) = state.dedupe.get(&key) {
            if let Some(record) = state.jobs.get(job_id) {
                return Ok(CreateJobOutcome::Existing(record.clone()));
            }
        }

        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let record = JobRecord {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            source: request.source,
            package: request.package,
            revision: request.revision,
            source_url: request.source_url,
            source_url_hash: request.source_url_hash,
            source_kind: request.source_kind,
            caller_id: request.caller_id,
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
        };
        state.dedupe.insert(key, job_id.clone());
        state.jobs.insert(job_id, record.clone());
        Ok(CreateJobOutcome::Created(record))
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        self.update_job(job_id, |record| {
            record.execution_arn = Some(execution_arn.to_string());
            record.updated_at = "started".to_string();
        })
    }

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        self.update_job(job_id, |record| {
            record.status = status;
            record.stage = Some(stage.to_string());
            record.updated_at = "stage".to_string();
        })
    }

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Complete;
            record.snapshot_id = Some(snapshot_id);
            record.row_counts = Some(row_counts);
            record.error_code = None;
            record.error_detail = None;
            record.updated_at = "complete".to_string();
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn mark_failed(
        &self,
        job_id: &str,
        code: &str,
        detail: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Failed;
            record.error_code = Some(code.to_string());
            record.error_detail = Some(detail.to_string());
            record.updated_at = "failed".to_string();
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn lookup_job(
        &self,
        job_id: &str,
    ) -> spur_context_service::jobs::Result<Option<JobRecord>> {
        Ok(self
            .state
            .lock()
            .expect("fake store lock")
            .jobs
            .get(job_id)
            .cloned())
    }

    async fn release_dedupe_if_owner(
        &self,
        record: &JobRecord,
    ) -> spur_context_service::jobs::Result<()> {
        let mut state = self.state.lock().expect("fake store lock");
        let key = record.key();
        if state.dedupe.get(&key).is_some_and(|job_id| job_id == &record.job_id) {
            state.dedupe.remove(&key);
        }
        Ok(())
    }
}

impl FakeJobStore {
    fn update_job(
        &self,
        job_id: &str,
        update: impl FnOnce(&mut JobRecord),
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let mut state = self.state.lock().expect("fake store lock");
        let record = state
            .jobs
            .get_mut(job_id)
            .ok_or(spur_context_service::jobs::JobsError::NotFound)?;
        update(record);
        Ok(record.clone())
    }
}
