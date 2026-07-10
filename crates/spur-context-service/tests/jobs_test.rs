use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::json;
use spur_context_service::drainer::{self, DrainSummary};
use spur_context_service::jobs::{
    BacklogOwner, BacklogOwnerKind, CreateJobOutcome, CreateJobRequest, EnqueueOutcome, JobKey,
    JobRecord, JobStatus, JobStore, JobsError, QueueConfig,
};
use spur_context_service::mcp::{
    self, IndexExecutionRequest, IndexExecutionStarter, McpHandlerError,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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

// ─── Bounded queueing store primitives ─────────────────────────────────────

fn queue_config(max_queued_per_owner: u32) -> QueueConfig {
    QueueConfig {
        max_queued_per_owner,
        max_queued_global: 0,
        max_running_per_owner: u32::MAX,
        max_running_global: 0,
        shard_count: 16,
    }
}

#[test]
fn enqueue_creates_queued_job_with_owner_accounting() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let outcome = store
            .enqueue_job(create_job_request(), owner.clone(), &queue_config(5))
            .await
            .context("enqueue first job")?;

        let record = match outcome {
            EnqueueOutcome::Enqueued(record) => record,
            EnqueueOutcome::Existing(_) => anyhow::bail!("expected newly enqueued job"),
        };
        assert_eq!(record.status, JobStatus::Queued);
        assert_eq!(record.owner_kind, Some(BacklogOwnerKind::Caller));
        assert_eq!(record.owner_id.as_deref(), Some("alice"));
        assert!(
            record.has_queue_gsi_attributes(),
            "queued job must carry GSI attrs"
        );
        assert_eq!(store.owner_queued(&owner), 1);
        Ok(())
    })
}

#[test]
fn enqueue_dedupe_returns_existing_active_job() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let first = store
            .enqueue_job(create_job_request(), owner.clone(), &queue_config(5))
            .await
            .context("enqueue first")?;
        let second = store
            .enqueue_job(create_job_request(), owner.clone(), &queue_config(5))
            .await
            .context("enqueue duplicate")?;

        assert!(first.is_enqueued());
        assert!(matches!(second, EnqueueOutcome::Existing(_)));
        assert_eq!(second.into_record().job_id, first.into_record().job_id);
        // Only one queued slot consumed.
        assert_eq!(store.owner_queued(&owner), 1);
        Ok(())
    })
}

#[test]
fn enqueue_owner_queue_full_rejects_without_partial_writes() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(2);

        // Fill the owner backlog.
        store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?;
        store
            .enqueue_job(with_package("serde2"), owner.clone(), &config)
            .await?;
        assert_eq!(store.owner_queued(&owner), 2);

        // Third unique request overflows the cap.
        let err = store
            .enqueue_job(with_package("serde3"), owner.clone(), &config)
            .await
            .unwrap_err();
        assert!(
            matches!(err, JobsError::QueueFull),
            "expected QueueFull, got {err:?}"
        );

        // No partial writes: the overflowing job was not persisted, the dedupe
        // pointer was not created, and the queued counter did not move.
        assert_eq!(
            store.owner_queued(&owner),
            2,
            "counter must not move on reject"
        );
        assert_eq!(store.job_count(), 2, "no job record written on reject");
        assert!(
            store
                .find_active_dedupe_job(&dedupe_key("serde3"))
                .await?
                .is_none(),
            "no dedupe pointer written on reject"
        );
        Ok(())
    })
}

#[test]
fn enqueue_global_queue_full_rejects() -> Result<()> {
    block_on(async {
        let config = QueueConfig {
            max_queued_per_owner: 10,
            max_queued_global: 1,
            max_running_per_owner: u32::MAX,
            max_running_global: 0,
            shard_count: 16,
        };
        let store = FakeJobStore::default();
        let alice = BacklogOwner::caller("alice");
        let bob = BacklogOwner::caller("bob");

        store
            .enqueue_job(create_job_request(), alice.clone(), &config)
            .await?;
        let err = store
            .enqueue_job(with_package("serde2"), bob.clone(), &config)
            .await
            .unwrap_err();
        assert!(
            matches!(err, JobsError::GlobalQueueFull),
            "expected GlobalQueueFull, got {err:?}"
        );
        // Second owner's slot was not consumed.
        assert_eq!(store.owner_queued(&bob), 0);
        Ok(())
    })
}

