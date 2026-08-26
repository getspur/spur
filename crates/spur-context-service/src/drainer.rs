//! Correctness drainer: dispatches queued index jobs under concurrency limits.
//!
//! The admission path (`external_index`) enqueues jobs into a bounded DynamoDB
//! backlog. This module is the path that makes those queued jobs actually
//! start: periodically or on worker invocation it selects eligible queued jobs
//! by shard, transactionally transitions them to `dispatching` while claiming
//! running quota, and invokes Step Functions for accepted jobs.
//!
//! ## Design invariants
//!
//! - **Transactional quota**: dispatch always goes through
//!   [`JobStore::dispatch_queued_job`], which moves queued→dispatching and
//!   decrements queued counters / claims running quota in one DynamoDB
//!   transaction. The drainer never bypasses this.
//! - **Idempotency**: `dispatch_queued_job` conditions on `status = queued`,
//!   so concurrent drainers can never start the same job twice. Step Functions
//!   execution names are the `job_id`, providing a second dedup layer.
//! - **No leaked quota**: if Step Functions start or `record_execution_started`
//!   fails *after* the dispatch transaction claimed running quota, the drainer
//!   marks the job `failed` and calls [`JobStore::release_running_quota`] so
//!   the running slot is observable and freed — never silently leaked.
//! - **Bounded work**: each invocation issues at most one configured candidate
//!   page per shard and dispatches at most `max_dispatches_per_run` jobs.
//! - **Bounded fairness (no starvation)**: the drainer persists the complete
//!   DynamoDB continuation key plus a CAS version per shard. If an eligible job
//!   has `A` candidates ahead in the persisted cursor's circular scan order,
//!   then starting that shard with available global capacity advances by at
//!   least `R = min(scan_limit_per_shard, max_dispatches_per_run)` candidates.
//!   It is examined within `ceil((A + 1) / R) + 1` such starts; the extra start
//!   covers an empty tail probe before wrap. Total per-shard depth bounds `A`—
//!   one owner's configured cap does not.
//! - **Conflict tolerance**: store `Conflict`, `QueueFull`, `GlobalQueueFull`,
//!   and running-limit conflicts are skip/retry signals for that candidate,
//!   not fatal drainer crashes.

use crate::jobs::{
    JobRecord, JobStore, JobsError, QueueConfig, QueueCursorSaveOutcome, QueuePageKey,
    QueueScanCursor,
};
use crate::mcp::{self, ExecutionStatusChecker, IndexExecutionStarter};

/// Default maximum number of jobs a single drainer invocation will dispatch.
/// Keeps one Lambda invocation from exhausting the global running capacity or
/// running past its timeout.
const DEFAULT_MAX_DISPATCHES_PER_RUN: usize = mcp::DEFAULT_INDEX_DRAINER_BATCH_LIMIT;

/// Default maximum candidates returned by the single Query page requested from
/// each shard per invocation.
const DEFAULT_SCAN_LIMIT_PER_SHARD: usize = mcp::DEFAULT_INDEX_DRAINER_SCAN_LIMIT_PER_SHARD;
const DEFAULT_ROTATION_INTERVAL_SECS: u64 = mcp::DEFAULT_INDEX_DRAINER_SCHEDULE_RATE_MINUTES * 60;

/// Summary of a single drainer invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainSummary {
    /// Jobs successfully dispatched (queued→dispatching→Step Functions started).
    pub dispatched: usize,
    /// Jobs skipped (already dispatched, at running cap, or contention).
    pub skipped: usize,
    /// Jobs that failed after dispatch (start or record error) and had their
    /// running quota released.
    pub failed: usize,
    /// Leftover `RUNNING#` tokens repaired without a client status poll
    /// (`sol_9bed32c0774d46bf`).
    pub repaired: usize,
}

impl DrainSummary {
    pub fn total(&self) -> usize {
        self.dispatched + self.skipped + self.failed
    }
}

/// Per-candidate dispatch outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchOutcome {
    /// Job was dispatched, Step Functions started, and the execution ARN was
    /// recorded.
    Started,
    /// Job was skipped — another drainer dispatched it, the owner is at the
    /// running cap, or a transient conflict occurred. The job stays queued and
    /// will be retried on a future invocation.
    Skipped,
    /// Job failed after the dispatch transaction claimed running quota. The
    /// drainer marked it `failed` and released running quota so no slot leaked.
    Failed,
    /// The hard global running-token pool is saturated. Stop the invocation so
    /// a large backlog cannot repeat the same bounded token probes per job.
    GlobalCapacityFull,
}

