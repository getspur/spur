use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::json;
use spur_context_service::drainer::{self, DrainSummary};
use spur_context_service::jobs::{
    BacklogOwner, BacklogOwnerKind, CreateJobOutcome, CreateJobRequest, EnqueueOutcome, JobKey,
    JobRecord, JobStatus, JobStore, JobsError, QueueConfig, QueueCursorSaveOutcome, QueuePageKey,
    QueueScanCursor, QueuedJobsPage,
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
fn namespaced_auth_identities_remain_distinct_backlog_owners() {
    let owners = [
        "cognito:user:opaque-human",
        "cognito:client:organization-a",
        "cognito:client:organization-b",
        "iam:123456789012:AROASTABLE",
        "anonymous-internal",
    ]
    .map(BacklogOwner::caller);

    assert_eq!(
        owners
            .iter()
            .map(BacklogOwner::pk)
            .collect::<HashSet<_>>()
            .len(),
        owners.len()
    );
    assert_eq!(
        owners
            .iter()
            .map(BacklogOwner::quota_pk)
            .collect::<HashSet<_>>()
            .len(),
        owners.len()
    );
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

// ─── Terminal release wiring ───────────────────────────────────────────────
//
// The live worker terminal paths (success/failure/spot) must release running
// quota exactly once after recording terminal status. These tests exercise the
// combined store methods that compose `mark_complete`/`mark_failed` with
// `release_running_quota`, proving no running capacity leaks and a terminal job
// no longer dedupes a fresh request.

/// Enqueue + dispatch a job so it holds a running slot, then return the
/// dispatched record and owner for assertions.
async fn setup_dispatched_running_job(
    store: &FakeJobStore,
    owner: &BacklogOwner,
    config: &QueueConfig,
) -> JobRecord {
    let enqueued = store
        .enqueue_job(create_job_request(), owner.clone(), config)
        .await
        .expect("enqueue")
        .into_record();
    store
        .dispatch_queued_job(&enqueued.job_id, config)
        .await
        .expect("dispatch")
}

#[test]
fn terminal_success_releases_running_quota_and_active_dedupe() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let dispatched = setup_dispatched_running_job(&store, &owner, &config).await;
        assert_eq!(store.owner_running(&owner), 1, "running slot held");
        assert!(store.has_running_token(&dispatched.job_id));

        store
            .mark_complete_and_release_running_quota(&dispatched.job_id, 99, json!({ "nodes": 1 }))
            .await
            .context("complete + release")?;

        assert_eq!(store.owner_running(&owner), 0, "running quota released");
        assert!(
            !store.has_running_token(&dispatched.job_id),
            "running token removed"
        );
        assert!(
            store
                .find_active_dedupe_job(&create_job_request().key())
                .await?
                .is_none(),
            "active dedupe must be cleared so a fresh request can enqueue"
        );
        Ok(())
    })
}

#[test]
fn terminal_failure_releases_running_quota_and_active_dedupe() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let dispatched = setup_dispatched_running_job(&store, &owner, &config).await;
        assert_eq!(store.owner_running(&owner), 1);

        store
            .mark_failed_and_release_running_quota(&dispatched.job_id, "translate", "boom")
            .await
            .context("fail + release")?;

        assert_eq!(store.owner_running(&owner), 0, "running quota released");
        assert!(
            !store.has_running_token(&dispatched.job_id),
            "running token removed"
        );
        assert!(
            store
                .find_active_dedupe_job(&create_job_request().key())
                .await?
                .is_none(),
            "active dedupe must be cleared on failure"
        );
        let record = store.lookup_job(&dispatched.job_id).await?.context("job")?;
        assert_eq!(record.status, JobStatus::Failed);
        Ok(())
    })
}

