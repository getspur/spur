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
//! - **Bounded work**: each invocation scans at most `shard_count` shards with
//!   `scan_limit_per_shard` candidates each, and dispatches at most
//!   `max_dispatches_per_run` jobs.
//! - **Conflict tolerance**: store `Conflict`, `QueueFull`, `GlobalQueueFull`,
//!   and running-limit conflicts are skip/retry signals for that candidate,
//!   not fatal drainer crashes.

use crate::jobs::{JobRecord, JobStore, JobsError, QueueConfig};
use crate::mcp::{self, IndexExecutionStarter};

/// Default maximum number of jobs a single drainer invocation will dispatch.
/// Keeps one Lambda invocation from exhausting the global running capacity or
/// running past its timeout.
const DEFAULT_MAX_DISPATCHES_PER_RUN: usize = 8;

/// Default maximum candidates examined per shard per invocation. Each candidate
/// that is at-capacity or contended counts toward this bound so a backlog of
/// at-cap owners cannot cause an unbounded scan.
const DEFAULT_SCAN_LIMIT_PER_SHARD: usize = 32;

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
    config: QueueConfig,
    max_dispatches_per_run: usize,
    scan_limit_per_shard: usize,
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
            config,
            max_dispatches_per_run: DEFAULT_MAX_DISPATCHES_PER_RUN,
            scan_limit_per_shard: DEFAULT_SCAN_LIMIT_PER_SHARD,
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

    /// Run one drainer invocation: scan shards in rotated order, dispatch
    /// eligible queued jobs under the configured running caps, and start Step
    /// Functions for each accepted job.
    ///
    /// The shard rotation offset is derived from `now_secs` so consecutive
    /// invocations start from different shards, ensuring every shard is scanned
    /// within `shard_count` invocations.
    pub async fn drain(&self, now_secs: u64) -> DrainSummary {
        let mut summary = DrainSummary::default();
        let shard_count = self.config.shard_count.max(1);
        let start_shard = now_secs.wrapping_rem(u64::from(shard_count));

        for offset in 0..shard_count {
            if summary.dispatched >= self.max_dispatches_per_run {
                break;
            }
            let shard_index = (start_shard + u64::from(offset)) % u64::from(shard_count);
            let shard = format!("{shard_index:02}");

            let candidates = match self
                .jobs
                .list_queued_jobs(&shard, now_secs, self.scan_limit_per_shard)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => {
                    eprintln!("[drainer] list_queued_jobs shard {shard} failed, skipping: {error}");
                    continue;
                }
            };

            for record in candidates {
                if summary.dispatched >= self.max_dispatches_per_run {
                    break;
                }
                match self.dispatch_one(&record).await {
                    DispatchOutcome::Started => summary.dispatched += 1,
                    DispatchOutcome::Skipped => summary.skipped += 1,
                    DispatchOutcome::Failed => summary.failed += 1,
                }
            }
        }

        summary
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