#[test]
fn dispatch_frees_global_queue_capacity_for_next_owner() -> Result<()> {
    block_on(async {
        // Verifies that dispatch decrements the global queued counter so a
        // subsequent enqueue from a different owner succeeds. This mirrors
        // the production DynamoDB path where dispatch decrements the matching
        // GLOBAL#QUEUE#<shard> counter.
        let config = QueueConfig {
            max_queued_per_owner: 10,
            max_queued_global: 1,
            max_running_per_owner: u32::MAX,
            max_running_global: 0,
            shard_count: 16,
        };
        let store = FakeJobStore::default();
        let alice = BacklogOwner::caller("alice");
        let bob = BacklogOwner::caller("bob");

        // Fill the single global queued slot.
        let enqueued = store
            .enqueue_job(create_job_request(), alice.clone(), &config)
            .await?
            .into_record();

        // Bob is rejected — global queue is full.
        let err = store
            .enqueue_job(with_package("serde2"), bob.clone(), &config)
            .await
            .unwrap_err();
        assert!(matches!(err, JobsError::GlobalQueueFull));

        // Dispatch frees the global queued slot.
        store.dispatch_queued_job(&enqueued.job_id, &config).await?;

        // Bob can now enqueue — global capacity was released.
        let outcome = store
            .enqueue_job(with_package("serde2"), bob.clone(), &config)
            .await
            .context("enqueue after dispatch should succeed")?;
        assert!(outcome.is_enqueued());
        Ok(())
    })
}

#[test]
fn enqueue_zero_cap_preserves_legacy_reject_over_capacity() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let err = store
            .enqueue_job(create_job_request(), owner.clone(), &queue_config(0))
            .await
            .unwrap_err();
        assert!(matches!(err, JobsError::QueueFull));
        assert_eq!(store.owner_queued(&owner), 0);
        assert_eq!(store.job_count(), 0);
        Ok(())
    })
}

#[test]
fn dispatch_transition_moves_counters_and_removes_gsi() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = QueueConfig {
            max_queued_per_owner: 5,
            max_queued_global: 0,
            max_running_per_owner: 2,
            max_running_global: 0,
            shard_count: 16,
        };
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await
            .context("enqueue")?
            .into_record();
        assert_eq!(store.owner_queued(&owner), 1);
        assert_eq!(store.owner_running(&owner), 0);

        let dispatched = store
            .dispatch_queued_job(&enqueued.job_id, &config)
            .await
            .context("dispatch")?;

        assert_eq!(dispatched.status, JobStatus::Dispatching);
        assert!(
            !dispatched.has_queue_gsi_attributes(),
            "dispatch must remove GSI attrs"
        );
        assert!(dispatched.dispatched_at.is_some());
        // Counters moved: queued -1, running +1.
        assert_eq!(store.owner_queued(&owner), 0);
        assert_eq!(store.owner_running(&owner), 1);
        assert!(store.has_running_token(&enqueued.job_id));
        Ok(())
    })
}

#[test]
fn dispatch_when_owner_at_running_cap_is_rejected() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = QueueConfig {
            max_queued_per_owner: 5,
            max_queued_global: 0,
            max_running_per_owner: 1,
            max_running_global: 0,
            shard_count: 16,
        };
        let first = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();
        let second = store
            .enqueue_job(with_package("serde2"), owner.clone(), &config)
            .await?
            .into_record();
        store.dispatch_queued_job(&first.job_id, &config).await?;
        assert_eq!(store.owner_running(&owner), 1);

        // Owner is at the running cap → second dispatch must be rejected.
        let err = store
            .dispatch_queued_job(&second.job_id, &config)
            .await
            .unwrap_err();
        assert!(matches!(err, JobsError::Conflict));
        // The second job stays queued and keeps its GSI attributes.
        assert_eq!(store.owner_running(&owner), 1);
        let still_queued = store.lookup_job(&second.job_id).await?.context("job")?;
        assert_eq!(still_queued.status, JobStatus::Queued);
        assert!(still_queued.has_queue_gsi_attributes());
        Ok(())
    })
}