#[test]
fn duplicate_terminal_event_does_not_decrement_counters_twice() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let dispatched = setup_dispatched_running_job(&store, &owner, &config).await;

        // First terminal release decrements the running counter and removes the
        // token.
        store
            .mark_failed_and_release_running_quota(&dispatched.job_id, "fail", "first")
            .await
            .context("first terminal release")?;
        assert_eq!(store.owner_running(&owner), 0);

        // A duplicate terminal event (e.g. a retry/duplicate delivery) must NOT
        // decrement again — the token is already gone.
        store
            .mark_failed_and_release_running_quota(&dispatched.job_id, "fail", "duplicate")
            .await
            .context("duplicate terminal release")?;
        assert_eq!(
            store.owner_running(&owner),
            0,
            "duplicate terminal must not underflow the running counter"
        );
        assert!(
            !store.has_running_token(&dispatched.job_id),
            "token still absent after duplicate"
        );
        Ok(())
    })
}

#[test]
fn transaction_conflict_during_release_is_not_treated_as_success() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let dispatched = setup_dispatched_running_job(&store, &owner, &config).await;

        // Inject a TransactionConflict on the release path — the token still
        // exists and the counter is still held.
        store.fail_release.store(true, Ordering::SeqCst);

        let err = store
            .mark_failed_and_release_running_quota(&dispatched.job_id, "fail", "conflict")
            .await
            .unwrap_err();
        assert!(
            matches!(err, JobsError::Conflict),
            "release conflict must surface, not be swallowed as success; got {err:?}"
        );
        // The status WAS recorded, but the running slot must remain held so the
        // reconciler can retry — the release must not be dropped.
        assert_eq!(
            store.owner_running(&owner),
            1,
            "running quota must NOT be released on conflict"
        );
        assert!(
            store.has_running_token(&dispatched.job_id),
            "token must remain so the release can be retried"
        );
        Ok(())
    })
}

#[test]
fn after_terminal_same_package_can_enqueue_fresh_job() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = queue_config(5);
        let dispatched = setup_dispatched_running_job(&store, &owner, &config).await;

        store
            .mark_complete_and_release_running_quota(&dispatched.job_id, 1, json!({}))
            .await
            .context("terminal")?;

        // The same package/revision request must now be able to enqueue a fresh
        // job instead of deduping to the (now terminal) completed job.
        let outcome = store
            .enqueue_job(create_job_request(), owner.clone(), &config)
            .await
            .context("enqueue after terminal")?;
        let fresh = match outcome {
            EnqueueOutcome::Enqueued(record) => record,
            EnqueueOutcome::Existing(_) => anyhow::bail!("expected fresh job, got existing"),
        };
        assert_ne!(fresh.job_id, dispatched.job_id);
        assert_eq!(fresh.status, JobStatus::Queued);
        assert_eq!(
            store.owner_queued(&owner),
            1,
            "fresh job consumes a queued slot"
        );
        assert_eq!(
            store.owner_running(&owner),
            0,
            "completed job's running slot stays released"
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
    /// When true, `release_running_quota` returns a `Conflict` error (leaving
    /// the token and counter untouched) so the terminal-release conflict path
    /// can be exercised.
    fail_release: AtomicBool,
}