/// Bounded correctness drainer for queued index jobs.
///
/// Construct with [`Drainer::new`] (or [`Drainer::with_limits`] for a custom
/// scan/dispatch budget), then call [`Drainer::drain`] to process one
/// invocation. The drainer holds references to the job store and Step Functions
/// starter so it can be used from both the production Lambda path (real AWS
/// clients) and tests (fakes).
pub struct Drainer<'a> {
    jobs: &'a dyn JobStore,
    starter: &'a dyn IndexExecutionStarter,
    checker: Option<&'a dyn ExecutionStatusChecker>,
    config: QueueConfig,
    max_dispatches_per_run: usize,
    scan_limit_per_shard: usize,
    rotation_interval_secs: u64,
}

impl<'a> Drainer<'a> {
    /// Create a drainer with default scan/dispatch limits.
    pub fn new(
        jobs: &'a dyn JobStore,
        starter: &'a dyn IndexExecutionStarter,
        config: QueueConfig,
    ) -> Self {
        Self {
            jobs,
            starter,
            checker: None,
            config,
            max_dispatches_per_run: DEFAULT_MAX_DISPATCHES_PER_RUN,
            scan_limit_per_shard: DEFAULT_SCAN_LIMIT_PER_SHARD,
            rotation_interval_secs: DEFAULT_ROTATION_INTERVAL_SECS,
        }
    }

    /// Override the per-invocation dispatch and scan limits.
    #[must_use]
    pub fn with_limits(
        mut self,
        max_dispatches_per_run: usize,
        scan_limit_per_shard: usize,
    ) -> Self {
        self.max_dispatches_per_run = max_dispatches_per_run.max(1);
        self.scan_limit_per_shard = scan_limit_per_shard.max(1);
        self
    }

    /// Set the scheduled invocation interval used to derive the shard-start
    /// rotation tick. Dividing wall time by the cadence avoids the common-factor
    /// bug from `now_secs % shard_count` (for example, 120-second ticks with 16
    /// shards visiting only two starts).
    #[must_use]
    pub fn with_rotation_interval_secs(mut self, rotation_interval_secs: u64) -> Self {
        self.rotation_interval_secs = rotation_interval_secs.max(1);
        self
    }

    /// Observe Step Functions executions when repairing stale running jobs.
    /// Terminal leftover-token repair does not need a checker.
    #[must_use]
    pub fn with_checker(mut self, checker: &'a dyn ExecutionStatusChecker) -> Self {
        self.checker = Some(checker);
        self
    }

    /// Run one drainer invocation: scan shards in rotated order, dispatch
    /// eligible queued jobs under the configured running caps, and start Step
    /// Functions for each accepted job.
    ///
    /// ## Bounded fairness (no starvation)
    ///
    /// The shard rotation offset is derived from the configured schedule tick,
    /// so every shard becomes the starting shard even when cadence and shard
    /// count share factors. Within each shard the drainer uses a versioned,
    /// complete DynamoDB continuation key so the next run resumes past work
    /// already examined and stale concurrent saves cannot regress progress.
    ///
    /// `scan_limit_per_shard` is the Query page limit and at most one page is
    /// requested per shard per invocation. Let `A` be the candidates ahead in
    /// the persisted cursor's circular scan order, `L` the page limit, `B` the
    /// global dispatch budget, and `R = min(L, B)`. When this shard is the
    /// scheduled starting shard and hard global capacity is available, at least
    /// `R` candidates are examined, so the job is reached within
    /// `ceil((A + 1) / R) + 1` shard starts. Start rotation converts that to at
    /// most `shard_count * (ceil((A + 1) / R) + 1)` scheduled invocations. The
    /// extra start covers an empty tail probe before wrap. `A` is bounded by
    /// total eligible per-shard depth, not one owner's cap. Tail/wrap is detected
    /// only by an absent DynamoDB `LastEvaluatedKey`, never by item count.
    pub async fn drain(&self, now_secs: u64) -> DrainSummary {
        let mut summary = DrainSummary::default();
        self.reconcile_running_quota(&mut summary).await;
        let shard_count = self.config.shard_count.max(1);
        let rotation_tick = now_secs / self.rotation_interval_secs;
        let start_shard = rotation_tick.wrapping_rem(u64::from(shard_count));

        for offset in 0..shard_count {
            if summary.dispatched >= self.max_dispatches_per_run {
                break;
            }
            let shard_index = (start_shard + u64::from(offset)) % u64::from(shard_count);
            let shard = format!("{shard_index:02}");

            let global_full = self.drain_shard(&shard, now_secs, &mut summary).await;

            if global_full {
                // Hard global running pool saturated — stop scanning all
                // remaining shards. The next scheduled drainer run or
                // completion kick will resume when capacity frees.
                break;
            }

            if summary.dispatched >= self.max_dispatches_per_run {
                break;
            }
        }

        summary
    }