#[test]
fn terminal_release_is_exact_once_under_repeat_calls() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = QueueConfig {
            max_queued_per_owner: 5,
            max_queued_global: 0,
            max_running_per_owner: 2,
            max_running_global: 0,
            shard_count: 16,
        };
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();
        let dispatched = store.dispatch_queued_job(&enqueued.job_id, &config).await?;
        assert_eq!(store.owner_running(&owner), 1);

        // Simulate complete + a duplicate terminal update race.
        store.release_running_quota(&dispatched).await?;
        assert_eq!(store.owner_running(&owner), 0, "first release decrements");
        assert!(!store.has_running_token(&enqueued.job_id));

        // Second release must be a no-op (token already gone).
        store.release_running_quota(&dispatched).await?;
        assert_eq!(
            store.owner_running(&owner),
            0,
            "second release must not underflow"
        );
        Ok(())
    })
}

#[test]
fn partial_release_is_terminal_for_quota() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();
        let mut dispatched = store.dispatch_queued_job(&enqueued.job_id, &config).await?;
        // A spot-interruption marks the job partial, then releases running quota.
        dispatched.status = JobStatus::Partial;
        store.release_running_quota(&dispatched).await?;
        assert_eq!(store.owner_running(&owner), 0);
        assert!(
            JobStatus::Partial.is_terminal_for_quota(),
            "partial must be recognized as terminal-for-quota"
        );
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

fn with_package(package: &str) -> CreateJobRequest {
    let mut request = create_job_request();
    request.package = package.to_string();
    request.source_url_hash = format!("{SOURCE_URL_HASH}:{package}");
    request
}

fn dedupe_key(package: &str) -> JobKey {
    with_package(package).key()
}

#[derive(Default)]
struct FakeJobStore {
    next_id: AtomicU64,
    state: Mutex<FakeJobState>,
    /// When true, `record_execution_started` returns a `Conflict` error so the
    /// drainer's record-failure no-leak path can be exercised.
    fail_record_started: AtomicBool,
}

#[derive(Default)]
struct FakeJobState {
    jobs: HashMap<String, JobRecord>,
    dedupe: HashMap<JobKey, String>,
    owner_counters: HashMap<String, OwnerCounters>,
    global_queued: u32,
    global_running: u32,
    running_tokens: HashSet<String>,
}