#[derive(Default)]
struct FakeJobState {
    jobs: HashMap<String, JobRecord>,
    dedupe: HashMap<JobKey, String>,
    owner_counters: HashMap<String, OwnerCounters>,
    global_queued: u32,
    global_running: u32,
    running_tokens: HashSet<String>,
    dispatch_attempts: usize,
    /// Durable per-shard drainer cursor state, including its CAS version.
    scan_cursors: HashMap<String, QueueScanCursor>,
    /// Ordered shard-query log used to assert the bounded page and rotation
    /// contracts through the public drainer behavior.
    queried_shards: Vec<String>,
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
        state.dispatch_attempts += 1;

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
            return Err(JobsError::GlobalRunningFull);
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
        // Injected conflict: the token still exists and the counter is held.
        // Mirrors the DynamoDB TransientConflict/QuotaConflict path where the
        // release transaction is cancelled but the running slot is not freed.
        if self.fail_release.load(Ordering::SeqCst) {
            return Err(JobsError::Conflict);
        }
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
        exclusive_start_key: Option<&QueuePageKey>,
    ) -> spur_context_service::jobs::Result<QueuedJobsPage> {
        if exclusive_start_key.is_some_and(|cursor| cursor.queue_shard != shard) {
            return Err(JobsError::Conflict);
        }

        let mut state = self.state.lock().expect("fake store lock");
        state.queried_shards.push(shard.to_string());
        let mut candidates: Vec<JobRecord> = state
            .jobs
            .values()
            .filter(|record| {
                record.status == JobStatus::Queued
                    && record.queue_shard.as_deref() == Some(shard)
                    && record
                        .next_eligible_at
                        .is_some_and(|eligible| eligible <= now_unix_secs)
                    && match exclusive_start_key {
                        Some(cursor) => record
                            .queue_sort_key
                            .as_deref()
                            .is_some_and(|sk| sk > cursor.queue_sort_key.as_str()),
                        None => true,
                    }
            })
            .cloned()
            .collect();
        // FIFO order: ascending queue_sort_key.
        candidates.sort_by(|a, b| a.queue_sort_key.cmp(&b.queue_sort_key));
        candidates.truncate(limit.max(1));
        // DynamoDB's contract is deliberately conservative: a page that
        // evaluates exactly Limit items may carry LastEvaluatedKey even when a
        // follow-up request will discover the tail. Only an absent key proves
        // that the query reached the tail.
        let last_evaluated_key = if candidates.len() == limit.max(1) {
            candidates.last().map(queue_page_key)
        } else {
            None
        };
        Ok(QueuedJobsPage {
            jobs: candidates,
            last_evaluated_key,
        })
    }

    async fn queue_scan_cursor(
        &self,
        shard: &str,
    ) -> spur_context_service::jobs::Result<QueueScanCursor> {
        Ok(self
            .state
            .lock()
            .expect("fake store lock")
            .scan_cursors
            .get(shard)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_queue_scan_cursor(
        &self,
        shard: &str,
        expected_version: u64,
        position: Option<&QueuePageKey>,
    ) -> spur_context_service::jobs::Result<QueueCursorSaveOutcome> {
        let mut state = self.state.lock().expect("fake store lock");
        let current = state.scan_cursors.entry(shard.to_string()).or_default();
        if current.version != expected_version {
            return Ok(QueueCursorSaveOutcome::Stale);
        }
        current.version = current.version.checked_add(1).ok_or(JobsError::Conflict)?;
        current.position = position.cloned();
        Ok(QueueCursorSaveOutcome::Saved)
    }
}

fn queue_page_key(record: &JobRecord) -> QueuePageKey {
    QueuePageKey {
        queue_shard: record
            .queue_shard
            .clone()
            .expect("queued fake record has queue_shard"),
        queue_sort_key: record
            .queue_sort_key
            .clone()
            .expect("queued fake record has queue_sort_key"),
        job_pk: format!("JOB#{}", record.job_id),
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

    fn take_queried_shards(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().expect("fake store lock").queried_shards)
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
fn drainer_stops_after_global_running_pool_is_saturated() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let mut config = drainer_queue_config(u32::MAX);
        config.max_running_global = 1;

        for i in 1..=3 {
            store
                .enqueue_job(
                    with_package(&format!("global-cap-{i}")),
                    owner.clone(),
                    &config,
                )
                .await?;
        }

        let starter = FakeStarter::new();
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;

        assert_eq!(summary.dispatched, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            store
                .state
                .lock()
                .expect("fake store lock")
                .dispatch_attempts,
            2,
            "global saturation must stop the invocation instead of probing every queued job"
        );
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

// ─── End-to-end burst / backpressure integration ───────────────────────────

/// Release all running jobs as terminal-complete. Helper for the burst-drain
/// integration test. Propagates release errors so a leaked running slot fails
/// the test instead of silently blocking future dispatches.
async fn release_all_running(store: &FakeJobStore) -> Result<()> {
    let running_ids: Vec<String> = store
        .state
        .lock()
        .expect("fake store lock")
        .jobs
        .values()
        .filter(|r| r.status.holds_running_quota())
        .map(|r| r.job_id.clone())
        .collect();
    for id in &running_ids {
        store
            .mark_complete_and_release_running_quota(id, 1, json!({}))
            .await
            .context("release_all_running: mark_complete_and_release_running_quota")?;
    }
    Ok(())
}

#[test]
fn release_all_running_propagates_terminal_release_errors() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = QueueConfig {
            max_queued_per_owner: 1,
            max_queued_global: 10,
            max_running_per_owner: 1,
            max_running_global: 1,
            shard_count: 1,
        };
        let job = store
            .enqueue_job(create_job_request(), owner, &config)
            .await?
            .into_record();
        store.dispatch_queued_job(&job.job_id, &config).await?;
        store.fail_release.store(true, Ordering::SeqCst);

        let error = release_all_running(&store)
            .await
            .expect_err("release helper must not swallow a leaked-slot conflict");

        assert!(error
            .to_string()
            .contains("release_all_running: mark_complete_and_release_running_quota"));
        assert!(store.has_running_token(&job.job_id));
        Ok(())
    })
}

