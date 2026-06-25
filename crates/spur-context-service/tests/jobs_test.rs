use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::json;
use spur_context_service::jobs::{
    CreateJobOutcome, CreateJobRequest, JobKey, JobRecord, JobStatus, JobStore,
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