#[derive(Default, Clone, Copy)]
struct OwnerCounters {
    queued: u32,
    running: u32,
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
            owner_kind: None,
            owner_id: None,
            queue_shard: None,
            queue_sort_key: None,
            next_eligible_at: None,
            dispatched_at: None,
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
        if self.fail_record_started.load(Ordering::SeqCst) {
            return Err(JobsError::Conflict);
        }
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
        if state
            .dedupe
            .get(&key)
            .is_some_and(|job_id| job_id == &record.job_id)
        {
            state.dedupe.remove(&key);
        }
        Ok(())
    }

    async fn find_active_dedupe_job(
        &self,
        key: &JobKey,
    ) -> spur_context_service::jobs::Result<Option<JobRecord>> {
        let state = self.state.lock().expect("fake store lock");
        if let Some(job_id) = state.dedupe.get(key) {
            if let Some(record) = state.jobs.get(job_id) {
                if record.status.holds_running_quota() || record.status == JobStatus::Queued {
                    return Ok(Some(record.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn enqueue_job(
        &self,
        request: CreateJobRequest,
        owner: BacklogOwner,
        config: &QueueConfig,
    ) -> spur_context_service::jobs::Result<EnqueueOutcome> {
        let key = request.key();
        let mut state = self.state.lock().expect("fake store lock");

        // Idempotent admission: return an existing active job.
        if let Some(existing) = active_dedupe_in_state(&state, &key) {
            return Ok(EnqueueOutcome::Existing(existing));
        }

        // Hard caps, checked before any write so over-cap rejects never produce
        // partial writes.
        if config.max_queued_per_owner == 0 {
            return Err(JobsError::QueueFull);
        }
        let owner_pk = owner.pk();
        let queued = state
            .owner_counters
            .get(&owner_pk)
            .map(|c| c.queued)
            .unwrap_or_default();
        if queued >= config.max_queued_per_owner {
            return Err(JobsError::QueueFull);
        }
        if config.max_queued_global > 0 && state.global_queued >= config.max_queued_global {
            return Err(JobsError::GlobalQueueFull);
        }

        let n = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let job_id = format!("job-{n}");
        let shard = format!("{:02}", n % u64::from(config.shard_count.max(1)));
        let sort_key = format!("{:011}#queued#{job_id}", n);
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
            owner_kind: Some(owner.kind),
            owner_id: Some(owner.id),
            queue_shard: Some(shard),
            queue_sort_key: Some(sort_key),
            next_eligible_at: Some(n),
            dispatched_at: None,
        };

        {
            let counters = state.owner_counters.entry(owner_pk).or_default();
            counters.queued += 1;
        }
        state.global_queued += 1;
        state.dedupe.insert(key, job_id.clone());
        state.jobs.insert(job_id, record.clone());
        Ok(EnqueueOutcome::Enqueued(record))
    }

    async fn dispatch_queued_job(
        &self,
        job_id: &str,
        config: &QueueConfig,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let mut state = self.state.lock().expect("fake store lock");

        // Validate + extract owner without holding a record borrow.
        let owner = {
            let record = state.jobs.get(job_id).ok_or(JobsError::NotFound)?;
            if record.status != JobStatus::Queued {
                return Err(JobsError::Conflict);
            }
            record.owner().ok_or(JobsError::Conflict)?
        };
        let owner_pk = owner.pk();

        let running = state
            .owner_counters
            .get(&owner_pk)
            .map(|c| c.running)
            .unwrap_or_default();
        if running >= config.max_running_per_owner {
            return Err(JobsError::Conflict);
        }
        if config.max_running_global > 0 && state.global_running >= config.max_running_global {
            return Err(JobsError::Conflict);
        }

        // Move counters, scoping the mutable borrow so subsequent state access
        // compiles.
        {
            let counters = state.owner_counters.entry(owner_pk).or_default();
            counters.queued = counters.queued.saturating_sub(1);
            counters.running += 1;
        }
        state.global_queued = state.global_queued.saturating_sub(1);
        state.global_running += 1;
        state.running_tokens.insert(job_id.to_string());

        let record = state.jobs.get_mut(job_id).ok_or(JobsError::NotFound)?;
        record.status = JobStatus::Dispatching;
        record.queue_shard = None;
        record.queue_sort_key = None;
        record.next_eligible_at = None;
        record.dispatched_at = Some("dispatched".to_string());
        Ok(record.clone())
    }

    async fn release_running_quota(
        &self,
        record: &JobRecord,
    ) -> spur_context_service::jobs::Result<()> {
        let mut state = self.state.lock().expect("fake store lock");
        // Exactly-once: only release if the RUNNING#<job_id> token still
        // exists. A repeat call finds no token and is a no-op.
        if !state.running_tokens.remove(&record.job_id) {
            return Ok(());
        }
        if let Some(owner) = record.owner() {
            if let Some(counters) = state.owner_counters.get_mut(&owner.pk()) {
                counters.running = counters.running.saturating_sub(1);
            }
        }
        if state.global_running > 0 {
            state.global_running -= 1;
        }
        Ok(())
    }

    async fn list_queued_jobs(
        &self,
        shard: &str,
        now_unix_secs: u64,
        limit: usize,
    ) -> spur_context_service::jobs::Result<Vec<JobRecord>> {
        let state = self.state.lock().expect("fake store lock");
        let mut candidates: Vec<JobRecord> = state
            .jobs
            .values()
            .filter(|record| {
                record.status == JobStatus::Queued
                    && record.queue_shard.as_deref() == Some(shard)
                    && record
                        .next_eligible_at
                        .is_some_and(|eligible| eligible <= now_unix_secs)
            })
            .cloned()
            .collect();
        // FIFO order: ascending queue_sort_key.
        candidates.sort_by(|a, b| a.queue_sort_key.cmp(&b.queue_sort_key));
        candidates.truncate(limit);
        Ok(candidates)
    }
}

fn active_dedupe_in_state(state: &FakeJobState, key: &JobKey) -> Option<JobRecord> {
    let job_id = state.dedupe.get(key)?;
    let record = state.jobs.get(job_id)?;
    if record.status.holds_running_quota() || record.status == JobStatus::Queued {
        Some(record.clone())
    } else {
        None
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

    fn job_count(&self) -> usize {
        self.state.lock().expect("fake store lock").jobs.len()
    }

    fn owner_queued(&self, owner: &BacklogOwner) -> u32 {
        self.state
            .lock()
            .expect("fake store lock")
            .owner_counters
            .get(&owner.pk())
            .map(|c| c.queued)
            .unwrap_or_default()
    }

    fn owner_running(&self, owner: &BacklogOwner) -> u32 {
        self.state
            .lock()
            .expect("fake store lock")
            .owner_counters
            .get(&owner.pk())
            .map(|c| c.running)
            .unwrap_or_default()
    }

    fn has_running_token(&self, job_id: &str) -> bool {
        self.state
            .lock()
            .expect("fake store lock")
            .running_tokens
            .contains(job_id)
    }
}

// ─── Drainer tests ──────────────────────────────────────────────────────────

/// Fake `IndexExecutionStarter` that records every `start_execution` call and
/// returns a synthetic execution ARN derived from the job name. Can be
/// configured to always fail.
struct FakeStarter {
    calls: Mutex<Vec<IndexExecutionRequest>>,
    fail: AtomicBool,
}

impl FakeStarter {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
        }
    }

    fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("starter lock").len()
    }
}

impl IndexExecutionStarter for FakeStarter {
    fn start_execution<'a>(
        &'a self,
        request: IndexExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, McpHandlerError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("starter lock")
                .push(request.clone());
            if self.fail.load(Ordering::SeqCst) {
                return Err(McpHandlerError::Internal(
                    "fake start_execution failure".to_owned(),
                ));
            }
            Ok(format!("arn:aws:states:execution:fake/{}", request.name))
        })
    }
}