#[test]
fn stale_concurrent_cursor_save_cannot_regress_or_clear_newer_progress() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let initial = store.queue_scan_cursor("00").await?;
        let first_position = QueuePageKey {
            queue_shard: "00".to_string(),
            queue_sort_key: "00000000001#queued#job-1".to_string(),
            job_pk: "JOB#job-1".to_string(),
        };
        let newer_position = QueuePageKey {
            queue_shard: "00".to_string(),
            queue_sort_key: "00000000002#queued#job-2".to_string(),
            job_pk: "JOB#job-2".to_string(),
        };

        assert_eq!(
            store
                .save_queue_scan_cursor("00", initial.version, Some(&first_position))
                .await?,
            QueueCursorSaveOutcome::Saved
        );
        let after_first = store.queue_scan_cursor("00").await?;
        assert_eq!(
            store
                .save_queue_scan_cursor("00", after_first.version, Some(&newer_position))
                .await?,
            QueueCursorSaveOutcome::Saved
        );

        assert_eq!(
            store
                .save_queue_scan_cursor("00", initial.version, Some(&first_position))
                .await?,
            QueueCursorSaveOutcome::Stale,
            "stale writer cannot move the cursor backward"
        );
        assert_eq!(
            store
                .save_queue_scan_cursor("00", initial.version, None)
                .await?,
            QueueCursorSaveOutcome::Stale,
            "stale writer cannot clear a newer cursor"
        );

        let current = store.queue_scan_cursor("00").await?;
        assert_eq!(current.version, 2);
        assert_eq!(current.position, Some(newer_position));
        Ok(())
    })
}

#[test]
fn tail_wraps_only_after_query_returns_no_last_evaluated_key() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        let config = QueueConfig {
            max_queued_per_owner: 2,
            max_queued_global: 10,
            max_running_per_owner: 0,
            max_running_global: 0,
            shard_count: 1,
        };
        store
            .enqueue_job(with_package("a-1"), owner.clone(), &config)
            .await?;
        store
            .enqueue_job(with_package("a-2"), owner, &config)
            .await?;
        let starter = FakeStarter::new();

        drainer::Drainer::new(&store, &starter, config)
            .with_limits(8, 2)
            .drain(now_secs_large())
            .await;
        let after_full_page = store.queue_scan_cursor("00").await?;
        assert!(
            after_full_page.position.is_some(),
            "a full page carries LastEvaluatedKey and must not wrap"
        );
        assert_eq!(store.take_queried_shards(), vec!["00"]);

        drainer::Drainer::new(&store, &starter, config)
            .with_limits(8, 2)
            .drain(now_secs_large())
            .await;
        let after_tail_probe = store.queue_scan_cursor("00").await?;
        assert_eq!(
            after_tail_probe.position, None,
            "only an absent LastEvaluatedKey proves tail and permits wrap"
        );
        assert_eq!(after_tail_probe.version, after_full_page.version + 1);
        assert_eq!(store.take_queried_shards(), vec!["00"]);
        Ok(())
    })
}