    /// Repair leftover `RUNNING#` tokens and stale dispatching/running jobs
    /// using the same path as `external_index_status` (`update_stale_job`).
    /// Failures are logged and skipped so dispatch can still proceed.
    async fn reconcile_running_quota(&self, summary: &mut DrainSummary) {
        let limit = mcp::MAX_INDEX_GLOBAL_RUNNING_TOKENS as usize;
        let job_ids = match self.jobs.list_running_token_job_ids(limit).await {
            Ok(ids) => ids,
            Err(error) => {
                eprintln!("[drainer] list_running_token_job_ids failed: {error}");
                return;
            }
        };
        for job_id in job_ids {
            let record = match self.jobs.lookup_job(&job_id).await {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(error) => {
                    eprintln!("[drainer] lookup_job {job_id} for quota repair failed: {error}");
                    continue;
                }
            };
            let before = record.status;
            match mcp::update_stale_job(record, self.jobs, self.checker).await {
                Ok(updated) => {
                    if before.is_terminal_for_quota() || updated.status != before {
                        summary.repaired += 1;
                    }
                }
                Err(error) => {
                    eprintln!("[drainer] quota repair for {job_id} failed: {error}");
                }
            }
        }
    }

    /// Drain one bounded Query page from a shard.
    ///
    /// Loads the persisted cursor, passes its complete key as
    /// `ExclusiveStartKey`, dispatches candidates from exactly one page, and
    /// compare-and-sets the next position. If the global dispatch cap stops the
    /// loop mid-page, the last examined item's complete key is persisted.
    ///
    /// Returns `true` if the hard global running-token pool is saturated, so
    /// the caller can stop the entire invocation without probing every shard.
    async fn drain_shard(&self, shard: &str, now_secs: u64, summary: &mut DrainSummary) -> bool {
        let cursor = match self.jobs.queue_scan_cursor(shard).await {
            Ok(cursor) => cursor,
            Err(error) => {
                eprintln!(
                    "[drainer] queue_scan_cursor shard {shard} failed, skipping shard: {error}"
                );
                return false;
            }
        };
        let page = match self
            .jobs
            .list_queued_jobs(
                shard,
                now_secs,
                self.scan_limit_per_shard,
                cursor.position.as_ref(),
            )
            .await
        {
            Ok(page) => page,
            Err(error) => {
                eprintln!("[drainer] list_queued_jobs shard {shard} failed, skipping: {error}");
                return false;
            }
        };

        let mut last_examined: Option<QueuePageKey> = None;
        let mut examined = 0usize;
        for record in &page.jobs {
            if summary.dispatched >= self.max_dispatches_per_run {
                break;
            }
            let position = match QueuePageKey::from_job_record(record) {
                Ok(position) => position,
                Err(error) => {
                    eprintln!(
                        "[drainer] queued job {} has invalid page key, skipping shard: {error}",
                        record.job_id
                    );
                    return false;
                }
            };
            examined += 1;
            last_examined = Some(position);

            match self.dispatch_one(record).await {
                DispatchOutcome::Started => summary.dispatched += 1,
                DispatchOutcome::Failed => summary.failed += 1,
                DispatchOutcome::Skipped => summary.skipped += 1,
                DispatchOutcome::GlobalCapacityFull => {
                    summary.skipped += 1;
                    self.save_cursor(shard, &cursor, last_examined.as_ref())
                        .await;
                    return true;
                }
            }
        }

        let stopped_mid_page = examined < page.jobs.len();
        let next_position = if stopped_mid_page {
            last_examined
        } else {
            // Only LastEvaluatedKey determines whether another page exists.
            // None is a versioned wrap marker; item count is irrelevant.
            page.last_evaluated_key
        };
        if next_position != cursor.position {
            self.save_cursor(shard, &cursor, next_position.as_ref())
                .await;
        }
        false
    }

    /// Save (or clear) the scan cursor, logging on failure so a store error
    /// does not crash the drainer. A failed save only means the next run
    /// re-scans from a stale position — correctness is not compromised because
    /// dispatch is still conditional on `status = queued`.
    async fn save_cursor(
        &self,
        shard: &str,
        current: &QueueScanCursor,
        next_position: Option<&QueuePageKey>,
    ) {
        match self
            .jobs
            .save_queue_scan_cursor(shard, current.version, next_position)
            .await
        {
            Ok(QueueCursorSaveOutcome::Saved) => {}
            Ok(QueueCursorSaveOutcome::Stale) => {
                eprintln!(
                    "[drainer] save_queue_scan_cursor shard {shard} lost CAS race; newer progress retained"
                );
            }
            Err(error) => {
                eprintln!("[drainer] save_queue_scan_cursor shard {shard} failed: {error}");
            }
        }
    }