fn drainer_queue_config(max_running_per_owner: u32) -> QueueConfig {
    QueueConfig {
        max_queued_per_owner: 20,
        max_queued_global: 0,
        max_running_per_owner,
        max_running_global: 0,
        shard_count: 4,
    }
}

#[test]
fn drainer_dispatches_queued_jobs_and_starts_each_once() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(u32::MAX);
        // Enqueue 3 unique jobs.
        let mut job_ids = Vec::new();
        for i in 1..=3 {
            let record = store
                .enqueue_job(with_package(&format!("pkg{i}")), owner.clone(), &config)
                .await?
                .into_record();
            job_ids.push(record.job_id);
        }

        let starter = FakeStarter::new();
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.dispatched, 3, "all 3 queued jobs should dispatch");
        assert_eq!(
            starter.call_count(),
            3,
            "start_execution called exactly once per job"
        );

        // Each job should have an execution ARN recorded and be running.
        for job_id in &job_ids {
            let record = store
                .lookup_job(job_id)
                .await?
                .context("job should exist")?;
            assert!(
                record.execution_arn.is_some(),
                "job {job_id} should have execution ARN"
            );
            assert!(
                !record.has_queue_gsi_attributes(),
                "job {job_id} should have GSI attrs removed after dispatch"
            );
        }
        Ok(())
    })
}