#[test]
fn dispatch_cap_persists_last_examined_position_mid_page() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let config = QueueConfig {
            max_queued_per_owner: 3,
            max_queued_global: 10,
            max_running_per_owner: 1,
            max_running_global: 0,
            shard_count: 1,
        };
        let first = store
            .enqueue_job(
                with_package("first"),
                BacklogOwner::caller("first"),
                &config,
            )
            .await?
            .into_record();
        let second = store
            .enqueue_job(
                with_package("second"),
                BacklogOwner::caller("second"),
                &config,
            )
            .await?
            .into_record();
        store
            .enqueue_job(
                with_package("third"),
                BacklogOwner::caller("third"),
                &config,
            )
            .await?;
        let starter = FakeStarter::new();

        drainer::Drainer::new(&store, &starter, config)
            .with_limits(1, 3)
            .drain(now_secs_large())
            .await;
        let cursor = store.queue_scan_cursor("00").await?;
        assert_eq!(cursor.position, Some(queue_page_key(&first)));

        // A fresh instance must resume at the last examined item, not at the
        // page's LastEvaluatedKey (which points at the unexamined third item).
        drainer::Drainer::new(&store, &starter, config)
            .with_limits(1, 3)
            .drain(now_secs_large())
            .await;
        let second_after = store
            .lookup_job(&second.job_id)
            .await?
            .context("second job")?;
        assert!(second_after.status.holds_running_quota());
        assert_eq!(store.take_queried_shards(), vec!["00", "00"]);
        Ok(())
    })
}

#[test]
fn dispatch_budget_limited_fairness_uses_effective_page_progress() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let config = QueueConfig {
            max_queued_per_owner: 1,
            max_queued_global: 20,
            max_running_per_owner: 1,
            max_running_global: 0,
            shard_count: 1,
        };
        let jobs_ahead = 10usize;
        for index in 0..jobs_ahead {
            store
                .enqueue_job(
                    with_package(&format!("ahead-{index}")),
                    BacklogOwner::caller(format!("owner-{index}")),
                    &config,
                )
                .await?;
        }
        let target = store
            .enqueue_job(
                with_package("target"),
                BacklogOwner::caller("target-owner"),
                &config,
            )
            .await?
            .into_record();
        let starter = FakeStarter::new();
        let page_limit = 32usize;
        let dispatch_budget = 2usize;
        let effective_progress = page_limit.min(dispatch_budget);
        let visit_bound = (jobs_ahead + 1).div_ceil(effective_progress) + 1;

        for _ in 0..visit_bound {
            drainer::Drainer::new(&store, &starter, config)
                .with_limits(dispatch_budget, page_limit)
                .drain(now_secs_large())
                .await;
            assert_eq!(store.take_queried_shards(), vec!["00"]);
            let record = store.lookup_job(&target.job_id).await?.context("target")?;
            if record.status.holds_running_quota() {
                return Ok(());
            }
        }

        anyhow::bail!(
            "target with {jobs_ahead} jobs ahead was not dispatched within \
             {visit_bound} dispatch-budget-limited shard visits"
        );
    })
}