    /// Dispatch a single queued job candidate.
    ///
    /// This is the core correctness path:
    /// 1. Transactional `dispatch_queued_job` (queued→dispatching + quota).
    /// 2. Build the Step Functions payload from the dispatched record.
    /// 3. Start Step Functions execution.
    /// 4. Record the execution ARN.
    ///
    /// If steps 3 or 4 fail *after* step 1 claimed running quota, the job is
    /// marked `failed` and running quota is released (see
    /// [`Self::handle_dispatch_failure`]).
    async fn dispatch_one(&self, record: &JobRecord) -> DispatchOutcome {
        // 1. Transactional dispatch: queued -> dispatching + running quota claim.
        //    The condition `status = queued` makes this idempotent — concurrent
        //    drainers cannot dispatch the same job twice.
        let dispatched = match self
            .jobs
            .dispatch_queued_job(&record.job_id, &self.config)
            .await
        {
            Ok(record) => record,
            Err(JobsError::GlobalRunningFull) => {
                return DispatchOutcome::GlobalCapacityFull;
            }
            Err(
                JobsError::Conflict
                | JobsError::QueueFull
                | JobsError::GlobalQueueFull
                | JobsError::NotFound,
            ) => {
                // Skip: another drainer dispatched it, the owner is at the
                // running cap, or the job is no longer queued. Not fatal.
                return DispatchOutcome::Skipped;
            }
            Err(error) => {
                // Unexpected store error — log and skip this candidate; the
                // scheduled drainer will retry on the next tick.
                eprintln!(
                    "[drainer] dispatch_queued_job {} unexpected error, skipping: {error}",
                    record.job_id
                );
                return DispatchOutcome::Skipped;
            }
        };

        // 2. Build the Step Functions payload from the dispatched record,
        //    reusing the same contract as the old immediate-start admission.
        let request = mcp::build_index_execution_request(&dispatched);

        // 3. Start Step Functions execution.
        let execution_arn = match self.starter.start_execution(request).await {
            Ok(arn) => arn,
            Err(error) => {
                let detail = error.to_string();
                eprintln!(
                    "[drainer] start_execution failed for {}: {detail}",
                    dispatched.job_id
                );
                self.handle_dispatch_failure(&dispatched, "start_execution", &detail)
                    .await;
                return DispatchOutcome::Failed;
            }
        };

        // 4. Record the execution ARN so `external_index_status` can observe
        //    and repair the running job.
        match self
            .jobs
            .record_execution_started(&dispatched.job_id, &execution_arn)
            .await
        {
            Ok(_) => DispatchOutcome::Started,
            Err(error) => {
                let detail = format!("record_execution_started: {error}");
                eprintln!(
                    "[drainer] record_execution_started failed for {}: {detail}",
                    dispatched.job_id
                );
                // Mark failed + release quota. This is the critical no-leak
                // path: the job is in `dispatching` with a claimed running slot
                // but no recorded ARN. Without this repair the job would be an
                // unobservable stuck `dispatching` job holding running quota.
                self.handle_dispatch_failure(&dispatched, "record_execution_started", &detail)
                    .await;
                DispatchOutcome::Failed
            }
        }
    }

    /// Handle a dispatch failure that occurred *after* the dispatch transaction
    /// claimed running quota.
    ///
    /// Marks the job `failed` (observable, not stuck) and releases running
    /// quota (no leaked slot). If `mark_failed` itself fails, the dispatched
    /// record is still used for the quota release — the `RUNNING#<job_id>` token
    /// created by dispatch is independent of the job status, so the release
    /// succeeds and the reconciler can later repair the stuck `dispatching`
    /// status.
    async fn handle_dispatch_failure(&self, dispatched: &JobRecord, code: &str, detail: &str) {
        let mark_result = self
            .jobs
            .mark_failed(&dispatched.job_id, code, detail)
            .await;
        if let Err(error) = &mark_result {
            eprintln!(
                "[drainer] mark_failed {} also failed: {error}",
                dispatched.job_id
            );
        }
        // Use the mark_failed record (still carries owner metadata) if
        // available, otherwise the dispatched record.
        let record = mark_result.as_ref().ok().unwrap_or(dispatched);
        if let Err(error) = self.jobs.release_running_quota(record).await {
            eprintln!(
                "[drainer] release_running_quota {} failed after dispatch failure: {error}",
                dispatched.job_id
            );
        }
    }
}

/// Convenience entrypoint: construct a [`Drainer`] from injected services and
/// run one invocation. This is the function the production Lambda path
/// (`lambda::drain_queued_jobs`) delegates to, and it is directly testable with
/// fake stores/starters without the `lambda` feature.
pub async fn drain_queued_jobs_with_services(
    jobs: &dyn JobStore,
    starter: &dyn IndexExecutionStarter,
    config: QueueConfig,
    now_secs: u64,
) -> DrainSummary {
    let drainer = Drainer::new(jobs, starter, config);
    drainer.drain(now_secs).await
}