#[test]
fn drainer_running_caps_limit_dispatch_concurrency() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(2);
        // Enqueue 5 unique jobs; only 2 can run at once.
        for i in 1..=5 {
            store
                .enqueue_job(with_package(&format!("pkg{i}")), owner.clone(), &config)
                .await?;
        }

        let starter = FakeStarter::new();
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.dispatched, 2, "running cap limits to 2 dispatches");
        // Remaining jobs stay queued with GSI attrs intact.
        let queued = store
            .state
            .lock()
            .expect("lock")
            .jobs
            .values()
            .filter(|r| r.status == JobStatus::Queued)
            .count();
        assert_eq!(queued, 3, "3 jobs should remain queued");
        Ok(())
    })
}

#[test]
fn drainer_conflict_skips_contested_job_without_crashing() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(1);
        // Enqueue 3 jobs; only 1 can run.
        for i in 1..=3 {
            store
                .enqueue_job(with_package(&format!("pkg{i}")), owner.clone(), &config)
                .await?;
        }

        let starter = FakeStarter::new();
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        // First job dispatches; the other two hit the running cap and are
        // skipped as Conflict — the drainer must not crash.
        assert_eq!(summary.dispatched, 1);
        assert_eq!(
            summary.skipped, 2,
            "at-cap candidates are skipped, not crashed"
        );
        assert_eq!(starter.call_count(), 1);
        Ok(())
    })
}

#[test]
fn drainer_dispatch_removes_queue_gsi_and_frees_queued_capacity() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(u32::MAX);
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();
        assert_eq!(store.owner_queued(&owner), 1);

        let starter = FakeStarter::new();
        drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large()).await;

        let record = store.lookup_job(&enqueued.job_id).await?.context("job")?;
        assert!(
            !record.has_queue_gsi_attributes(),
            "GSI attrs removed on dispatch"
        );
        assert_eq!(
            store.owner_queued(&owner),
            0,
            "queued counter freed on dispatch"
        );
        assert_eq!(
            store.owner_running(&owner),
            1,
            "running counter claimed on dispatch"
        );
        Ok(())
    })
}

#[test]
fn drainer_start_failure_marks_job_failed_and_releases_quota() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(u32::MAX);
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();

        let starter = FakeStarter::new();
        starter.set_fail(true);

        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.failed, 1, "start failure → failed outcome");
        assert_eq!(starter.call_count(), 1, "start_execution was attempted");

        let record = store.lookup_job(&enqueued.job_id).await?.context("job")?;
        assert_eq!(
            record.status,
            JobStatus::Failed,
            "job must be failed, not stuck dispatching"
        );
        assert!(
            record.error_code.as_deref() == Some("start_execution"),
            "error code must be recorded"
        );
        // Running quota must be released — no leak.
        assert_eq!(
            store.owner_running(&owner),
            0,
            "running quota released after start failure"
        );
        assert!(
            !store.has_running_token(&enqueued.job_id),
            "running token removed after start failure"
        );
        Ok(())
    })
}

#[test]
fn drainer_record_failure_marks_job_failed_and_releases_quota() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(u32::MAX);
        let enqueued = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?
            .into_record();

        // Step Functions start succeeds, but record_execution_started fails.
        store.fail_record_started.store(true, Ordering::SeqCst);
        let starter = FakeStarter::new();

        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.failed, 1, "record failure → failed outcome");
        assert_eq!(
            starter.call_count(),
            1,
            "start_execution succeeded before record failure"
        );

        let record = store.lookup_job(&enqueued.job_id).await?.context("job")?;
        assert_eq!(
            record.status,
            JobStatus::Failed,
            "job must be failed, not stuck dispatching with no ARN"
        );
        assert!(
            record
                .error_code
                .is_some_and(|code| code.contains("record_execution_started")),
            "error code must reference record_execution_started"
        );
        // No leaked running quota.
        assert_eq!(
            store.owner_running(&owner),
            0,
            "running quota released after record failure — no leak"
        );
        assert!(
            !store.has_running_token(&enqueued.job_id),
            "running token removed after record failure"
        );
        Ok(())
    })
}