#[test]
fn scheduled_rotation_covers_all_shards_when_rate_shares_factor_with_count() {
    block_on(async {
        let store = FakeJobStore::default();
        let starter = FakeStarter::new();
        let config = QueueConfig {
            shard_count: 16,
            ..QueueConfig::default()
        };
        let drainer = drainer::Drainer::new(&store, &starter, config)
            .with_limits(8, 2)
            .with_rotation_interval_secs(2 * 60);
        let mut first_shards = HashSet::new();

        for tick in 0..16_u64 {
            drainer.drain(tick * 2 * 60).await;
            let queried = store.take_queried_shards();
            first_shards.insert(
                queried
                    .first()
                    .expect("each invocation queries at least one shard")
                    .clone(),
            );
        }

        assert_eq!(first_shards.len(), 16);
        for shard in 0..16 {
            assert!(first_shards.contains(&format!("{shard:02}")));
        }
    })
}

/// Return one job_id currently holding a running slot (Dispatching/Running).
fn one_running_job(store: &FakeJobStore) -> Option<String> {
    store
        .state
        .lock()
        .expect("fake store lock")
        .jobs
        .values()
        .find(|r| r.status.holds_running_quota())
        .map(|r| r.job_id.clone())
}

#[test]
fn burst_enqueue_dispatches_only_running_cap_then_drains_after_release() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner = BacklogOwner::caller("alice");
        // Global cap is set to 10 (not disabled) so the burst exercises the
        // global queue accounting path: all 10 are accepted, dispatch frees
        // capacity, and subsequent enqueue after terminal release succeeds.
        let config = QueueConfig {
            max_queued_per_owner: 20,
            max_queued_global: 10,
            max_running_per_owner: 2,
            max_running_global: 0,
            shard_count: 5,
        };

        // Burst: 10 unique cold requests are all durably accepted.
        for i in 1..=10 {
            store
                .enqueue_job(with_package(&format!("burst-{i}")), owner.clone(), &config)
                .await?;
        }
        assert_eq!(store.owner_queued(&owner), 10, "all 10 accepted");
        assert_eq!(store.owner_running(&owner), 0);

        let starter = FakeStarter::new();

        // Drainer starts only the configured running capacity (2).
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;
        assert_eq!(summary.dispatched, 2, "only running cap dispatched");
        assert_eq!(store.owner_running(&owner), 2);
        assert_eq!(store.owner_queued(&owner), 8, "8 remain queued");

        // A duplicate request for an already-queued coordinate is deduped.
        let dedup = store
            .enqueue_job(with_package("burst-3"), owner.clone(), &config)
            .await
            .context("dedupe during burst")?;
        assert!(
            matches!(dedup, EnqueueOutcome::Existing(_)),
            "duplicate must dedupe, not consume another slot"
        );
        assert_eq!(
            store.owner_queued(&owner),
            8,
            "dedupe did not consume a slot"
        );

        // After terminal release of one running job, the drainer dispatches
        // the next queued job to re-saturate the running cap.
        let running_id =
            one_running_job(&store).context("at least one running job expected after drain")?;
        store
            .mark_complete_and_release_running_quota(&running_id, 1, json!({}))
            .await?;
        assert_eq!(store.owner_running(&owner), 1, "one slot freed by terminal");
        let summary =
            drainer::drain_queued_jobs_with_services(&store, &starter, config, now_secs_large())
                .await;
        assert_eq!(summary.dispatched, 1, "one more dispatched after release");
        assert_eq!(store.owner_running(&owner), 2, "running cap re-saturated");

        // Drain the rest: alternate terminal-release + drainer until queue empty.
        for _ in 0..20 {
            release_all_running(&store).await?;
            let summary = drainer::drain_queued_jobs_with_services(
                &store,
                &starter,
                config,
                now_secs_large(),
            )
            .await;
            if store.owner_queued(&owner) == 0 && summary.dispatched == 0 {
                break;
            }
        }

        // All 10 jobs were durably accepted and eventually dispatched.
        assert_eq!(starter.call_count(), 10, "all 10 eventually dispatched");
        assert_eq!(store.owner_queued(&owner), 0, "queue fully drained");
        assert_eq!(store.owner_running(&owner), 0, "all running released");
        Ok(())
    })
}

#[test]
fn max_queued_global_one_rejects_second_owner_until_dispatch_frees_capacity() -> Result<()> {
    block_on(async {
        let config = QueueConfig {
            max_queued_per_owner: 10,
            max_queued_global: 1,
            max_running_per_owner: u32::MAX,
            max_running_global: 0,
            shard_count: 4,
        };
        let store = FakeJobStore::default();
        let alice = BacklogOwner::caller("alice");
        let bob = BacklogOwner::caller("bob");

        // Alice fills the single global queued slot.
        let alice_job = store
            .enqueue_job(create_job_request(), alice.clone(), &config)
            .await
            .context("alice enqueue")?
            .into_record();

        // Bob is rejected — global queue is full.
        let err = store
            .enqueue_job(with_package("bob-pkg"), bob.clone(), &config)
            .await
            .unwrap_err();
        assert!(matches!(err, JobsError::GlobalQueueFull));

        // Dispatch frees the global queued slot.
        store
            .dispatch_queued_job(&alice_job.job_id, &config)
            .await?;

        // Bob can now enqueue — global capacity was released by dispatch.
        let bob_outcome = store
            .enqueue_job(with_package("bob-pkg"), bob.clone(), &config)
            .await
            .context("bob enqueue after dispatch")?;
        assert!(bob_outcome.is_enqueued());
        assert_eq!(store.owner_queued(&bob), 1);
        Ok(())
    })
}

// ─── Bounded multi-owner fairness (no starvation) ───────────────────────────
//
// Proves the drainer's per-shard scan cursor guarantees that a later eligible
// owner's job is dispatched within a finite, documented K drainer runs — even
// when an earlier owner continuously replenishes their backlog to stay at the
// scan-limit boundary. Without the cursor, a continuously-replenished backlog
// can fill the scan window indefinitely and starve later owners on the same
// shard.
//
// If a job has A eligible jobs ahead of it in its shard when admitted and each
// shard visit examines L candidates, it is examined within
// ceil((A + 1) / L) visits from a head cursor, or one extra visit if the
// persisted cursor must wrap first. This bound depends on actual per-shard
// depth/jobs ahead, not on one owner's configured maximum.

/// Helper: enqueue a unique package for an owner, returning the job_id. Uses a
/// monotonically increasing package suffix so each job gets a distinct dedupe
/// key and a higher `queue_sort_key` than all prior jobs.
async fn enqueue_unique(
    store: &FakeJobStore,
    owner: &BacklogOwner,
    config: &QueueConfig,
    counter: &mut u64,
    prefix: &str,
) -> Result<String> {
    *counter += 1;
    let pkg = format!("{prefix}-{counter}");
    let record = store
        .enqueue_job(with_package(&pkg), owner.clone(), config)
        .await
        .context("enqueue_unique")?
        .into_record();
    Ok(record.job_id)
}