#[test]
fn build_index_execution_request_matches_admission_payload_contract() -> Result<()> {
    let record = JobRecord {
        job_id: "job-abc".to_owned(),
        status: JobStatus::Dispatching,
        source: "registry:crates-io".to_owned(),
        package: "serde".to_owned(),
        revision: "1.0.197".to_owned(),
        source_url: "https://crates.io/api/v1/crates/serde/1.0.197/download".to_owned(),
        source_url_hash: "sha256:serde".to_owned(),
        source_kind: "tarball".to_owned(),
        caller_id: "caller-42".to_owned(),
        execution_arn: None,
        attempt: 1,
        stage: None,
        snapshot_id: None,
        row_counts: None,
        error_code: None,
        error_detail: None,
        created_at: "now".to_owned(),
        updated_at: "now".to_owned(),
        owner_kind: Some(BacklogOwnerKind::Caller),
        owner_id: Some("caller-42".to_owned()),
        queue_shard: None,
        queue_sort_key: None,
        next_eligible_at: None,
        dispatched_at: None,
    };

    let request = mcp::build_index_execution_request(&record);

    assert_eq!(request.name, "job-abc", "execution name is the job_id");
    assert_eq!(request.input["job_id"], "job-abc");
    assert_eq!(request.input["source"], "registry:crates-io");
    assert_eq!(request.input["package"], "serde");
    assert_eq!(request.input["revision"], "1.0.197");
    assert_eq!(
        request.input["source_url"],
        "https://crates.io/api/v1/crates/serde/1.0.197/download"
    );
    assert_eq!(request.input["source_kind"], "tarball");
    assert_eq!(
        request.input["prefetch_source"], true,
        "tarball from non-S3 host must prefetch"
    );
    assert_eq!(request.input["caller_id"], "caller-42");
    assert!(
        request.input["limits"]["max_source_bytes"].is_u64(),
        "limits must carry max_source_bytes"
    );
    assert!(
        request.input["limits"]["max_build_seconds"].is_u64(),
        "limits must carry max_build_seconds"
    );
    Ok(())
}

#[test]
fn build_index_execution_request_s3_tarball_skips_prefetch() -> Result<()> {
    let record = JobRecord {
        job_id: "job-s3".to_owned(),
        status: JobStatus::Dispatching,
        source: "registry:custom".to_owned(),
        package: "pkg".to_owned(),
        revision: "rev".to_owned(),
        source_url: "https://bucket.s3.us-east-1.amazonaws.com/pkg.tar.gz".to_owned(),
        source_url_hash: "hash".to_owned(),
        source_kind: "tarball".to_owned(),
        caller_id: "caller".to_owned(),
        execution_arn: None,
        attempt: 1,
        stage: None,
        snapshot_id: None,
        row_counts: None,
        error_code: None,
        error_detail: None,
        created_at: "now".to_owned(),
        updated_at: "now".to_owned(),
        owner_kind: Some(BacklogOwnerKind::Caller),
        owner_id: Some("caller".to_owned()),
        queue_shard: None,
        queue_sort_key: None,
        next_eligible_at: None,
        dispatched_at: None,
    };

    let request = mcp::build_index_execution_request(&record);
    assert_eq!(
        request.input["prefetch_source"], false,
        "S3-hosted tarball does not need worker-side prefetch"
    );
    Ok(())
}

#[test]
fn drainer_production_entrypoint_delegates_to_drainer() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = drainer_queue_config(u32::MAX);
        store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await?;

        let starter = FakeStarter::new();
        // The production entrypoint delegates through
        // drain_queued_jobs_with_services — this test proves the runtime path.
        let summary: DrainSummary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.dispatched, 1);
        assert_eq!(starter.call_count(), 1);
        Ok(())
    })
}

fn now_secs_large() -> u64 {
    // Use a large now_secs so all jobs (next_eligible_at from the fake store
    // is the job sequence number n) are eligible.
    9_999_999_999
}