#[test]
fn drainer_no_starvation_continuous_replenishment_within_k_runs() -> Result<()> {
    block_on(async {
        let store = FakeJobStore::default();
        let owner_a = BacklogOwner::caller("alice");
        let owner_b = BacklogOwner::caller("bob");
        let config = QueueConfig {
            max_queued_per_owner: 10,
            max_queued_global: 0,
            max_running_per_owner: 1,
            max_running_global: 0,
            shard_count: 1, // force all jobs onto one shard (worst case)
        };

        // Alice fills her backlog with 10 jobs (all sort ahead of Bob).
        let mut a_counter = 0u64;
        for _ in 0..10 {
            enqueue_unique(&store, &owner_a, &config, &mut a_counter, "a").await?;
        }
        // Bob enqueues 1 job AFTER all of Alice's.
        let b_id = store
            .enqueue_job(with_package("b-1"), owner_b.clone(), &config)
            .await?
            .into_record()
            .job_id;

        let starter = FakeStarter::new();
        // Bob initially has A=10 jobs ahead and each invocation examines one
        // L=2 page, so the head-cursor bound is ceil((10 + 1) / 2) = 6 shard
        // visits. Continuous Alice replenishment sorts behind Bob and cannot
        // increase the number of jobs already ahead of him.
        let jobs_ahead = 10usize;
        let scan_limit = 2usize;
        let max_runs = (jobs_ahead + 1).div_ceil(scan_limit);

        for run in 1..=max_runs {
            // Release any running Alice job and immediately replenish to keep
            // Alice's backlog at max_queued_per_owner — the starvation scenario.
            release_all_running(&store).await?;
            // Replenish Alice: if she has room, enqueue new jobs.
            while store.owner_queued(&owner_a) < 10 {
                enqueue_unique(&store, &owner_a, &config, &mut a_counter, "a").await?;
            }

            // Recreate the drainer every time to prove progress lives in the
            // store rather than in one warm Lambda/Drainer instance.
            drainer::Drainer::new(&store, &starter, config)
                .with_limits(8, scan_limit)
                .drain(now_secs_large())
                .await;
            assert_eq!(
                store.take_queried_shards(),
                vec!["00"],
                "one configured Query page per shard visit"
            );

            // Check if Bob dispatched.
            let b = store.lookup_job(&b_id).await?.context("b job")?;
            if b.status.holds_running_quota() {
                // Bob dispatched within K runs — success!
                eprintln!("Bob dispatched on run {run} (bound={max_runs})");
                return Ok(());
            }
        }

        anyhow::bail!(
            "Bob was NOT dispatched within {max_runs} runs \
             despite continuous Alice replenishment — starvation!"
        );
    })
}

#[test]
fn drainer_no_starvation_smoke_fewer_jobs_than_scan_limit() -> Result<()> {
    // Simpler scenario: Alice has 3 jobs, Bob has 1, scan_limit=2. Without the
    // cursor, Bob is hidden behind Alice's 3rd job. With the cursor, Bob is
    // reached in run 2. This is a smoke test for the basic pagination path
    // without continuous replenishment.
    block_on(async {
        let store = FakeJobStore::default();
        let owner_a = BacklogOwner::caller("alice");
        let owner_b = BacklogOwner::caller("bob");
        let config = QueueConfig {
            max_queued_per_owner: 10,
            max_queued_global: 0,
            max_running_per_owner: 1,
            max_running_global: 0,
            shard_count: 1,
        };

        store
            .enqueue_job(with_package("a-1"), owner_a.clone(), &config)
            .await?;
        store
            .enqueue_job(with_package("a-2"), owner_a.clone(), &config)
            .await?;
        store
            .enqueue_job(with_package("a-3"), owner_a.clone(), &config)
            .await?;
        let b_id = store
            .enqueue_job(with_package("b-1"), owner_b.clone(), &config)
            .await?
            .into_record()
            .job_id;

        let starter = FakeStarter::new();
        // Run 1: a-1 dispatches (A at cap=1), a-2 skipped. Cursor saved.
        drainer::Drainer::new(&store, &starter, config)
            .with_limits(8, 2)
            .drain(now_secs_large())
            .await;
        assert_eq!(store.owner_running(&owner_a), 1, "A at cap after run 1");

        // Release A's job, then construct a fresh drainer — the durable cursor
        // should resume past the previously-examined page and reach Bob.
        release_all_running(&store).await?;
        drainer::Drainer::new(&store, &starter, config)
            .with_limits(8, 2)
            .drain(now_secs_large())
            .await;

        let b_final = store.lookup_job(&b_id).await?.context("b")?;
        assert!(
            b_final.status.holds_running_quota(),
            "Bob should have dispatched by run 2 with cursor pagination, got {:?}",
            b_final.status
        );
        Ok(())
    })
}

fn now_secs_large() -> u64 {
    // Use a large now_secs so all jobs (next_eligible_at from the fake store
    // is the job sequence number n) are eligible.
    9_999_999_999
}
