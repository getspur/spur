//! Index job control-plane records and status transitions.

use std::{
    collections::HashMap,
    env,
    error::Error as StdError,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use aws_sdk_dynamodb::{
    error::SdkError,
    operation::{
        query::QueryInput,
        transact_write_items::TransactWriteItemsError,
        update_item::{UpdateItemError, UpdateItemInput},
    },
    types::{
        AttributeValue, CancellationReason, Delete, Put, ReturnValue, TransactWriteItem, Update,
    },
    Client as DynamoDbClient,
};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, JobsError>;

const DEFAULT_INDEX_JOBS_TABLE: &str = "spur-context-index-jobs";
const JOB_PK_PREFIX: &str = "JOB#";
const DEDUPE_PK_PREFIX: &str = "DEDUP#";
const CALLER_QUOTA_PK_PREFIX: &str = "CALLER_QUOTA#";
const CALLER_RATE_PK_PREFIX: &str = "CALLER_RATE#";
const ACTIVE_JOB_PK_PREFIX: &str = "ACTIVE_JOB#";
const ACTIVE_JOB_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const RATE_WINDOW_TTL_SECS: u64 = 10 * 60;

// ─── Bounded queueing primitives ───────────────────────────────────────────
//
// Prefixes and defaults for the backlog/accounting items described in the
// backpressure design spec. These are additive to the existing control-plane
// keys; the live admission path keeps using `ACTIVE_JOB#` / `CALLER_QUOTA#`
// until a downstream task switches to owner-scoped backlog accounting.
const OWNER_PK_PREFIX: &str = "OWNER#";
const OWNER_QUOTA_SUFFIX: &str = "#QUOTA";
const GLOBAL_QUEUE_PK_PREFIX: &str = "GLOBAL#QUEUE#";
const GLOBAL_RUNNING_TOKEN_PK_PREFIX: &str = "GLOBAL#RUNNING_TOKEN#";
const RUNNING_TOKEN_PK_PREFIX: &str = "RUNNING#";
const RUNNING_TOKEN_TTL_SECS: u64 = 24 * 60 * 60;
/// Per-shard drainer scan-cursor item prefix: `CURSOR#<shard>`. Stores a
/// versioned copy of the complete DynamoDB continuation key under cursor-only
/// attribute names (so the cursor row does not enter the sparse queue GSI).
const QUEUE_CURSOR_PK_PREFIX: &str = "CURSOR#";
const DEFAULT_QUEUE_SHARD_COUNT: u32 = 16;
/// Name of the sparse DynamoDB GSI keyed by `(queue_shard, queue_sort_key)`.
pub const QUEUE_GSI_NAME: &str = "queue-gsi";
/// Zero-padding width for the Unix-seconds prefix of `queue_sort_key` so the
/// sparse queue GSI returns strictly ascending chronological order.
const QUEUE_SORT_KEY_PAD: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Dispatching,
    Running,
    Complete,
    Failed,
    Partial,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Partial => "partial",
        }
    }

    fn from_str(value: &str) -> std::result::Result<Self, InvalidJobStatus> {
        match value {
            "queued" => Ok(Self::Queued),
            "dispatching" => Ok(Self::Dispatching),
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "partial" => Ok(Self::Partial),
            _ => Err(InvalidJobStatus(value.to_string())),
        }
    }

    /// Whether this status holds a running quota slot that must be released
    /// exactly once on the terminal transition. Only `dispatching` and
    /// `running` hold a live `RUNNING#<job_id>` token. `partial` is terminal
    /// for quota purposes ([`Self::is_terminal_for_quota`]) — the terminal
    /// release has already freed (or is freeing) the running slot, so it does
    /// not itself "hold" quota. Use [`Self::is_terminal_for_quota`] when
    /// deciding whether to invoke `release_running_quota`.
    pub fn holds_running_quota(&self) -> bool {
        matches!(self, Self::Dispatching | Self::Running)
    }

    /// Terminal statuses for quota accounting. `partial` is terminal for quota
    /// purposes (it releases the running slot) even though it is resumable.
    pub fn is_terminal_for_quota(&self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Partial)
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobKey {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url_hash: String,
}

/// Identity under which a backlog is accounted. The first implementation maps
/// authenticated traffic to [`BacklogOwnerKind::Caller`] using the existing
/// `caller_id`; future deployments can scope backlogs by `User` or `TenantUser`
/// without changing the queue/drainer logic because DynamoDB keys are derived
/// from `(kind, id)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BacklogOwner {
    pub kind: BacklogOwnerKind,
    pub id: String,
}

impl BacklogOwner {
    pub fn new(kind: BacklogOwnerKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// Convenience constructor preserving current caller-based behavior.
    pub fn caller(caller_id: impl Into<String>) -> Self {
        Self::new(BacklogOwnerKind::Caller, caller_id)
    }

    /// DynamoDB partition key for owner-scoped items: `OWNER#<kind>#<id>`.
    pub fn pk(&self) -> String {
        owner_pk(self.kind, &self.id)
    }

    /// DynamoDB partition key for the per-owner quota/counter item:
    /// `OWNER#<kind>#<id>#QUOTA`.
    pub fn quota_pk(&self) -> String {
        owner_quota_pk(self.kind, &self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogOwnerKind {
    Anonymous,
    Caller,
    User,
    TenantUser,
}

impl BacklogOwnerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Caller => "caller",
            Self::User => "user",
            Self::TenantUser => "tenant_user",
        }
    }

    fn from_str(value: &str) -> std::result::Result<Self, InvalidOwnerKind> {
        match value {
            "anonymous" => Ok(Self::Anonymous),
            "caller" => Ok(Self::Caller),
            "user" => Ok(Self::User),
            "tenant_user" => Ok(Self::TenantUser),
            _ => Err(InvalidOwnerKind(value.to_string())),
        }
    }
}

impl fmt::Display for BacklogOwnerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CreateJobRequest {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub caller_id: String,
}

impl CreateJobRequest {
    pub fn key(&self) -> JobKey {
        JobKey {
            source: self.source.clone(),
            package: self.package.clone(),
            revision: self.revision.clone(),
            source_url_hash: self.source_url_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub status: JobStatus,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub caller_id: String,
    pub execution_arn: Option<String>,
    pub attempt: u32,
    pub stage: Option<String>,
    pub snapshot_id: Option<i64>,
    pub row_counts: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    // ─── Bounded queueing fields ───────────────────────────────────────────
    // All optional so legacy job records (created via the existing
    // `create_or_get_active_job_with_limit` path) continue to deserialize with
    // these absent. The enqueue path populates them; dispatch removes the GSI
    // attributes in the same transition that moves the job to `dispatching`.
    /// Backlog owner kind stored on the job for accounting (`owner_kind`).
    pub owner_kind: Option<BacklogOwnerKind>,
    /// Backlog owner id stored on the job (`owner_id`).
    pub owner_id: Option<String>,
    /// Sparse queue GSI partition key. Present only while the job is queued.
    pub queue_shard: Option<String>,
    /// Sparse queue GSI sort key `<next_eligible_at>#<queued_at>#<job_id>`.
    pub queue_sort_key: Option<String>,
    /// Unix seconds when the job becomes eligible for dispatch (requeue
    /// backoff). Present only while the job is queued.
    pub next_eligible_at: Option<u64>,
    /// Epoch-millis string recorded when the job transitioned to
    /// `dispatching`. Running-release metadata for the drainer/reconciler.
    pub dispatched_at: Option<String>,
}

impl JobRecord {
    pub fn key(&self) -> JobKey {
        JobKey {
            source: self.source.clone(),
            package: self.package.clone(),
            revision: self.revision.clone(),
            source_url_hash: self.source_url_hash.clone(),
        }
    }

    /// The [`BacklogOwner`] this job is accounted under, when known.
    pub fn owner(&self) -> Option<BacklogOwner> {
        Some(BacklogOwner {
            kind: self.owner_kind?,
            id: self.owner_id.clone()?,
        })
    }

    /// Whether this job currently carries sparse queue-GSI attributes (i.e. is
    /// eligible for drainer dispatch).
    pub fn has_queue_gsi_attributes(&self) -> bool {
        self.queue_shard.is_some()
            && self.queue_sort_key.is_some()
            && self.next_eligible_at.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CreateJobOutcome {
    Created(JobRecord),
    Existing(JobRecord),
}

/// Outcome of the bounded-queue enqueue primitive.
#[derive(Debug, Clone, PartialEq)]
pub enum EnqueueOutcome {
    /// A new queued job was created and accounted.
    Enqueued(JobRecord),
    /// An existing queued/dispatching/running job for the same dedupe key was
    /// returned (idempotent admission).
    Existing(JobRecord),
}

impl EnqueueOutcome {
    pub fn into_record(self) -> JobRecord {
        match self {
            Self::Enqueued(record) | Self::Existing(record) => record,
        }
    }

    pub fn is_enqueued(&self) -> bool {
        matches!(self, Self::Enqueued(_))
    }
}

/// Configuration for the bounded-queue admission/dispatch primitives.
///
/// `max_queued_per_owner` and `max_running_per_owner` are hard caps enforced
/// transactionally on the `OWNER#...#QUOTA` item. `max_queued_global` and
/// `max_running_global` use sharded/token counters; `0` disables the global
/// check (small deployments can rely on per-owner caps plus API throttles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    pub max_queued_per_owner: u32,
    pub max_queued_global: u32,
    pub max_running_per_owner: u32,
    pub max_running_global: u32,
    pub shard_count: u32,
}

impl QueueConfig {
    /// Defaults that preserve the *shape* of the current live behavior once the
    /// admission path switches to enqueue: running cap = the legacy per-caller
    /// active-job cap, queued cap = 0 (reject over capacity until enabled).
    pub fn legacy_compat(max_running_per_owner: u32) -> Self {
        Self {
            max_queued_per_owner: 0,
            max_queued_global: 0,
            max_running_per_owner,
            max_running_global: 0,
            shard_count: DEFAULT_QUEUE_SHARD_COUNT,
        }
    }

    pub fn with_shard_count(mut self, shard_count: u32) -> Self {
        if shard_count > 0 {
            self.shard_count = shard_count;
        }
        self.shard_count = self.shard_count.max(1);
        self
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_queued_per_owner: 0,
            max_queued_global: 0,
            max_running_per_owner: u32::MAX,
            max_running_global: 0,
            shard_count: DEFAULT_QUEUE_SHARD_COUNT,
        }
    }
}

/// Complete DynamoDB continuation key for the sparse queue GSI.
///
/// DynamoDB requires `ExclusiveStartKey` to contain the GSI partition/range
/// keys plus the base-table primary key. Keeping all three fields prevents
/// lossy sort-key-only pagination and lets a cursor resume using the official
/// Query `LastEvaluatedKey` contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePageKey {
    pub queue_shard: String,
    pub queue_sort_key: String,
    pub job_pk: String,
}

impl QueuePageKey {
    fn to_dynamodb_key(&self) -> HashMap<String, AttributeValue> {
        HashMap::from([
            (
                "queue_shard".to_string(),
                AttributeValue::S(self.queue_shard.clone()),
            ),
            (
                "queue_sort_key".to_string(),
                AttributeValue::S(self.queue_sort_key.clone()),
            ),
            ("pk".to_string(), AttributeValue::S(self.job_pk.clone())),
        ])
    }

    pub(crate) fn from_job_record(record: &JobRecord) -> Result<Self> {
        Ok(Self {
            queue_shard: record.queue_shard.clone().ok_or_else(|| {
                malformed_item(format!(
                    "queued job {} is missing queue_shard",
                    record.job_id
                ))
            })?,
            queue_sort_key: record.queue_sort_key.clone().ok_or_else(|| {
                malformed_item(format!(
                    "queued job {} is missing queue_sort_key",
                    record.job_id
                ))
            })?,
            job_pk: job_pk(&record.job_id),
        })
    }
}

/// One bounded DynamoDB Query page plus the exact key DynamoDB returned for a
/// possible continuation. An absent `last_evaluated_key` is the only tail
/// signal; returned item count is deliberately not used for wrap detection.
#[derive(Debug, Clone, Default)]
pub struct QueuedJobsPage {
    pub jobs: Vec<JobRecord>,
    pub last_evaluated_key: Option<QueuePageKey>,
}

/// Durable per-shard cursor state. The version is compare-and-set on every
/// progress or wrap update so a stale concurrent drainer cannot overwrite or
/// clear newer progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueScanCursor {
    pub position: Option<QueuePageKey>,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueCursorSaveOutcome {
    Saved,
    Stale,
}

#[async_trait]
pub trait JobStore: Send + Sync {
    async fn create_or_get_active_job(&self, request: CreateJobRequest)
        -> Result<CreateJobOutcome>;

    async fn check_index_rate_limit(
        &self,
        caller_id: &str,
        max_requests_per_minute: u32,
    ) -> Result<()> {
        let _ = (caller_id, max_requests_per_minute);
        Ok(())
    }

    async fn create_or_get_active_job_with_limit(
        &self,
        request: CreateJobRequest,
        max_active_jobs_per_caller: u32,
    ) -> Result<CreateJobOutcome> {
        let _ = max_active_jobs_per_caller;
        self.create_or_get_active_job(request).await
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> Result<JobRecord>;

    async fn update_stage(&self, job_id: &str, status: JobStatus, stage: &str)
        -> Result<JobRecord>;

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord>;

    async fn mark_failed(&self, job_id: &str, code: &str, detail: &str) -> Result<JobRecord>;

    /// Mark a job complete, then release its running quota exactly once.
    ///
    /// This is the live terminal-success path: after the terminal status is
    /// recorded ([`Self::mark_complete`] releases the active dedupe pointer and
    /// legacy caller quota), the owner running count and any global running
    /// token are released via [`Self::release_running_quota`].
    ///
    /// The release is idempotent: a repeated call (e.g. a duplicate terminal
    /// event delivery) finds the `RUNNING#<job_id>` token already gone and is a
    /// no-op, so running counters are never decremented twice. If the release
    /// encounters a transient [`JobsError::Conflict`] (concurrent write
    /// contention on the token/counter), the error is **surfaced** rather than
    /// swallowed — the caller (or reconciler) must retry so a running slot is
    /// never silently leaked. The terminal status has already been recorded by
    /// that point.
    async fn mark_complete_and_release_running_quota(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord> {
        let record = self.mark_complete(job_id, snapshot_id, row_counts).await?;
        self.release_running_quota(&record).await?;
        Ok(record)
    }

    /// Mark a job failed, then release its running quota exactly once.
    ///
    /// This is the live terminal-failure path (worker execution error, Step
    /// Functions failure, cancellation): after the terminal status is recorded
    /// ([`Self::mark_failed`] releases the active dedupe pointer and legacy
    /// caller quota), the owner running count and any global running token are
    /// released via [`Self::release_running_quota`].
    ///
    /// Same exactly-once and conflict-surfacing guarantees as
    /// [`Self::mark_complete_and_release_running_quota`]: a duplicate terminal
    /// event does not double-decrement, and a `Conflict` during release is
    /// surfaced for retry rather than dropped.
    async fn mark_failed_and_release_running_quota(
        &self,
        job_id: &str,
        code: &str,
        detail: &str,
    ) -> Result<JobRecord> {
        let record = self.mark_failed(job_id, code, detail).await?;
        self.release_running_quota(&record).await?;
        Ok(record)
    }

    async fn lookup_job(&self, job_id: &str) -> Result<Option<JobRecord>>;

    async fn release_dedupe_if_owner(&self, record: &JobRecord) -> Result<()>;

    // ─── Bounded queueing primitives ──────────────────────────────────────
    //
    // These are additive store primitives. The default implementations
    // preserve the current caller-based admission behavior so live callers
    // keep working until a downstream task switches to the queue path. The
    // DynamoDB and in-memory fake stores override them with real cap/queue
    // enforcement.

    /// Look up an existing queued/dispatching/running dedupe job for a key.
    ///
    /// Used by idempotent admission to return the active job instead of
    /// creating a duplicate. Defaults to no lookup (legacy stores did this
    /// inline inside `create_or_get_active_job_with_limit`).
    async fn find_active_dedupe_job(&self, key: &JobKey) -> Result<Option<JobRecord>> {
        let _ = key;
        Ok(None)
    }

    /// Enqueue a job under a backlog owner with bounded owner/global queue
    /// caps. Returns the existing active job if a dedupe entry already points
    /// at a queued/dispatching/running job for the same coordinate.
    ///
    /// Cap overflow rejects atomically: no job, dedupe pointer, queue-GSI
    /// attributes, or counter increments are written.
    async fn enqueue_job(
        &self,
        request: CreateJobRequest,
        owner: BacklogOwner,
        config: &QueueConfig,
    ) -> Result<EnqueueOutcome> {
        // Default: fall back to the existing active-job path so behavior is
        // preserved for stores that have not implemented queue accounting.
        let _ = (owner, config);
        let outcome = self
            .create_or_get_active_job_with_limit(request, u32::MAX)
            .await?;
        Ok(match outcome {
            CreateJobOutcome::Created(record) => EnqueueOutcome::Enqueued(record),
            CreateJobOutcome::Existing(record) => EnqueueOutcome::Existing(record),
        })
    }

    /// Transition a queued job to `dispatching`, acquiring running quota and
    /// creating the `RUNNING#<job_id>` release token. Skeleton for the drainer:
    /// it removes the sparse queue-GSI attributes from the job in the same
    /// transition so the job stops appearing in drainer scans.
    async fn dispatch_queued_job(&self, job_id: &str, config: &QueueConfig) -> Result<JobRecord> {
        let _ = (job_id, config);
        Err(JobsError::NotFound)
    }

    /// Idempotently release running quota for a job that has reached a terminal
    /// (for quota) state. Deletes the `RUNNING#<job_id>` release token exactly
    /// once; a repeated call is a no-op and must not decrement counters twice.
    async fn release_running_quota(&self, record: &JobRecord) -> Result<()> {
        let _ = record;
        Ok(())
    }

    /// List queued jobs eligible for dispatch from a queue shard, in FIFO order
    /// (ascending `queue_sort_key`). Returns at most `limit` jobs whose
    /// `next_eligible_at <= now_unix_secs`. The drainer uses this to discover
    /// candidates before attempting a transactional dispatch transition.
    ///
    /// When `exclusive_start_key` is present, it must be the complete key from
    /// a prior page's `LastEvaluatedKey` (or the complete key of the last item
    /// examined when stopping mid-page).
    ///
    /// Legacy stores that have not implemented queue GSI reads return an empty
    /// list — no queued work is visible, so the drainer is a no-op.
    async fn list_queued_jobs(
        &self,
        shard: &str,
        now_unix_secs: u64,
        limit: usize,
        exclusive_start_key: Option<&QueuePageKey>,
    ) -> Result<QueuedJobsPage> {
        let _ = (shard, now_unix_secs, limit, exclusive_start_key);
        Ok(QueuedJobsPage::default())
    }

    /// Read the persisted, versioned drainer scan cursor for a shard.
    ///
    /// Default: version zero at the head (no cursor persistence). Stores
    /// implementing bounded queueing override this to return the durable row.
    async fn queue_scan_cursor(&self, shard: &str) -> Result<QueueScanCursor> {
        let _ = shard;
        Ok(QueueScanCursor::default())
    }

    /// Compare-and-set the drainer cursor. `position = None` records a versioned
    /// wrap marker without deleting the cursor row, avoiding an ABA window in
    /// which a stale writer could recreate old progress after a clear.
    ///
    /// Default: reports success without persistence. Stores implementing
    /// bounded queueing override this with a conditional version update.
    async fn save_queue_scan_cursor(
        &self,
        shard: &str,
        expected_version: u64,
        position: Option<&QueuePageKey>,
    ) -> Result<QueueCursorSaveOutcome> {
        let _ = (shard, expected_version, position);
        Ok(QueueCursorSaveOutcome::Saved)
    }
}

#[derive(Debug, Clone)]
pub struct DynamoDbJobStore {
    client: DynamoDbClient,
    table_name: String,
    queue_gsi_name: String,
}

impl DynamoDbJobStore {
    pub fn new(client: DynamoDbClient) -> Self {
        let table_name = env::var("SPUR_INDEX_JOBS_TABLE")
            .unwrap_or_else(|_| DEFAULT_INDEX_JOBS_TABLE.to_string());
        let queue_gsi_name = configured_queue_gsi_name();
        Self {
            client,
            table_name,
            queue_gsi_name,
        }
    }

    pub fn with_table_name(client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
            queue_gsi_name: QUEUE_GSI_NAME.to_string(),
        }
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    async fn lookup_dedupe_job(&self, key: &JobKey) -> Result<Option<JobRecord>> {
        let Some(item) = self.get_item(&dedupe_pk(key)).await? else {
            return Ok(None);
        };
        let job_id = string_attr(&item, "job_id")?;
        self.lookup_job(&job_id).await
    }

    async fn get_item(&self, pk: &str) -> Result<Option<HashMap<String, AttributeValue>>> {
        // Control-plane reads must observe tokens created by the immediately
        // preceding dispatch transaction; treating a stale miss as an
        // idempotent release would leak owner/global running capacity.
        let output = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(pk.to_string()))
            .consistent_read(true)
            .send()
            .await
            .map_err(dynamodb_error)?;
        Ok(output.item)
    }

    async fn find_available_global_running_token(
        &self,
        job_id: &str,
        max_running_global: u32,
    ) -> Result<Option<(u32, String)>> {
        if max_running_global == 0 {
            return Ok(None);
        }
        let start = (fnv1a_64(job_id.as_bytes()) % u64::from(max_running_global)) as u32;
        for offset in 0..max_running_global {
            let slot = start.wrapping_add(offset) % max_running_global;
            let pk = global_running_token_pk(slot);
            if self.get_item(&pk).await?.is_none() {
                return Ok(Some((slot, pk)));
            }
        }
        Ok(None)
    }

    async fn update_job(
        &self,
        job_id: &str,
        update_expression: &str,
        names: HashMap<String, String>,
        values: HashMap<String, AttributeValue>,
    ) -> Result<JobRecord> {
        let output = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(job_pk(job_id)))
            .update_expression(update_expression)
            .set_expression_attribute_names(Some(names))
            .set_expression_attribute_values(Some(values))
            .condition_expression("attribute_exists(pk)")
            .return_values(ReturnValue::AllNew)
            .send()
            .await
            .map_err(dynamodb_error)?;
        let item = output.attributes.ok_or_else(|| {
            malformed_item(format!("update for job {job_id} returned no attributes"))
        })?;
        job_record_from_item(&item)
    }
}

#[async_trait]
impl JobStore for DynamoDbJobStore {
    async fn check_index_rate_limit(
        &self,
        caller_id: &str,
        max_requests_per_minute: u32,
    ) -> Result<()> {
        if max_requests_per_minute == 0 {
            return Err(JobsError::RateLimited);
        }
        let now = now_unix_secs();
        let update = caller_rate_acquire_update(
            &self.table_name,
            caller_id,
            now / 60,
            now,
            max_requests_per_minute,
        )?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().update(update).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_transaction_conflict(&error) => Err(JobsError::RateLimited),
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> Result<CreateJobOutcome> {
        self.create_or_get_active_job_with_limit(request, u32::MAX)
            .await
    }

    async fn create_or_get_active_job_with_limit(
        &self,
        request: CreateJobRequest,
        max_active_jobs_per_caller: u32,
    ) -> Result<CreateJobOutcome> {
        let key = request.key();
        if max_active_jobs_per_caller == 0 {
            if let Some(existing) = self.lookup_dedupe_job(&key).await? {
                return Ok(CreateJobOutcome::Existing(existing));
            }
            return Err(JobsError::ConcurrentLimit);
        }

        let now = now_string();
        let record = JobRecord {
            job_id: Uuid::new_v4().to_string(),
            status: JobStatus::Queued,
            source: request.source.clone(),
            package: request.package.clone(),
            revision: request.revision.clone(),
            source_url: request.source_url.clone(),
            source_url_hash: request.source_url_hash.clone(),
            source_kind: request.source_kind.clone(),
            caller_id: request.caller_id.clone(),
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: now.clone(),
            updated_at: now,
            // Legacy admission path does not populate queue/owner accounting.
            owner_kind: None,
            owner_id: None,
            queue_shard: None,
            queue_sort_key: None,
            next_eligible_at: None,
            dispatched_at: None,
        };
        let job_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(job_item(&record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let dedupe_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(dedupe_item(&key, &record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let active_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(active_job_item(&record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let quota_update = caller_quota_acquire_update(
            &self.table_name,
            &request.caller_id,
            max_active_jobs_per_caller,
        )?;

        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(job_put).build())
            .transact_items(TransactWriteItem::builder().put(dedupe_put).build())
            .transact_items(TransactWriteItem::builder().put(active_put).build())
            .transact_items(TransactWriteItem::builder().update(quota_update).build())
            .send()
            .await;

        match result {
            Ok(_) => Ok(CreateJobOutcome::Created(record)),
            Err(error) if is_transaction_conflict(&error) => {
                if let Some(existing) = self.lookup_dedupe_job(&key).await? {
                    Ok(CreateJobOutcome::Existing(existing))
                } else {
                    Err(JobsError::ConcurrentLimit)
                }
            }
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#execution_arn".to_string(), "execution_arn".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":execution_arn".to_string(),
            AttributeValue::S(execution_arn.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        self.update_job(
            job_id,
            "SET #execution_arn = :execution_arn, #updated_at = :updated_at",
            names,
            values,
        )
        .await
    }

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#stage".to_string(), "stage".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(status.as_str().to_string()),
        );
        values.insert(":stage".to_string(), AttributeValue::S(stage.to_string()));
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        self.update_job(
            job_id,
            "SET #status = :status, #stage = :stage, #updated_at = :updated_at",
            names,
            values,
        )
        .await
    }

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#snapshot_id".to_string(), "snapshot_id".to_string());
        names.insert("#row_counts".to_string(), "row_counts".to_string());
        names.insert("#error_code".to_string(), "error_code".to_string());
        names.insert("#error_detail".to_string(), "error_detail".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(JobStatus::Complete.as_str().to_string()),
        );
        values.insert(
            ":snapshot_id".to_string(),
            AttributeValue::N(snapshot_id.to_string()),
        );
        values.insert(
            ":row_counts".to_string(),
            AttributeValue::S(row_counts.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        let record = self
            .update_job(
                job_id,
                "SET #status = :status, #snapshot_id = :snapshot_id, #row_counts = :row_counts, #updated_at = :updated_at REMOVE #error_code, #error_detail",
                names,
                values,
            )
            .await?;
        self.release_dedupe_if_owner(&record).await?;
        self.release_active_job_if_owner(&record).await?;
        Ok(record)
    }

    async fn mark_failed(&self, job_id: &str, code: &str, detail: &str) -> Result<JobRecord> {
        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#error_code".to_string(), "error_code".to_string());
        names.insert("#error_detail".to_string(), "error_detail".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":status".to_string(),
            AttributeValue::S(JobStatus::Failed.as_str().to_string()),
        );
        values.insert(
            ":error_code".to_string(),
            AttributeValue::S(code.to_string()),
        );
        values.insert(
            ":error_detail".to_string(),
            AttributeValue::S(detail.to_string()),
        );
        values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
        let record = self
            .update_job(
                job_id,
                "SET #status = :status, #error_code = :error_code, #error_detail = :error_detail, #updated_at = :updated_at",
                names,
                values,
            )
            .await?;
        self.release_dedupe_if_owner(&record).await?;
        self.release_active_job_if_owner(&record).await?;
        Ok(record)
    }

    async fn lookup_job(&self, job_id: &str) -> Result<Option<JobRecord>> {
        let Some(item) = self.get_item(&job_pk(job_id)).await? else {
            return Ok(None);
        };
        job_record_from_item(&item).map(Some)
    }

    async fn release_dedupe_if_owner(&self, record: &JobRecord) -> Result<()> {
        let delete = Delete::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(dedupe_pk(&record.key())))
            .condition_expression("job_id = :job_id")
            .expression_attribute_values(":job_id", AttributeValue::S(record.job_id.clone()))
            .build()
            .map_err(dynamodb_error)?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().delete(delete).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_transaction_conflict(&error) => Ok(()),
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn find_active_dedupe_job(&self, key: &JobKey) -> Result<Option<JobRecord>> {
        let Some(record) = self.lookup_dedupe_job(key).await? else {
            return Ok(None);
        };
        // Only return jobs that are still active (queued/dispatching/running).
        // Terminal jobs are not useful for dedupe; admission should create a
        // fresh job for them.
        if record.status.holds_running_quota() || record.status == JobStatus::Queued {
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    async fn enqueue_job(
        &self,
        request: CreateJobRequest,
        owner: BacklogOwner,
        config: &QueueConfig,
    ) -> Result<EnqueueOutcome> {
        let key = request.key();

        // Idempotent admission: return an existing active job for the same
        // coordinate before attempting to create a duplicate.
        if let Some(existing) = self.find_active_dedupe_job(&key).await? {
            return Ok(EnqueueOutcome::Existing(existing));
        }

        if config.max_queued_per_owner == 0 {
            // Preserves the legacy "reject over capacity" contract for stacks
            // that have not enabled queueing yet.
            return Err(JobsError::QueueFull);
        }

        let now_millis = now_string();
        let now_secs = now_unix_secs();
        let job_id = Uuid::new_v4().to_string();
        let shard = queue_shard_for(owner.kind, &owner.id, &job_id, config.shard_count);
        let sort_key = queue_sort_key_for(now_secs, &now_millis, &job_id);
        let record = JobRecord {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            source: request.source.clone(),
            package: request.package.clone(),
            revision: request.revision.clone(),
            source_url: request.source_url.clone(),
            source_url_hash: request.source_url_hash.clone(),
            source_kind: request.source_kind.clone(),
            caller_id: request.caller_id.clone(),
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: now_millis.clone(),
            updated_at: now_millis.clone(),
            owner_kind: Some(owner.kind),
            owner_id: Some(owner.id.clone()),
            queue_shard: Some(shard.clone()),
            queue_sort_key: Some(sort_key),
            next_eligible_at: Some(now_secs),
            dispatched_at: None,
        };

        let mut job_item_map = job_item(&record);
        // Re-write the queue GSI attributes through the shared helper to keep
        // the DynamoDB item and the record in sync; job_item already populated
        // them from the record, so this is a shape-consistent overwrite.
        write_queue_gsi_attributes(
            &mut job_item_map,
            owner.kind,
            &owner.id,
            &job_id,
            now_secs,
            &now_millis,
            config.shard_count,
        );

        let job_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(job_item_map))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let dedupe_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(dedupe_item(&key, &record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;
        let owner_update =
            owner_quota_enqueue_update(&self.table_name, &owner, config.max_queued_per_owner)?;

        let mut tx = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(job_put).build())
            .transact_items(TransactWriteItem::builder().put(dedupe_put).build())
            .transact_items(TransactWriteItem::builder().update(owner_update).build());

        // Optional sharded global queued counter. Omitted entirely when the
        // global cap is disabled (0) so small deployments pay no extra write.
        if config.max_queued_global > 0 {
            // The global counter shard is computed separately from the queue
            // GSI shard: when max_queued_global < shard_count, fewer counter
            // shards are used so total capacity stays ≤ max_queued_global.
            let global_shard = global_counter_shard_for(
                owner.kind,
                &owner.id,
                &job_id,
                config.max_queued_global,
                config.shard_count,
            );
            let global_update = global_queue_enqueue_update(
                &self.table_name,
                &global_shard,
                config.max_queued_global,
                config.shard_count,
            )?;
            tx = tx.transact_items(TransactWriteItem::builder().update(global_update).build());
        }

        let has_global_item = config.max_queued_global > 0;
        let result = tx.send().await;
        match result {
            Ok(_) => Ok(EnqueueOutcome::Enqueued(record)),
            Err(error) if is_transaction_conflict(&error) => {
                // The transaction was cancelled atomically: either a duplicate
                // dedupe pointer beat us, or an owner/global cap was hit. No
                // partial writes occurred. Inspect the cancellation reasons by
                // transaction-item position so callers see the right rejection
                // reason. Items: [job(0), dedupe(1), owner(2), global?(3)].
                match classify_enqueue_cancellation(&error, has_global_item) {
                    EnqueueCancellation::Duplicate => {
                        if let Some(existing) = self.find_active_dedupe_job(&key).await? {
                            Ok(EnqueueOutcome::Existing(existing))
                        } else {
                            Err(JobsError::Conflict)
                        }
                    }
                    EnqueueCancellation::OwnerFull => Err(JobsError::QueueFull),
                    EnqueueCancellation::GlobalFull => Err(JobsError::GlobalQueueFull),
                    // TransactionConflict at an owner/global counter item:
                    // concurrent write contention, not a definitive capacity
                    // rejection. Surface as Conflict so the caller retries
                    // rather than falsely reporting the queue as full.
                    EnqueueCancellation::TransientConflict => Err(JobsError::Conflict),
                    // Fallback: re-check dedupe; otherwise default to
                    // QueueFull since the most common non-positioned cancel is
                    // an owner cap collision under message-only errors.
                    EnqueueCancellation::Unknown => {
                        if let Some(existing) = self.find_active_dedupe_job(&key).await? {
                            Ok(EnqueueOutcome::Existing(existing))
                        } else {
                            Err(JobsError::QueueFull)
                        }
                    }
                }
            }
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn dispatch_queued_job(&self, job_id: &str, config: &QueueConfig) -> Result<JobRecord> {
        // Read the full job record to capture owner AND queue_shard before the
        // dispatch transition removes the GSI attributes.
        let record = self.lookup_job(job_id).await?.ok_or(JobsError::NotFound)?;
        let owner = record.owner().ok_or(JobsError::Conflict)?;
        // Capture the shard so we can decrement the matching GLOBAL#QUEUE#<shard>
        // counter in the same transaction. A queued job must have GSI attrs; if
        // they are missing the job was already dispatched.
        let queue_shard = record.queue_shard.clone().ok_or(JobsError::Conflict)?;

        // Select a candidate from the fixed global token pool. The later
        // conditional Put inside the dispatch transaction is the hard cap;
        // this read only avoids attempting slots that are visibly occupied.
        let global_running_token = if config.max_running_global > 0 {
            Some(
                self.find_available_global_running_token(job_id, config.max_running_global)
                    .await?
                    .ok_or(JobsError::GlobalRunningFull)?,
            )
        } else {
            None
        };

        let now_millis = now_string();

        let mut names = HashMap::new();
        names.insert("#status".to_string(), "status".to_string());
        names.insert("#updated_at".to_string(), "updated_at".to_string());
        names.insert("#dispatched_at".to_string(), "dispatched_at".to_string());
        let mut values = HashMap::new();
        values.insert(
            ":queued".to_string(),
            AttributeValue::S(JobStatus::Queued.as_str().to_string()),
        );
        values.insert(
            ":dispatching".to_string(),
            AttributeValue::S(JobStatus::Dispatching.as_str().to_string()),
        );
        values.insert(
            ":updated_at".to_string(),
            AttributeValue::S(now_millis.clone()),
        );
        let job_update = Update::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(job_pk(job_id)))
            .update_expression(
                "SET #status = :dispatching, #updated_at = :updated_at, #dispatched_at = :updated_at REMOVE queue_shard, queue_sort_key, next_eligible_at",
            )
            .condition_expression("#status = :queued")
            .set_expression_attribute_names(Some(names))
            .set_expression_attribute_values(Some(values))
            .build()
            .map_err(dynamodb_error)?;
        let quota_update =
            owner_quota_dispatch_update(&self.table_name, &owner, config.max_running_per_owner)?;
        // Pass the real queue_shard captured from the job record so the token
        // carries correct metadata for the drainer/reconciler.
        let token = running_token_item(
            job_id,
            &now_millis,
            &owner,
            &queue_shard,
            global_running_token.as_ref().map(|(_, pk)| pk.as_str()),
        );
        let token_put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(token))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(dynamodb_error)?;

        let mut tx = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().update(job_update).build())
            .transact_items(TransactWriteItem::builder().update(quota_update).build())
            .transact_items(TransactWriteItem::builder().put(token_put).build());

        if let Some((slot, _)) = global_running_token {
            let global_token_put = Put::builder()
                .table_name(&self.table_name)
                .set_item(Some(global_running_token_item(slot, job_id, &now_millis)))
                .condition_expression("attribute_not_exists(pk)")
                .build()
                .map_err(dynamodb_error)?;
            tx = tx.transact_items(TransactWriteItem::builder().put(global_token_put).build());
        }

        // Decrement the matching sharded global queued counter so dispatch
        // frees global queue capacity. Omitted when the global cap is disabled.
        // The global counter shard is recomputed from the job identity + config
        // (not read from the record's queue_shard) because it may differ from
        // the queue GSI shard when max_queued_global < shard_count. The owner
        // and job_id are still on the record after GSI attributes are removed.
        if config.max_queued_global > 0 {
            let global_shard = global_counter_shard_for(
                owner.kind,
                &owner.id,
                job_id,
                config.max_queued_global,
                config.shard_count,
            );
            let global_update = global_queue_dispatch_update(&self.table_name, &global_shard)?;
            tx = tx.transact_items(TransactWriteItem::builder().update(global_update).build());
        }

        let result = tx.send().await;
        match result {
            Ok(_) => {
                // `transact_write_items` does not return updated attributes;
                // re-read the job to return the post-transition record.
                self.lookup_job(job_id).await?.ok_or(JobsError::NotFound)
            }
            Err(error) if is_transaction_conflict(&error) => {
                // Another drainer dispatched it, or the owner is at the running
                // cap. Surface as Conflict so the caller skips this candidate.
                Err(JobsError::Conflict)
            }
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn release_running_quota(&self, record: &JobRecord) -> Result<()> {
        let Some(owner) = record.owner() else {
            // Legacy jobs (no owner accounting) have no running token to
            // release; treat as already released.
            return Ok(());
        };
        let Some(token) = self.get_item(&running_token_pk(&record.job_id)).await? else {
            return Ok(());
        };
        let global_running_token_pk = optional_string_attr(&token, "global_running_token_pk")?;
        let token_delete = running_token_delete(&self.table_name, &record.job_id)?;
        let quota_update = owner_quota_release_update(&self.table_name, &owner)?;
        let mut tx = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().delete(token_delete).build())
            .transact_items(TransactWriteItem::builder().update(quota_update).build());
        if let Some(global_token_pk) = global_running_token_pk {
            let global_token_delete =
                global_running_token_delete(&self.table_name, &global_token_pk, &record.job_id)?;
            tx = tx.transact_items(
                TransactWriteItem::builder()
                    .delete(global_token_delete)
                    .build(),
            );
        }
        let result = tx.send().await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_transaction_conflict(&error) => {
                // Distinguish why the release transaction was cancelled. Items:
                // [token_delete(0), quota_update(1), global_token_delete?(2)].
                match classify_release_cancellation(&error) {
                    // The RUNNING#<job_id> token was already deleted → the
                    // release already happened. Idempotent no-op.
                    ReleaseCancellation::TokenGone => Ok(()),
                    // An owner-quota update or global-token delete condition
                    // failed while the per-job token still exists. Surface the
                    // error so reconciliation can repair the related item; do
                    // not pretend the release succeeded.
                    ReleaseCancellation::QuotaConflict => Err(JobsError::Conflict),
                    // TransactionConflict: concurrent write contention on the
                    // token or counter. The token may still exist — must NOT
                    // treat as already released (would leak a running slot).
                    // Surface as Conflict so the caller retries.
                    ReleaseCancellation::TransientConflict => Err(JobsError::Conflict),
                    // Unknown cancellation: conservatively treat as a conflict
                    // so the caller/reconciler can retry rather than silently
                    // dropping the release.
                    ReleaseCancellation::Unknown => Err(JobsError::Conflict),
                }
            }
            Err(error) => Err(dynamodb_error(error)),
        }
    }

    async fn list_queued_jobs(
        &self,
        shard: &str,
        now_secs: u64,
        limit: usize,
        exclusive_start_key: Option<&QueuePageKey>,
    ) -> Result<QueuedJobsPage> {
        let input = queued_jobs_query_input(
            &self.table_name,
            &self.queue_gsi_name,
            shard,
            now_secs,
            limit,
            exclusive_start_key,
        )?;
        let output = self
            .client
            .query()
            .set_table_name(input.table_name)
            .set_index_name(input.index_name)
            .set_key_condition_expression(input.key_condition_expression)
            .set_expression_attribute_values(input.expression_attribute_values)
            .set_exclusive_start_key(input.exclusive_start_key)
            .set_limit(input.limit)
            .send()
            .await
            .map_err(dynamodb_error)?;
        let last_evaluated_key = output
            .last_evaluated_key
            .as_ref()
            .map(queue_page_key_from_dynamodb)
            .transpose()?;
        let jobs = output
            .items
            .unwrap_or_default()
            .iter()
            .map(job_record_from_item)
            .collect::<Result<Vec<_>>>()?;
        Ok(QueuedJobsPage {
            jobs,
            last_evaluated_key,
        })
    }

    async fn queue_scan_cursor(&self, shard: &str) -> Result<QueueScanCursor> {
        let Some(item) = self.get_item(&queue_cursor_pk(shard)).await? else {
            return Ok(QueueScanCursor::default());
        };
        let stored_shard = optional_string_attr(&item, "cursor_queue_shard")?
            .or(optional_string_attr(&item, "queue_shard")?);
        if stored_shard
            .as_deref()
            .is_some_and(|stored| stored != shard)
        {
            return Err(malformed_item(format!(
                "queue cursor shard does not match requested shard {shard}"
            )));
        }
        let version = optional_u64_attr(&item, "cursor_version")?.unwrap_or(0);
        let queue_sort_key = optional_string_attr(&item, "cursor_queue_sort_key")?;
        let job_pk = optional_string_attr(&item, "cursor_job_pk")?;
        let position = match (queue_sort_key, job_pk) {
            (Some(queue_sort_key), Some(job_pk)) => Some(QueuePageKey {
                queue_shard: shard.to_string(),
                queue_sort_key,
                job_pk,
            }),
            (None, None) => None,
            _ => {
                return Err(malformed_item(format!(
                    "queue cursor {shard} has an incomplete continuation key"
                )))
            }
        };
        Ok(QueueScanCursor { position, version })
    }

    async fn save_queue_scan_cursor(
        &self,
        shard: &str,
        expected_version: u64,
        position: Option<&QueuePageKey>,
    ) -> Result<QueueCursorSaveOutcome> {
        let input = queue_cursor_update_input(
            &self.table_name,
            shard,
            expected_version,
            position,
            &now_string(),
        )?;
        let result = self
            .client
            .update_item()
            .set_table_name(input.table_name)
            .set_key(input.key)
            .set_update_expression(input.update_expression)
            .set_condition_expression(input.condition_expression)
            .set_expression_attribute_names(input.expression_attribute_names)
            .set_expression_attribute_values(input.expression_attribute_values)
            .send()
            .await;
        match result {
            Ok(_) => Ok(QueueCursorSaveOutcome::Saved),
            Err(error) if update_was_stale(&error) => Ok(QueueCursorSaveOutcome::Stale),
            Err(error) => Err(dynamodb_error(error)),
        }
    }
}

fn queued_jobs_query_input(
    table_name: &str,
    queue_gsi_name: &str,
    shard: &str,
    now_secs: u64,
    limit: usize,
    exclusive_start_key: Option<&QueuePageKey>,
) -> Result<QueryInput> {
    if exclusive_start_key.is_some_and(|cursor| cursor.queue_shard != shard) {
        return Err(malformed_item(format!(
            "queue cursor shard does not match query shard {shard}"
        )));
    }
    let ceiling = format!("{now_secs:0QUEUE_SORT_KEY_PAD$}~~~~~~~~~~~~~~~~~~");
    let expression_attribute_values = HashMap::from([
        (":shard".to_string(), AttributeValue::S(shard.to_string())),
        (":ceiling".to_string(), AttributeValue::S(ceiling)),
    ]);
    QueryInput::builder()
        .table_name(table_name)
        .index_name(queue_gsi_name)
        // DynamoDB permits one sort-key predicate. Pagination is expressed
        // exclusively through ExclusiveStartKey, never by combining > and <=.
        .key_condition_expression("queue_shard = :shard AND queue_sort_key <= :ceiling")
        .set_expression_attribute_values(Some(expression_attribute_values))
        .set_exclusive_start_key(exclusive_start_key.map(QueuePageKey::to_dynamodb_key))
        .limit(i32::try_from(limit.max(1)).unwrap_or(i32::MAX))
        .build()
        .map_err(dynamodb_error)
}

fn queue_page_key_from_dynamodb(key: &HashMap<String, AttributeValue>) -> Result<QueuePageKey> {
    Ok(QueuePageKey {
        queue_shard: string_attr(key, "queue_shard")?,
        queue_sort_key: string_attr(key, "queue_sort_key")?,
        job_pk: string_attr(key, "pk")?,
    })
}

fn queue_cursor_update_input(
    table_name: &str,
    shard: &str,
    expected_version: u64,
    position: Option<&QueuePageKey>,
    updated_at: &str,
) -> Result<UpdateItemInput> {
    if position.is_some_and(|cursor| cursor.queue_shard != shard) {
        return Err(malformed_item(format!(
            "queue cursor position does not match shard {shard}"
        )));
    }
    let next_version = expected_version
        .checked_add(1)
        .ok_or_else(|| malformed_item(format!("queue cursor {shard} version overflow")))?;
    let names = HashMap::from([
        ("#item_type".to_string(), "item_type".to_string()),
        (
            "#cursor_queue_shard".to_string(),
            "cursor_queue_shard".to_string(),
        ),
        ("#cursor_version".to_string(), "cursor_version".to_string()),
        ("#updated_at".to_string(), "updated_at".to_string()),
        (
            "#cursor_queue_sort_key".to_string(),
            "cursor_queue_sort_key".to_string(),
        ),
        ("#cursor_job_pk".to_string(), "cursor_job_pk".to_string()),
        ("#last_sort_key".to_string(), "last_sort_key".to_string()),
        ("#expires_at".to_string(), "expires_at".to_string()),
    ]);
    let mut values = HashMap::from([
        (
            ":item_type".to_string(),
            AttributeValue::S("queue_cursor".to_string()),
        ),
        (
            ":queue_shard".to_string(),
            AttributeValue::S(shard.to_string()),
        ),
        (
            ":next_version".to_string(),
            AttributeValue::N(next_version.to_string()),
        ),
        (
            ":updated_at".to_string(),
            AttributeValue::S(updated_at.to_string()),
        ),
    ]);
    let update_expression = if let Some(position) = position {
        values.insert(
            ":queue_sort_key".to_string(),
            AttributeValue::S(position.queue_sort_key.clone()),
        );
        values.insert(
            ":job_pk".to_string(),
            AttributeValue::S(position.job_pk.clone()),
        );
        "SET #item_type = :item_type, #cursor_queue_shard = :queue_shard, \
         #cursor_queue_sort_key = :queue_sort_key, #cursor_job_pk = :job_pk, \
         #cursor_version = :next_version, #updated_at = :updated_at \
         REMOVE #last_sort_key, #expires_at"
    } else {
        "SET #item_type = :item_type, #cursor_queue_shard = :queue_shard, \
         #cursor_version = :next_version, #updated_at = :updated_at \
         REMOVE #cursor_queue_sort_key, #cursor_job_pk, #last_sort_key, #expires_at"
    };
    let condition_expression = if expected_version == 0 {
        "attribute_not_exists(#cursor_version)"
    } else {
        values.insert(
            ":expected_version".to_string(),
            AttributeValue::N(expected_version.to_string()),
        );
        "#cursor_version = :expected_version"
    };

    UpdateItemInput::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(queue_cursor_pk(shard)))
        .update_expression(update_expression)
        .condition_expression(condition_expression)
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

fn update_was_stale(error: &SdkError<UpdateItemError>) -> bool {
    error
        .as_service_error()
        .is_some_and(UpdateItemError::is_conditional_check_failed_exception)
}

fn configured_queue_gsi_name() -> String {
    env::var("SPUR_INDEX_QUEUE_GSI_NAME").unwrap_or_else(|_| QUEUE_GSI_NAME.to_string())
}

impl DynamoDbJobStore {
    async fn release_active_job_if_owner(&self, record: &JobRecord) -> Result<()> {
        let delete = Delete::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(active_job_pk(&record.job_id)))
            .condition_expression("job_id = :job_id")
            .expression_attribute_values(":job_id", AttributeValue::S(record.job_id.clone()))
            .build()
            .map_err(dynamodb_error)?;
        let update = caller_quota_release_update(&self.table_name, &record.caller_id)?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().delete(delete).build())
            .transact_items(TransactWriteItem::builder().update(update).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_transaction_conflict(&error) => Ok(()),
            Err(error) => Err(dynamodb_error(error)),
        }
    }
}

fn job_pk(job_id: &str) -> String {
    format!("{JOB_PK_PREFIX}{job_id}")
}

fn dedupe_pk(key: &JobKey) -> String {
    format!(
        "{DEDUPE_PK_PREFIX}{}#{}#{}#{}",
        key.source, key.package, key.revision, key.source_url_hash
    )
}

fn caller_quota_pk(caller_id: &str) -> String {
    format!("{CALLER_QUOTA_PK_PREFIX}{caller_id}")
}

fn caller_rate_pk(caller_id: &str, window_epoch_minute: u64) -> String {
    format!("{CALLER_RATE_PK_PREFIX}{caller_id}#{window_epoch_minute}")
}

fn active_job_pk(job_id: &str) -> String {
    format!("{ACTIVE_JOB_PK_PREFIX}{job_id}")
}

// ─── Bounded queueing key helpers ──────────────────────────────────────────

/// `OWNER#<kind>#<id>` — partition key for owner-scoped job/accounting items.
fn owner_pk(kind: BacklogOwnerKind, id: &str) -> String {
    format!("{OWNER_PK_PREFIX}{}#{id}", kind.as_str())
}

/// `OWNER#<kind>#<id>#QUOTA` — partition key for the per-owner queued/running
/// counter item.
fn owner_quota_pk(kind: BacklogOwnerKind, id: &str) -> String {
    format!(
        "{OWNER_PK_PREFIX}{}#{id}{OWNER_QUOTA_SUFFIX}",
        kind.as_str()
    )
}

/// `GLOBAL#QUEUE#<shard>` — partition key for a sharded global queued counter.
fn global_queue_pk(shard: &str) -> String {
    format!("{GLOBAL_QUEUE_PK_PREFIX}{shard}")
}

/// `CURSOR#<shard>` — partition key for the persisted drainer scan cursor.
fn queue_cursor_pk(shard: &str) -> String {
    format!("{QUEUE_CURSOR_PK_PREFIX}{shard}")
}

/// `RUNNING#<job_id>` — partition key for the exactly-once running release
/// token created at dispatch time and deleted on the terminal transition.
fn running_token_pk(job_id: &str) -> String {
    format!("{RUNNING_TOKEN_PK_PREFIX}{job_id}")
}

fn global_running_token_pk(slot: u32) -> String {
    format!("{GLOBAL_RUNNING_TOKEN_PK_PREFIX}{slot}")
}

/// Deterministic FNV-1a 64-bit hash so queue shard assignment is stable across
/// processes (the default `HashMap` hasher uses random state and must not be
/// used for persisted sharding).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Stable queue shard for a job: `hash(owner_kind, owner_id, job_id) % count`,
/// formatted as a zero-padded 2-digit string so the sparse queue GSI groups
/// shards in stable lexicographic order. Returns `"00"` when `count` is `0`.
fn queue_shard_for(
    kind: BacklogOwnerKind,
    owner_id: &str,
    job_id: &str,
    shard_count: u32,
) -> String {
    let count = shard_count.max(1);
    let mut bytes = Vec::with_capacity(kind.as_str().len() + owner_id.len() + job_id.len() + 2);
    bytes.extend_from_slice(kind.as_str().as_bytes());
    bytes.push(b'#');
    bytes.extend_from_slice(owner_id.as_bytes());
    bytes.push(b'#');
    bytes.extend_from_slice(job_id.as_bytes());
    let index = fnv1a_64(&bytes) % u64::from(count);
    format!("{index:02}")
}

/// Lexicographically-ordered sparse queue GSI sort key:
/// `<next_eligible_at padded>#<queued_at>#<job_id>`. Zero-padding the seconds
/// prefix keeps GSI scans in strict chronological order; `queued_at` and
/// `job_id` are stable per-job tie-breakers.
fn queue_sort_key_for(next_eligible_at: u64, queued_at: &str, job_id: &str) -> String {
    format!("{next_eligible_at:0QUEUE_SORT_KEY_PAD$}#{queued_at}#{job_id}")
}

/// Compute the sparse queue-GSI attributes for a queued job and write them into
/// the supplied DynamoDB item map. Returns whether any attributes were written.
fn write_queue_gsi_attributes(
    item: &mut HashMap<String, AttributeValue>,
    kind: BacklogOwnerKind,
    owner_id: &str,
    job_id: &str,
    next_eligible_at: u64,
    queued_at: &str,
    shard_count: u32,
) -> bool {
    let shard = queue_shard_for(kind, owner_id, job_id, shard_count);
    let sort_key = queue_sort_key_for(next_eligible_at, queued_at, job_id);
    item.insert("queue_shard".to_string(), AttributeValue::S(shard));
    item.insert("queue_sort_key".to_string(), AttributeValue::S(sort_key));
    item.insert(
        "next_eligible_at".to_string(),
        AttributeValue::N(next_eligible_at.to_string()),
    );
    true
}

fn now_string() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.to_string()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn job_item(record: &JobRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S(job_pk(&record.job_id)));
    item.insert(
        "item_type".to_string(),
        AttributeValue::S("job".to_string()),
    );
    item.insert(
        "job_id".to_string(),
        AttributeValue::S(record.job_id.clone()),
    );
    item.insert(
        "status".to_string(),
        AttributeValue::S(record.status.as_str().to_string()),
    );
    item.insert(
        "source".to_string(),
        AttributeValue::S(record.source.clone()),
    );
    item.insert(
        "package".to_string(),
        AttributeValue::S(record.package.clone()),
    );
    item.insert(
        "revision".to_string(),
        AttributeValue::S(record.revision.clone()),
    );
    item.insert(
        "source_url".to_string(),
        AttributeValue::S(record.source_url.clone()),
    );
    item.insert(
        "source_url_hash".to_string(),
        AttributeValue::S(record.source_url_hash.clone()),
    );
    item.insert(
        "source_kind".to_string(),
        AttributeValue::S(record.source_kind.clone()),
    );
    item.insert(
        "caller_id".to_string(),
        AttributeValue::S(record.caller_id.clone()),
    );
    item.insert(
        "attempt".to_string(),
        AttributeValue::N(record.attempt.to_string()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(record.created_at.clone()),
    );
    item.insert(
        "updated_at".to_string(),
        AttributeValue::S(record.updated_at.clone()),
    );
    insert_optional_string(&mut item, "execution_arn", record.execution_arn.as_deref());
    insert_optional_string(&mut item, "stage", record.stage.as_deref());
    insert_optional_number(&mut item, "snapshot_id", record.snapshot_id);
    if let Some(row_counts) = &record.row_counts {
        item.insert(
            "row_counts".to_string(),
            AttributeValue::S(row_counts.to_string()),
        );
    }
    insert_optional_string(&mut item, "error_code", record.error_code.as_deref());
    insert_optional_string(&mut item, "error_detail", record.error_detail.as_deref());
    // Sparse queue fields: present only while the job is queued. The GSI keys
    // (`queue_shard` / `queue_sort_key`) are written here so a queued record
    // appears in the sparse queue GSI; dispatch removes them.
    if let Some(owner_kind) = record.owner_kind {
        item.insert(
            "owner_kind".to_string(),
            AttributeValue::S(owner_kind.as_str().to_string()),
        );
    }
    insert_optional_string(&mut item, "owner_id", record.owner_id.as_deref());
    insert_optional_string(&mut item, "queue_shard", record.queue_shard.as_deref());
    insert_optional_string(
        &mut item,
        "queue_sort_key",
        record.queue_sort_key.as_deref(),
    );
    if let Some(next_eligible_at) = record.next_eligible_at {
        item.insert(
            "next_eligible_at".to_string(),
            AttributeValue::N(next_eligible_at.to_string()),
        );
    }
    insert_optional_string(&mut item, "dispatched_at", record.dispatched_at.as_deref());
    item
}

fn active_job_item(record: &JobRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert(
        "pk".to_string(),
        AttributeValue::S(active_job_pk(&record.job_id)),
    );
    item.insert(
        "item_type".to_string(),
        AttributeValue::S("active_job".to_string()),
    );
    item.insert(
        "job_id".to_string(),
        AttributeValue::S(record.job_id.clone()),
    );
    item.insert(
        "caller_id".to_string(),
        AttributeValue::S(record.caller_id.clone()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(record.created_at.clone()),
    );
    item.insert(
        "expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    item
}

fn caller_quota_acquire_update(
    table_name: &str,
    caller_id: &str,
    max_active_jobs_per_caller: u32,
) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":limit".to_string(),
        AttributeValue::N(max_active_jobs_per_caller.to_string()),
    );
    values.insert(
        ":item_type".to_string(),
        AttributeValue::S("caller_quota".to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(caller_quota_pk(caller_id)))
        .update_expression(
            "SET item_type = if_not_exists(item_type, :item_type), #updated_at = :updated_at, expires_at = :expires_at ADD active_count :one",
        )
        .condition_expression("attribute_not_exists(active_count) OR active_count < :limit")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

fn caller_rate_acquire_update(
    table_name: &str,
    caller_id: &str,
    window_epoch_minute: u64,
    now_unix_secs: u64,
    max_requests_per_minute: u32,
) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":limit".to_string(),
        AttributeValue::N(max_requests_per_minute.to_string()),
    );
    values.insert(
        ":item_type".to_string(),
        AttributeValue::S("caller_rate".to_string()),
    );
    values.insert(
        ":caller_id".to_string(),
        AttributeValue::S(caller_id.to_owned()),
    );
    values.insert(
        ":window_epoch_minute".to_string(),
        AttributeValue::N(window_epoch_minute.to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs + RATE_WINDOW_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key(
            "pk",
            AttributeValue::S(caller_rate_pk(caller_id, window_epoch_minute)),
        )
        .update_expression(
            "SET item_type = if_not_exists(item_type, :item_type), caller_id = if_not_exists(caller_id, :caller_id), window_epoch_minute = if_not_exists(window_epoch_minute, :window_epoch_minute), #updated_at = :updated_at, expires_at = :expires_at ADD request_count :one",
        )
        .condition_expression("attribute_not_exists(request_count) OR request_count < :limit")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

fn caller_quota_release_update(table_name: &str, caller_id: &str) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":minus_one".to_string(),
        AttributeValue::N("-1".to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(caller_quota_pk(caller_id)))
        .update_expression(
            "SET #updated_at = :updated_at, expires_at = :expires_at ADD active_count :minus_one",
        )
        .condition_expression("active_count >= :one")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

// ─── Bounded queueing DynamoDB builders ────────────────────────────────────
//
// Pure builder functions returning DynamoDB `Put`/`Update`/`Delete` actions.
// They are kept side-effect free so the in-file unit tests can assert their
// shape without a live DynamoDB endpoint, mirroring the existing
// `caller_quota_acquire_update` / `caller_quota_release_update` pattern.

/// Per-owner queued counter increment with a hard cap. The condition
/// `queued_count < :limit` enforces the cap transactionally; a
/// `ConditionalCheckFailed` cancels the whole enqueue transaction so no job,
/// dedupe pointer, or GSI attributes are written (no partial writes).
fn owner_quota_enqueue_update(
    table_name: &str,
    owner: &BacklogOwner,
    max_queued_per_owner: u32,
) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":limit".to_string(),
        AttributeValue::N(max_queued_per_owner.to_string()),
    );
    values.insert(
        ":item_type".to_string(),
        AttributeValue::S("owner_quota".to_string()),
    );
    values.insert(
        ":owner_kind".to_string(),
        AttributeValue::S(owner.kind.as_str().to_string()),
    );
    values.insert(":owner_id".to_string(), AttributeValue::S(owner.id.clone()));
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(owner.quota_pk()))
        .update_expression(
            "SET item_type = if_not_exists(item_type, :item_type), owner_kind = if_not_exists(owner_kind, :owner_kind), owner_id = if_not_exists(owner_id, :owner_id), #updated_at = :updated_at, expires_at = :expires_at ADD queued_count :one",
        )
        .condition_expression("attribute_not_exists(queued_count) OR queued_count < :limit")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

/// Dispatch transition counter move: `queued_count -1, running_count +1` on the
/// owner quota item. Guards against running-count overflow with
/// `running_count < :running_limit` when a finite cap is configured.
fn owner_quota_dispatch_update(
    table_name: &str,
    owner: &BacklogOwner,
    max_running_per_owner: u32,
) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":minus_one".to_string(),
        AttributeValue::N("-1".to_string()),
    );
    values.insert(
        ":running_limit".to_string(),
        AttributeValue::N(max_running_per_owner.to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(owner.quota_pk()))
        .update_expression(
            "SET #updated_at = :updated_at, expires_at = :expires_at ADD queued_count :minus_one, running_count :one",
        )
        .condition_expression("(attribute_not_exists(running_count) OR running_count < :running_limit) AND (attribute_exists(queued_count) AND queued_count >= :one)")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

/// Terminal release counter move: `running_count -1` on the owner quota item.
/// The `running_count >= :one` guard prevents underflow.
fn owner_quota_release_update(table_name: &str, owner: &BacklogOwner) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":minus_one".to_string(),
        AttributeValue::N("-1".to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(owner.quota_pk()))
        .update_expression(
            "SET #updated_at = :updated_at, expires_at = :expires_at ADD running_count :minus_one",
        )
        .condition_expression("attribute_exists(running_count) AND running_count >= :one")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

/// Effective number of global counter shards for the sharded
/// `GLOBAL#QUEUE#<shard>` items.
///
/// Capped at `min(shard_count, max_queued_global)` so a small global cap does
/// not inflate total capacity. Previously the per-shard limit was floored to
/// 1, which meant `max_queued_global=1, shard_count=16` allowed one job per
/// shard (16 total) despite a cap of 1.
///
/// With effective shards `E = min(shard_count, max_queued_global)` and a
/// per-shard budget of `floor(max_queued_global / E) ≥ 1`, the total capacity
/// `E × floor(max_queued_global / E)` is always `≤ max_queued_global`.
fn global_counter_shard_count(max_queued_global: u32, shard_count: u32) -> u32 {
    shard_count.max(1).min(max_queued_global.max(1))
}

/// Per-shard budget for the sharded global queued counter.
///
/// The global cap is enforced across `global_counter_shard_count` independent
/// `GLOBAL#QUEUE#<shard>` items to avoid a single write-hot partition. Each
/// effective shard is allowed at most this many queued jobs. Floor division
/// keeps the total service-wide capacity at or below the configured cap:
/// `effective_shards × per_shard_budget ≤ max_queued_global`. This is
/// deliberately conservative — the spec calls global enforcement "approximate"
/// and relies on the per-owner cap as the hard fairness boundary.
fn global_queue_per_shard_limit(max_queued_global: u32, shard_count: u32) -> u32 {
    let effective = global_counter_shard_count(max_queued_global, shard_count);
    (max_queued_global / effective).max(1)
}

/// Deterministic global counter shard for a job.
///
/// Maps the job identity to `[0, effective_global_shards)` using the same
/// FNV-1a hash as [`queue_shard_for`] but modulo the effective global shard
/// count. This is **separate** from the queue GSI shard (which uses
/// `shard_count`) because the effective global shard count may be smaller than
/// the queue shard count when `max_queued_global < shard_count`.
///
/// Both enqueue and dispatch MUST compute this from the same inputs —
/// `(owner_kind, owner_id, job_id, max_queued_global, shard_count)` — so
/// dispatch decrements the exact `GLOBAL#QUEUE#<shard>` counter that enqueue
/// incremented. All inputs are available on the job record (owner_kind,
/// owner_id, job_id) plus the runtime config, so no extra metadata needs to be
/// persisted.
fn global_counter_shard_for(
    kind: BacklogOwnerKind,
    owner_id: &str,
    job_id: &str,
    max_queued_global: u32,
    shard_count: u32,
) -> String {
    let effective = global_counter_shard_count(max_queued_global, shard_count);
    let mut bytes = Vec::with_capacity(kind.as_str().len() + owner_id.len() + job_id.len() + 2);
    bytes.extend_from_slice(kind.as_str().as_bytes());
    bytes.push(b'#');
    bytes.extend_from_slice(owner_id.as_bytes());
    bytes.push(b'#');
    bytes.extend_from_slice(job_id.as_bytes());
    let index = fnv1a_64(&bytes) % u64::from(effective);
    format!("{index:02}")
}

/// Sharded global queued counter increment. The shard key is precomputed by the
/// caller (derived from the job) so all increments for a job land on the same
/// shard. The per-shard condition limit is [`global_queue_per_shard_limit`] so
/// the aggregate service-wide capacity stays at or below `max_queued_global`.
fn global_queue_enqueue_update(
    table_name: &str,
    shard: &str,
    max_queued_global: u32,
    shard_count: u32,
) -> Result<Update> {
    let per_shard_limit = global_queue_per_shard_limit(max_queued_global, shard_count);
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":limit".to_string(),
        AttributeValue::N(per_shard_limit.to_string()),
    );
    values.insert(
        ":item_type".to_string(),
        AttributeValue::S("global_queue".to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(global_queue_pk(shard)))
        .update_expression(
            "SET item_type = if_not_exists(item_type, :item_type), #updated_at = :updated_at, expires_at = :expires_at ADD queued_count :one",
        )
        .condition_expression("attribute_not_exists(queued_count) OR queued_count < :limit")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

/// Sharded global queued counter decrement, used in the dispatch transaction to
/// free global queued capacity when a job transitions from `queued` to
/// `dispatching`. The `queued_count >= :one` guard prevents underflow.
fn global_queue_dispatch_update(table_name: &str, shard: &str) -> Result<Update> {
    let mut names = HashMap::new();
    names.insert("#updated_at".to_string(), "updated_at".to_string());
    let mut values = HashMap::new();
    values.insert(":one".to_string(), AttributeValue::N("1".to_string()));
    values.insert(
        ":minus_one".to_string(),
        AttributeValue::N("-1".to_string()),
    );
    values.insert(":updated_at".to_string(), AttributeValue::S(now_string()));
    values.insert(
        ":expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + ACTIVE_JOB_TTL_SECS).to_string()),
    );
    Update::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(global_queue_pk(shard)))
        .update_expression(
            "SET #updated_at = :updated_at, expires_at = :expires_at ADD queued_count :minus_one",
        )
        .condition_expression("attribute_exists(queued_count) AND queued_count >= :one")
        .set_expression_attribute_names(Some(names))
        .set_expression_attribute_values(Some(values))
        .build()
        .map_err(dynamodb_error)
}

/// `RUNNING#<job_id>` release-token item. Created at dispatch time and deleted
/// in the terminal-release transaction; its existence is the exactly-once
/// guard for running-quota release.
fn running_token_item(
    job_id: &str,
    updated_at: &str,
    owner: &BacklogOwner,
    queue_shard: &str,
    global_running_token_pk: Option<&str>,
) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert(
        "pk".to_string(),
        AttributeValue::S(running_token_pk(job_id)),
    );
    item.insert(
        "item_type".to_string(),
        AttributeValue::S("running_token".to_string()),
    );
    item.insert("job_id".to_string(), AttributeValue::S(job_id.to_string()));
    item.insert(
        "owner_kind".to_string(),
        AttributeValue::S(owner.kind.as_str().to_string()),
    );
    item.insert("owner_id".to_string(), AttributeValue::S(owner.id.clone()));
    item.insert(
        "queue_shard".to_string(),
        AttributeValue::S(queue_shard.to_string()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(updated_at.to_string()),
    );
    item.insert(
        "expires_at".to_string(),
        AttributeValue::N((now_unix_secs() + RUNNING_TOKEN_TTL_SECS).to_string()),
    );
    insert_optional_string(
        &mut item,
        "global_running_token_pk",
        global_running_token_pk,
    );
    item
}

fn global_running_token_item(
    slot: u32,
    job_id: &str,
    updated_at: &str,
) -> HashMap<String, AttributeValue> {
    HashMap::from([
        (
            "pk".to_string(),
            AttributeValue::S(global_running_token_pk(slot)),
        ),
        (
            "item_type".to_string(),
            AttributeValue::S("global_running_token".to_string()),
        ),
        ("job_id".to_string(), AttributeValue::S(job_id.to_string())),
        (
            "created_at".to_string(),
            AttributeValue::S(updated_at.to_string()),
        ),
        (
            "expires_at".to_string(),
            AttributeValue::N((now_unix_secs() + RUNNING_TOKEN_TTL_SECS).to_string()),
        ),
    ])
}

/// Delete of the `RUNNING#<job_id>` release token, conditioned on its
/// existence. A `ConditionalCheckFailed` here means the token was already
/// released, so the caller treats the release as already complete and skips
/// the counter decrement (idempotent).
fn running_token_delete(table_name: &str, job_id: &str) -> Result<Delete> {
    Delete::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(running_token_pk(job_id)))
        .condition_expression("attribute_exists(pk)")
        .build()
        .map_err(dynamodb_error)
}

fn global_running_token_delete(table_name: &str, token_pk: &str, job_id: &str) -> Result<Delete> {
    Delete::builder()
        .table_name(table_name)
        .key("pk", AttributeValue::S(token_pk.to_string()))
        .condition_expression("job_id = :job_id")
        .expression_attribute_values(":job_id", AttributeValue::S(job_id.to_string()))
        .build()
        .map_err(dynamodb_error)
}

fn dedupe_item(key: &JobKey, record: &JobRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("pk".to_string(), AttributeValue::S(dedupe_pk(key)));
    item.insert(
        "item_type".to_string(),
        AttributeValue::S("dedupe".to_string()),
    );
    item.insert(
        "job_id".to_string(),
        AttributeValue::S(record.job_id.clone()),
    );
    item.insert("source".to_string(), AttributeValue::S(key.source.clone()));
    item.insert(
        "package".to_string(),
        AttributeValue::S(key.package.clone()),
    );
    item.insert(
        "revision".to_string(),
        AttributeValue::S(key.revision.clone()),
    );
    item.insert(
        "source_url_hash".to_string(),
        AttributeValue::S(key.source_url_hash.clone()),
    );
    item.insert(
        "created_at".to_string(),
        AttributeValue::S(record.created_at.clone()),
    );
    item.insert(
        "updated_at".to_string(),
        AttributeValue::S(record.updated_at.clone()),
    );
    item
}

fn insert_optional_string(
    item: &mut HashMap<String, AttributeValue>,
    name: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        item.insert(name.to_string(), AttributeValue::S(value.to_string()));
    }
}

fn insert_optional_number(
    item: &mut HashMap<String, AttributeValue>,
    name: &str,
    value: Option<i64>,
) {
    if let Some(value) = value {
        item.insert(name.to_string(), AttributeValue::N(value.to_string()));
    }
}

fn job_record_from_item(item: &HashMap<String, AttributeValue>) -> Result<JobRecord> {
    let status = JobStatus::from_str(&string_attr(item, "status")?)
        .map_err(|error| malformed_item(error.to_string()))?;
    let owner_kind = optional_string_attr(item, "owner_kind")?
        .map(|raw| {
            BacklogOwnerKind::from_str(&raw).map_err(|error| malformed_item(error.to_string()))
        })
        .transpose()?;
    Ok(JobRecord {
        job_id: string_attr(item, "job_id")?,
        status,
        source: string_attr(item, "source")?,
        package: string_attr(item, "package")?,
        revision: string_attr(item, "revision")?,
        source_url: string_attr(item, "source_url")?,
        source_url_hash: string_attr(item, "source_url_hash")?,
        source_kind: string_attr(item, "source_kind")?,
        caller_id: string_attr(item, "caller_id")?,
        execution_arn: optional_string_attr(item, "execution_arn")?,
        attempt: number_attr(item, "attempt")?.parse().map_err(|error| {
            malformed_item(format!("invalid attempt value for job item: {error}"))
        })?,
        stage: optional_string_attr(item, "stage")?,
        snapshot_id: optional_number_attr(item, "snapshot_id")?,
        row_counts: optional_json_attr(item, "row_counts")?,
        error_code: optional_string_attr(item, "error_code")?,
        error_detail: optional_string_attr(item, "error_detail")?,
        created_at: string_attr(item, "created_at")?,
        updated_at: string_attr(item, "updated_at")?,
        owner_kind,
        owner_id: optional_string_attr(item, "owner_id")?,
        queue_shard: optional_string_attr(item, "queue_shard")?,
        queue_sort_key: optional_string_attr(item, "queue_sort_key")?,
        next_eligible_at: optional_u64_attr(item, "next_eligible_at")?,
        dispatched_at: optional_string_attr(item, "dispatched_at")?,
    })
}

fn string_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(value.clone()),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a string"))),
        None => Err(malformed_item(format!("missing string attribute {name}"))),
    }
}

fn optional_string_attr(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Option<String>> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(Some(value.clone())),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a string"))),
        None => Ok(None),
    }
}

fn number_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => Ok(value.clone()),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a number"))),
        None => Err(malformed_item(format!("missing number attribute {name}"))),
    }
}

fn optional_number_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<Option<i64>> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse()
            .map(Some)
            .map_err(|error| malformed_item(format!("invalid number attribute {name}: {error}"))),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a number"))),
        None => Ok(None),
    }
}

fn optional_u64_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<Option<u64>> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse()
            .map(Some)
            .map_err(|error| malformed_item(format!("invalid u64 attribute {name}: {error}"))),
        Some(_) => Err(malformed_item(format!("attribute {name} is not a number"))),
        None => Ok(None),
    }
}

fn optional_json_attr(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Option<serde_json::Value>> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => serde_json::from_str(value)
            .map(Some)
            .map_err(|error| malformed_item(format!("invalid json attribute {name}: {error}"))),
        Some(_) => Err(malformed_item(format!(
            "attribute {name} is not a json string"
        ))),
        None => Ok(None),
    }
}

fn is_transaction_conflict(error: &SdkError<TransactWriteItemsError>) -> bool {
    match error {
        SdkError::ServiceError(service_error) => {
            transact_write_error_is_conflict(service_error.err())
        }
        _ => transaction_conflict_message(&error.to_string()),
    }
}

fn transact_write_error_is_conflict(error: &TransactWriteItemsError) -> bool {
    match error {
        TransactWriteItemsError::TransactionCanceledException(error) => {
            error
                .cancellation_reasons()
                .iter()
                .any(|reason| cancellation_reason_is_conflict(reason.code()))
                || error.message().is_some_and(transaction_conflict_message)
        }
        TransactWriteItemsError::TransactionInProgressException(_) => true,
        _ => transaction_conflict_message(&error.to_string()),
    }
}

fn cancellation_reason_is_conflict(code: Option<&str>) -> bool {
    matches!(code, Some("ConditionalCheckFailed" | "TransactionConflict"))
}

fn transaction_conflict_message(message: &str) -> bool {
    message.contains("TransactionCanceledException")
        || message.contains("ConditionalCheckFailed")
        || message.contains("TransactionConflict")
}

/// Extract the `TransactionCanceledException` cancellation reasons from a
/// `TransactWriteItems` SDK error, if present. DynamoDB orders the reasons to
/// match the request item ordering, so the index of the first failing reason
/// identifies which transaction item caused the cancellation.
fn cancellation_reasons(
    error: &SdkError<TransactWriteItemsError>,
) -> Option<&[CancellationReason]> {
    match error {
        SdkError::ServiceError(service_error) => {
            if let TransactWriteItemsError::TransactionCanceledException(inner) =
                service_error.err()
            {
                Some(inner.cancellation_reasons())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// What kind of cancellation a specific transaction item experienced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemCancellation {
    /// `ConditionalCheckFailed` — the item's condition expression evaluated
    /// false. Definitive: the semantic reason maps from the item position.
    ConditionalCheckFailed,
    /// `TransactionConflict` — concurrent write contention on the item.
    /// Transient/retryable; the item's condition may have been satisfiable.
    TransactionConflict,
}

/// Classify a single `CancellationReason` code into a typed cancellation kind.
/// Returns `None` for `"None"` (success) and unrecognized codes.
fn item_cancellation_kind(code: Option<&str>) -> Option<ItemCancellation> {
    match code {
        Some("ConditionalCheckFailed") => Some(ItemCancellation::ConditionalCheckFailed),
        Some("TransactionConflict") => Some(ItemCancellation::TransactionConflict),
        _ => None,
    }
}

/// Classification of an enqueue transaction cancellation so the caller returns
/// the correct rejection reason. The enqueue transaction items are ordered:
/// `[job_put(0), dedupe_put(1), owner_update(2), global_update?(3)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueCancellation {
    /// A duplicate dedupe pointer beat us (item 1) — caller should return
    /// `EnqueueOutcome::Existing`.
    Duplicate,
    /// Owner queued cap overflow (item 2) → `JobsError::QueueFull`.
    OwnerFull,
    /// Global queued cap overflow (item 3) → `JobsError::GlobalQueueFull`.
    GlobalFull,
    /// A `TransactionConflict` (concurrent write contention) at an owner/global
    /// counter item. The quota may not be full — this is a transient retryable
    /// conflict, not a definitive capacity rejection. The caller should surface
    /// `Conflict` so the caller can retry rather than falsely reporting the
    /// queue as full.
    TransientConflict,
    /// No specific item identified (e.g. job pk collision or missing reasons);
    /// caller falls back to a dedupe re-check.
    Unknown,
}

fn classify_enqueue_cancellation(
    error: &SdkError<TransactWriteItemsError>,
    has_global_item: bool,
) -> EnqueueCancellation {
    classify_enqueue_cancellation_from(
        cancellation_reasons(error),
        has_global_item,
        enqueue_unknown_is_retryable_conflict(error),
    )
}

/// Pure classification of an enqueue cancellation from its extracted signals,
/// kept separate from the `SdkError` so the no-reasons transaction-conflict
/// fallback is directly unit-testable without constructing a full `SdkError`
/// (the AWS SDK's `HttpResponse` raw payload is awkward to build in tests).
///
/// - `reasons`: structured `TransactionCanceledException` cancellation
///   reasons, when present.
/// - `no_reasons_is_retryable_conflict`: whether the error signals retryable
///   transaction-level contention when item-level reasons are absent or
///   unresolved (see [`enqueue_unknown_is_retryable_conflict`]).
///
/// Item-level reasons take precedence: a definitive `ConditionalCheckFailed`
/// at a quota position is a real capacity rejection. Only when the reasons are
/// absent or resolve to [`EnqueueCancellation::Unknown`] do we consult the
/// retryable flag — a `TransactionInProgressException` or message-only
/// `TransactionConflict` upgrades to [`EnqueueCancellation::TransientConflict`]
/// so the caller retries (`JobsError::Conflict`) instead of falling through to
/// the owner-cap `QueueFull` default. A message-only `ConditionalCheckFailed`
/// keeps `Unknown` so the dedupe-recheck / `QueueFull` fallback is preserved.
fn classify_enqueue_cancellation_from(
    reasons: Option<&[CancellationReason]>,
    has_global_item: bool,
    no_reasons_is_retryable_conflict: bool,
) -> EnqueueCancellation {
    let classified = match reasons {
        Some(reasons) => classify_enqueue_reasons(reasons, has_global_item),
        None => EnqueueCancellation::Unknown,
    };
    if classified == EnqueueCancellation::Unknown && no_reasons_is_retryable_conflict {
        EnqueueCancellation::TransientConflict
    } else {
        classified
    }
}

/// Whether an enqueue transaction error whose item-level cancellation reasons
/// are absent or unresolved signals retryable transaction-level contention
/// rather than a capacity-style conditional failure.
///
/// This covers the errors that reach the conflict branch of
/// [`is_transaction_conflict`] but carry no populated item reasons:
/// - A typed `TransactionInProgressException` — the transaction could not
///   start because another is in flight on the same items.
/// - A message-only `TransactionConflict` — the error string contains the
///   conflict code but no structured reasons were attached.
///
/// Both are transient: the queue may have capacity and a retry can succeed, so
/// the caller should surface `JobsError::Conflict`. A message-only
/// `ConditionalCheckFailed` is NOT retryable here — it is a capacity-style
/// rejection (e.g. an owner-cap collision) and is left for the dedupe-recheck
/// / owner-cap `QueueFull` default.
fn enqueue_unknown_is_retryable_conflict(error: &SdkError<TransactWriteItemsError>) -> bool {
    let is_in_progress = matches!(
        error,
        SdkError::ServiceError(service_error) if matches!(
            service_error.err(),
            TransactWriteItemsError::TransactionInProgressException(_)
        )
    );
    is_in_progress || error.to_string().contains("TransactionConflict")
}

/// Pure classification of enqueue cancellation reasons by item position AND
/// reason code. Kept separate from the SDK error so unit tests can exercise
/// the position mapping without constructing a full `SdkError`.
///
/// `ConditionalCheckFailed` at a quota position is definitive (the cap is
/// full). `TransactionConflict` at any position is transient contention — the
/// capacity may exist but a concurrent transaction collided. We distinguish
/// the two so concurrent enqueue contention does not produce false
/// `queue_full`/`global_queue_full` rejections.
fn classify_enqueue_reasons(
    reasons: &[CancellationReason],
    has_global_item: bool,
) -> EnqueueCancellation {
    // Transaction items: [job_put(0), dedupe_put(1), owner_update(2), global?(3)]
    // Iterate in order; the first failing item identifies the primary cause.
    for (index, reason) in reasons.iter().enumerate() {
        match item_cancellation_kind(reason.code()) {
            Some(ItemCancellation::ConditionalCheckFailed) => {
                return match index {
                    0 => EnqueueCancellation::Unknown, // job pk collision (UUID, improbable)
                    1 => EnqueueCancellation::Duplicate,
                    2 => EnqueueCancellation::OwnerFull,
                    3 if has_global_item => EnqueueCancellation::GlobalFull,
                    _ => EnqueueCancellation::Unknown,
                };
            }
            Some(ItemCancellation::TransactionConflict) => {
                // Concurrent write contention — capacity may exist. Do NOT
                // report as queue_full/global_queue_full.
                return EnqueueCancellation::TransientConflict;
            }
            None => {}
        }
    }
    EnqueueCancellation::Unknown
}

/// Classification of a release transaction cancellation. The release items are
/// ordered `[token_delete(0), quota_update(1)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseCancellation {
    /// The `RUNNING#<job_id>` token was already deleted (item 0, code
    /// `ConditionalCheckFailed`) — the release is already complete; the caller
    /// treats it as a no-op.
    TokenGone,
    /// An owner-quota update or global-token delete condition failed (item 1+
    /// with `ConditionalCheckFailed`) while the per-job token still exists.
    /// The caller should surface this so the reconciler can repair it.
    QuotaConflict,
    /// A `TransactionConflict` at either item — concurrent write contention.
    /// The token may still exist (not gone) and the counter may be consistent;
    /// the caller should retry rather than falsely treating the release as done
    /// or permanently failed.
    TransientConflict,
    /// Unknown cancellation — caller falls back to conservative handling.
    Unknown,
}

fn classify_release_cancellation(error: &SdkError<TransactWriteItemsError>) -> ReleaseCancellation {
    let Some(reasons) = cancellation_reasons(error) else {
        return ReleaseCancellation::Unknown;
    };
    classify_release_reasons(reasons)
}

/// Pure classification of release cancellation reasons by item position AND
/// reason code. `ConditionalCheckFailed` at the token (item 0) means the token
/// is genuinely gone. `TransactionConflict` at the token means contention — the
/// token likely still exists and must not be treated as already released.
fn classify_release_reasons(reasons: &[CancellationReason]) -> ReleaseCancellation {
    for (index, reason) in reasons.iter().enumerate() {
        match item_cancellation_kind(reason.code()) {
            Some(ItemCancellation::ConditionalCheckFailed) => {
                return match index {
                    0 => ReleaseCancellation::TokenGone,
                    1.. => ReleaseCancellation::QuotaConflict,
                };
            }
            Some(ItemCancellation::TransactionConflict) => {
                return ReleaseCancellation::TransientConflict;
            }
            None => {}
        }
    }
    ReleaseCancellation::Unknown
}

fn dynamodb_error(error: impl fmt::Display) -> JobsError {
    JobsError::Db(Box::new(StringJobError(format!("dynamodb error: {error}"))))
}

fn malformed_item(message: String) -> JobsError {
    JobsError::Db(Box::new(StringJobError(format!(
        "malformed index job item: {message}"
    ))))
}

#[derive(Debug)]
struct StringJobError(String);

impl fmt::Display for StringJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl StdError for StringJobError {}

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("database error: {0}")]
    Db(Box<dyn StdError + Send + Sync>),
    #[error("conflicting index job")]
    Conflict,
    #[error("caller has too many active index jobs")]
    ConcurrentLimit,
    #[error("caller exceeded the indexing rate limit")]
    RateLimited,
    #[error("index job not found")]
    NotFound,
    #[error("owner backlog queue is full")]
    QueueFull,
    #[error("global backlog queue is full")]
    GlobalQueueFull,
    #[error("global running token pool is full")]
    GlobalRunningFull,
}

#[derive(Debug)]
struct InvalidJobStatus(String);

impl fmt::Display for InvalidJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid index job status: {}", self.0)
    }
}

impl StdError for InvalidJobStatus {}

#[derive(Debug)]
struct InvalidOwnerKind(String);

impl fmt::Display for InvalidOwnerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid backlog owner kind: {}", self.0)
    }
}

impl StdError for InvalidOwnerKind {}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::{
        operation::transact_write_items::TransactWriteItemsError,
        types::{error::TransactionCanceledException, CancellationReason},
    };

    use super::*;

    #[test]
    fn queued_jobs_query_uses_complete_exclusive_start_key_and_one_sort_condition() {
        let cursor = QueuePageKey {
            queue_shard: "03".to_string(),
            queue_sort_key: "00000000042#queued#job-42".to_string(),
            job_pk: "JOB#job-42".to_string(),
        };

        let input =
            queued_jobs_query_input("jobs-table", "queue-index", "03", 42, 7, Some(&cursor))
                .expect("valid production query input");

        assert_eq!(input.table_name(), Some("jobs-table"));
        assert_eq!(input.index_name(), Some("queue-index"));
        assert_eq!(input.limit(), Some(7));
        assert_eq!(
            input.key_condition_expression(),
            Some("queue_shard = :shard AND queue_sort_key <= :ceiling")
        );
        assert!(!input
            .key_condition_expression()
            .expect("key condition")
            .contains(":after"));
        let exclusive_start_key = input
            .exclusive_start_key()
            .expect("cursor becomes ExclusiveStartKey");
        assert_eq!(exclusive_start_key.len(), 3);
        assert_eq!(
            exclusive_start_key["queue_shard"]
                .as_s()
                .expect("queue shard"),
            "03"
        );
        assert_eq!(
            exclusive_start_key["queue_sort_key"]
                .as_s()
                .expect("queue sort key"),
            "00000000042#queued#job-42"
        );
        assert_eq!(
            exclusive_start_key["pk"].as_s().expect("base table pk"),
            "JOB#job-42"
        );
    }

    #[test]
    fn queue_cursor_update_is_versioned_for_progress_and_wrap() {
        let cursor = QueuePageKey {
            queue_shard: "03".to_string(),
            queue_sort_key: "00000000042#queued#job-42".to_string(),
            job_pk: "JOB#job-42".to_string(),
        };
        let progress = queue_cursor_update_input("jobs-table", "03", 4, Some(&cursor), "now")
            .expect("progress update input");
        assert_eq!(
            progress.condition_expression(),
            Some("#cursor_version = :expected_version")
        );
        assert!(progress
            .update_expression()
            .expect("progress update expression")
            .contains("#cursor_queue_sort_key = :queue_sort_key"));
        assert!(progress
            .update_expression()
            .expect("progress update expression")
            .contains("#cursor_job_pk = :job_pk"));
        let names = progress
            .expression_attribute_names()
            .expect("cursor update attribute names");
        assert_eq!(
            names["#cursor_queue_shard"], "cursor_queue_shard",
            "cursor rows must not carry the sparse GSI partition attribute"
        );
        assert_eq!(
            names["#cursor_queue_sort_key"], "cursor_queue_sort_key",
            "cursor rows must not carry the sparse GSI range attribute"
        );
        assert_eq!(names["#cursor_job_pk"], "cursor_job_pk");
        assert!(!names.values().any(|name| name == "queue_shard"));
        assert!(!names.values().any(|name| name == "queue_sort_key"));

        let wrap = queue_cursor_update_input("jobs-table", "03", 5, None, "later")
            .expect("wrap update input");
        assert_eq!(
            wrap.condition_expression(),
            Some("#cursor_version = :expected_version")
        );
        let expression = wrap.update_expression().expect("wrap update expression");
        assert!(expression.contains("REMOVE #cursor_queue_sort_key, #cursor_job_pk"));
        assert!(expression.contains("#cursor_version = :next_version"));

        let initial = queue_cursor_update_input("jobs-table", "03", 0, None, "initial")
            .expect("initial update input");
        assert_eq!(
            initial.condition_expression(),
            Some("attribute_not_exists(#cursor_version)")
        );
    }

    #[test]
    fn queue_gsi_name_uses_runtime_environment() {
        let previous = env::var_os("SPUR_INDEX_QUEUE_GSI_NAME");
        env::set_var("SPUR_INDEX_QUEUE_GSI_NAME", "custom-queue-index");

        let configured = configured_queue_gsi_name();

        match previous {
            Some(value) => env::set_var("SPUR_INDEX_QUEUE_GSI_NAME", value),
            None => env::remove_var("SPUR_INDEX_QUEUE_GSI_NAME"),
        }
        assert_eq!(configured, "custom-queue-index");
    }

    #[test]
    fn transaction_canceled_conditional_check_is_conflict() {
        let error = TransactWriteItemsError::TransactionCanceledException(
            transaction_canceled_with_reason("ConditionalCheckFailed"),
        );

        assert!(transact_write_error_is_conflict(&error));
    }

    #[test]
    fn transaction_canceled_transaction_conflict_is_conflict() {
        let error = TransactWriteItemsError::TransactionCanceledException(
            transaction_canceled_with_reason("TransactionConflict"),
        );

        assert!(transact_write_error_is_conflict(&error));
    }

    #[test]
    fn transaction_canceled_validation_error_is_not_conflict() {
        let error = TransactWriteItemsError::TransactionCanceledException(
            transaction_canceled_with_reason("ValidationError"),
        );

        assert!(!transact_write_error_is_conflict(&error));
    }

    fn transaction_canceled_with_reason(code: &str) -> TransactionCanceledException {
        TransactionCanceledException::builder()
            .cancellation_reasons(CancellationReason::builder().code(code).build())
            .build()
    }

    // ─── Status parsing / serialization ────────────────────────────────────

    #[test]
    fn dispatching_status_round_trips() {
        assert_eq!(JobStatus::Dispatching.as_str(), "dispatching");
        assert_eq!(
            JobStatus::from_str("dispatching").unwrap(),
            JobStatus::Dispatching
        );
    }

    #[test]
    fn all_statuses_parse_and_serialize() {
        for status in [
            JobStatus::Queued,
            JobStatus::Dispatching,
            JobStatus::Running,
            JobStatus::Complete,
            JobStatus::Failed,
            JobStatus::Partial,
        ] {
            let raw = status.as_str();
            assert_eq!(JobStatus::from_str(raw).unwrap(), status, "{raw}");
        }
    }

    #[test]
    fn unknown_status_is_rejected() {
        assert!(JobStatus::from_str("pending").is_err());
    }

    #[test]
    fn dispatching_holds_running_quota() {
        assert!(JobStatus::Dispatching.holds_running_quota());
        assert!(JobStatus::Running.holds_running_quota());
        assert!(!JobStatus::Queued.holds_running_quota());
    }

    #[test]
    fn partial_is_terminal_for_quota() {
        assert!(JobStatus::Partial.is_terminal_for_quota());
        assert!(JobStatus::Complete.is_terminal_for_quota());
        assert!(JobStatus::Failed.is_terminal_for_quota());
        assert!(!JobStatus::Running.is_terminal_for_quota());
        assert!(!JobStatus::Dispatching.is_terminal_for_quota());
    }

    #[test]
    fn dispatching_status_serializes_to_lowercase() {
        let json = serde_json::to_string(&JobStatus::Dispatching).unwrap();
        assert_eq!(json, "\"dispatching\"");
        let parsed: JobStatus = serde_json::from_str("\"dispatching\"").unwrap();
        assert_eq!(parsed, JobStatus::Dispatching);
    }

    // ─── Owner key construction ────────────────────────────────────────────

    #[test]
    fn owner_pk_uses_kind_and_id() {
        let owner = BacklogOwner::caller("alice");
        assert_eq!(owner.pk(), "OWNER#caller#alice");
        assert_eq!(owner.quota_pk(), "OWNER#caller#alice#QUOTA");
    }

    #[test]
    fn owner_pk_supports_future_kinds() {
        let user = BacklogOwner::new(BacklogOwnerKind::User, "u-42");
        assert_eq!(user.pk(), "OWNER#user#u-42");
        let tenant_user = BacklogOwner::new(BacklogOwnerKind::TenantUser, "t1#u1");
        assert_eq!(tenant_user.pk(), "OWNER#tenant_user#t1#u1");
        assert_eq!(tenant_user.quota_pk(), "OWNER#tenant_user#t1#u1#QUOTA");
    }

    #[test]
    fn owner_kind_round_trips_serde() {
        for kind in [
            BacklogOwnerKind::Anonymous,
            BacklogOwnerKind::Caller,
            BacklogOwnerKind::User,
            BacklogOwnerKind::TenantUser,
        ] {
            let raw = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                serde_json::from_str::<BacklogOwnerKind>(&raw).unwrap(),
                kind
            );
        }
        assert_eq!(
            serde_json::to_string(&BacklogOwnerKind::TenantUser).unwrap(),
            "\"tenant_user\""
        );
    }

    #[test]
    fn owner_kind_from_str_rejects_unknown() {
        assert!(BacklogOwnerKind::from_str("root").is_err());
    }

    // ─── Queue shard / sort key ────────────────────────────────────────────

    #[test]
    fn queue_shard_is_stable_and_bounded() {
        let shard_a = queue_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 16);
        let shard_b = queue_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 16);
        assert_eq!(shard_a, shard_b, "shard must be deterministic");

        let value: u32 = shard_a.parse().unwrap();
        assert!(value < 16, "shard must be within shard_count");
        assert_eq!(shard_a.len(), 2, "shard is zero-padded to 2 digits");
    }

    #[test]
    fn queue_shard_respects_count() {
        let shard = queue_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 1);
        assert_eq!(shard, "00");
        // count=0 must not panic; clamped to 1.
        let _ = queue_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 0);
    }

    #[test]
    fn queue_sort_key_orders_chronologically() {
        let earlier = queue_sort_key_for(100, "100", "job-a");
        let later = queue_sort_key_for(2_000, "2000", "job-b");
        assert!(
            earlier < later,
            "earlier next_eligible_at must sort before later"
        );
        // The seconds prefix is zero-padded so lexicographic == numeric.
        assert!(earlier.starts_with("00000000100#"));
        assert!(later.starts_with("00000002000#"));
    }

    #[test]
    fn queue_sort_key_breaks_ties_by_queued_at_then_job_id() {
        let a = queue_sort_key_for(100, "10", "job-a");
        let b = queue_sort_key_for(100, "20", "job-b");
        assert!(a < b, "same eligibility: earlier queued_at sorts first");

        let c = queue_sort_key_for(100, "10", "job-a");
        let d = queue_sort_key_for(100, "10", "job-z");
        assert!(c < d, "same eligibility+queued_at: job_id tie-break");
    }

    // ─── Queue GSI attribute lifecycle ─────────────────────────────────────

    #[test]
    fn write_queue_gsi_attributes_populates_gsi_keys() {
        let mut item = HashMap::new();
        let wrote = write_queue_gsi_attributes(
            &mut item,
            BacklogOwnerKind::Caller,
            "alice",
            "job-1",
            123,
            "456",
            16,
        );
        assert!(wrote);
        assert!(matches!(
            item.get("queue_shard"),
            Some(AttributeValue::S(_))
        ));
        assert!(matches!(
            item.get("queue_sort_key"),
            Some(AttributeValue::S(_))
        ));
        assert!(matches!(
            item.get("next_eligible_at"),
            Some(AttributeValue::N(n)) if n == "123"
        ));
    }

    #[test]
    fn record_reports_gsi_attribute_presence() {
        let mut record = sample_record();
        assert!(!record.has_queue_gsi_attributes());
        record.queue_shard = Some("00".to_string());
        record.queue_sort_key = Some("k".to_string());
        record.next_eligible_at = Some(1);
        assert!(record.has_queue_gsi_attributes());
        // Removing one breaks the invariant.
        record.queue_sort_key = None;
        assert!(!record.has_queue_gsi_attributes());
    }

    // ─── Release token helpers ─────────────────────────────────────────────

    #[test]
    fn running_token_pk_is_namespaced() {
        assert_eq!(running_token_pk("job-7"), "RUNNING#job-7");
    }

    #[test]
    fn running_token_item_carries_owner_metadata() {
        let owner = BacklogOwner::caller("alice");
        let item = running_token_item("job-7", "now", &owner, "03", None);
        assert_eq!(
            item.get("pk").and_then(|v| v.as_s().ok()),
            Some(&"RUNNING#job-7".to_string())
        );
        assert_eq!(
            item.get("owner_kind").and_then(|v| v.as_s().ok()),
            Some(&"caller".to_string())
        );
        assert_eq!(
            item.get("owner_id").and_then(|v| v.as_s().ok()),
            Some(&"alice".to_string())
        );
        assert_eq!(
            item.get("queue_shard").and_then(|v| v.as_s().ok()),
            Some(&"03".to_string())
        );
    }

    #[test]
    fn global_queue_pk_is_shard_namespaced() {
        assert_eq!(global_queue_pk("07"), "GLOBAL#QUEUE#07");
    }

    // ─── Global queue per-shard budget ────────────────────────────────────

    #[test]
    fn per_shard_limit_divides_global_cap_by_shard_count() {
        // 100 / 16 = 6 (floor). Total capacity = 16 * 6 = 96 ≤ 100.
        assert_eq!(global_queue_per_shard_limit(100, 16), 6);
    }

    #[test]
    fn per_shard_limit_for_small_cap_uses_effective_shards() {
        // A small cap (1) across many shards must NOT allow 1 per shard (16
        // total). With effective shards = min(16, 1) = 1, the per-shard budget
        // is 1/1 = 1, and total capacity is 1 — matching the configured cap.
        assert_eq!(global_queue_per_shard_limit(1, 16), 1);
        // cap=15, shards=16: effective=min(16,15)=15, per_shard=15/15=1.
        assert_eq!(global_queue_per_shard_limit(15, 16), 1);
        // cap=3, shards=16: effective=min(16,3)=3, per_shard=3/3=1.
        assert_eq!(global_queue_per_shard_limit(3, 16), 1);
    }

    #[test]
    fn effective_shard_count_caps_at_max_queued_global() {
        // When the global cap is smaller than shard_count, the effective
        // counter shard count is the cap itself so each shard gets a
        // meaningful budget instead of a forced floor of 1.
        assert_eq!(global_counter_shard_count(1, 16), 1);
        assert_eq!(global_counter_shard_count(3, 16), 3);
        assert_eq!(global_counter_shard_count(5, 16), 5);
        // When the cap is larger, effective shards = shard_count.
        assert_eq!(global_counter_shard_count(100, 16), 16);
        assert_eq!(global_counter_shard_count(20, 16), 16);
    }

    #[test]
    fn per_shard_limit_total_never_exceeds_cap() {
        // Verify the core invariant: effective_shards * per_shard ≤ cap.
        // This is the fix for the bug where max_queued_global=1,
        // shard_count=16 allowed 16 jobs in production DynamoDB.
        for (cap, shards) in [
            (100u32, 16u32),
            (1, 1),
            (50, 8),
            (3, 16),
            (7, 3),
            (1, 16),
            (5, 16),
            (20, 16),
        ] {
            let effective = global_counter_shard_count(cap, shards);
            let per_shard = global_queue_per_shard_limit(cap, shards);
            let total = effective * per_shard;
            assert!(
                total <= cap,
                "effective={effective} * per_shard={per_shard} = {total} > cap={cap}"
            );
        }
    }

    #[test]
    fn global_queue_enqueue_update_uses_per_shard_limit_not_full_cap() {
        // The previous implementation used `max_queued_global` directly as the
        // per-shard limit, allowing shard_count * max_queued_global capacity.
        // Verify the builder now uses the divided per-shard budget.
        let cap = 100u32;
        let shards = 16u32;
        let expected_limit = global_queue_per_shard_limit(cap, shards);
        let update = global_queue_enqueue_update("tbl", "00", cap, shards).unwrap();
        let values = update.expression_attribute_values().unwrap();
        let limit_value = values
            .get(":limit")
            .and_then(|v| v.as_n().ok())
            .expect(":limit must be present");
        assert_eq!(
            limit_value.parse::<u32>().unwrap(),
            expected_limit,
            "enqueue condition must use per-shard budget, not full cap"
        );
        assert_ne!(
            expected_limit, cap,
            "per-shard limit must be strictly less than the global cap"
        );
    }

    #[test]
    fn global_queue_enqueue_update_small_cap_uses_budget_of_one() {
        // max_queued_global=1, shard_count=16: effective shards = 1, so the
        // per-shard budget is 1. The enqueue condition must limit to 1, not 16.
        let update = global_queue_enqueue_update("tbl", "00", 1, 16).unwrap();
        let values = update.expression_attribute_values().unwrap();
        let limit = values
            .get(":limit")
            .and_then(|v| v.as_n().ok())
            .expect(":limit must be present");
        assert_eq!(limit, "1", "small cap must produce per-shard budget of 1");
    }

    // ─── Global counter shard determinism ──────────────────────────────────

    #[test]
    fn global_counter_shard_is_deterministic() {
        let shard_a = global_counter_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 10, 16);
        let shard_b = global_counter_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 10, 16);
        assert_eq!(shard_a, shard_b, "must be deterministic for same inputs");
    }

    #[test]
    fn global_counter_shard_bounded_by_effective_count() {
        // When cap < shard_count, effective = cap, so shards are in [0, cap).
        for cap in [1u32, 3, 5] {
            let shards = 16u32;
            let shard =
                global_counter_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", cap, shards);
            let value: u32 = shard.parse().unwrap();
            assert!(
                value < cap,
                "shard {shard} must be < effective={cap} (cap={cap}, shards={shards})"
            );
        }
        // When cap >= shard_count, effective = shard_count.
        let shard = global_counter_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 100, 16);
        let value: u32 = shard.parse().unwrap();
        assert!(value < 16, "shard {shard} must be < 16");
    }

    #[test]
    fn global_counter_shard_independent_of_queue_gsi_shard() {
        // The global counter shard and the queue GSI shard use different
        // moduli (effective vs shard_count), so they can differ. This test
        // documents that dispatch must NOT reuse the queue_shard for the global
        // counter decrement.
        let gsi_shard = queue_shard_for(BacklogOwnerKind::Caller, "alice", "job-1", 16);
        let global_shard = global_counter_shard_for(
            BacklogOwnerKind::Caller,
            "alice",
            "job-1",
            1,
            16, // effective = 1, so global_shard is always "00"
        );
        // With effective=1, the global counter shard is always "00" regardless
        // of the GSI shard assignment.
        assert_eq!(global_shard, "00");
        // The GSI shard can be anything in [0,16).
        let gsi_val: u32 = gsi_shard.parse().unwrap();
        assert!(gsi_val < 16);
    }

    #[test]
    fn enqueue_and_dispatch_target_same_global_counter_shard() {
        // The core invariant: dispatch must decrement the exact
        // GLOBAL#QUEUE#<shard> that enqueue incremented. Since both recompute
        // from the same identity + config, the PKs must match.
        let kind = BacklogOwnerKind::Caller;
        let owner_id = "alice";
        let job_id = "job-42";
        // Test across various cap/shard combos including small caps.
        for (cap, shards) in [(1u32, 16u32), (3, 16), (5, 16), (100, 16), (20, 8)] {
            let shard = global_counter_shard_for(kind, owner_id, job_id, cap, shards);

            let enqueue = global_queue_enqueue_update("tbl", &shard, cap, shards).unwrap();
            let dispatch = global_queue_dispatch_update("tbl", &shard).unwrap();

            let enqueue_pk = enqueue.key().get("pk").and_then(|v| v.as_s().ok()).unwrap();
            let dispatch_pk = dispatch
                .key()
                .get("pk")
                .and_then(|v| v.as_s().ok())
                .unwrap();

            assert_eq!(
                enqueue_pk, dispatch_pk,
                "enqueue and dispatch must target the same shard (cap={cap}, shards={shards})"
            );
            assert_eq!(enqueue_pk, &format!("GLOBAL#QUEUE#{shard}"));
        }
    }

    #[test]
    fn global_counter_shard_recomputable_after_dispatch_removes_gsi_attrs() {
        // After dispatch, the GSI attributes (queue_shard, queue_sort_key,
        // next_eligible_at) are removed from the job record. But owner_kind,
        // owner_id, and job_id remain. The global counter shard must be
        // recomputable from just those fields + config.
        let kind = BacklogOwnerKind::Caller;
        let owner_id = "alice";
        let job_id = "job-77";
        let cap = 3u32;
        let shards = 16u32;

        // "Enqueue" computation:
        let enqueue_shard = global_counter_shard_for(kind, owner_id, job_id, cap, shards);

        // "Dispatch" recomputation from the surviving record fields:
        let dispatch_shard = global_counter_shard_for(kind, owner_id, job_id, cap, shards);

        assert_eq!(
            enqueue_shard, dispatch_shard,
            "global counter shard must be recomputable from job record + config"
        );
    }

    // ─── Enqueue cancellation classification ──────────────────────────────

    /// Build cancellation reasons matching the enqueue transaction item order:
    /// `[job_put(0), dedupe_put(1), owner_update(2), global_update?(3)]`.
    fn enqueue_reasons_with_failure(
        failing_index: usize,
        has_global: bool,
    ) -> Vec<CancellationReason> {
        enqueue_reasons_with_failure_code(failing_index, has_global, "ConditionalCheckFailed")
    }

    /// Same as [`enqueue_reasons_with_failure`] but lets the caller pick the
    /// cancellation reason code (e.g. `"TransactionConflict"`).
    fn enqueue_reasons_with_failure_code(
        failing_index: usize,
        has_global: bool,
        code: &str,
    ) -> Vec<CancellationReason> {
        let item_count = if has_global { 4 } else { 3 };
        (0..item_count)
            .map(|i| {
                let reason_code = if i == failing_index { code } else { "None" };
                CancellationReason::builder().code(reason_code).build()
            })
            .collect()
    }

    #[test]
    fn enqueue_cancel_at_owner_quota_is_owner_full() {
        let reasons = enqueue_reasons_with_failure(2, false);
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::OwnerFull
        );
    }

    #[test]
    fn enqueue_cancel_at_dedupe_is_duplicate() {
        let reasons = enqueue_reasons_with_failure(1, false);
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::Duplicate
        );
    }

    #[test]
    fn enqueue_cancel_at_global_quota_is_global_full() {
        let reasons = enqueue_reasons_with_failure(3, true);
        assert_eq!(
            classify_enqueue_reasons(&reasons, true),
            EnqueueCancellation::GlobalFull
        );
    }

    #[test]
    fn enqueue_cancel_at_global_index_without_global_item_is_unknown() {
        let reasons = enqueue_reasons_with_failure(3, true);
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::Unknown
        );
    }

    #[test]
    fn enqueue_cancel_at_job_pk_is_unknown() {
        let reasons = enqueue_reasons_with_failure(0, false);
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::Unknown
        );
    }

    #[test]
    fn enqueue_transaction_conflict_at_owner_is_transient_not_owner_full() {
        // TransactionConflict at the owner quota item means concurrent write
        // contention — the owner may have capacity. Must NOT be classified as
        // OwnerFull (which maps to QueueFull).
        let reasons = enqueue_reasons_with_failure_code(2, false, "TransactionConflict");
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::TransientConflict,
            "TransactionConflict at owner must be transient, not QueueFull"
        );
    }

    #[test]
    fn enqueue_transaction_conflict_at_global_is_transient_not_global_full() {
        // TransactionConflict at the global counter item means concurrent write
        // contention — the global queue may have capacity. Must NOT be
        // classified as GlobalFull.
        let reasons = enqueue_reasons_with_failure_code(3, true, "TransactionConflict");
        assert_eq!(
            classify_enqueue_reasons(&reasons, true),
            EnqueueCancellation::TransientConflict,
            "TransactionConflict at global must be transient, not GlobalQueueFull"
        );
    }

    #[test]
    fn enqueue_transaction_conflict_at_dedupe_is_transient() {
        // Even at the dedupe position, TransactionConflict is contention, not
        // a definitive duplicate. Surface as transient so the caller retries.
        let reasons = enqueue_reasons_with_failure_code(1, false, "TransactionConflict");
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::TransientConflict
        );
    }

    #[test]
    fn enqueue_conditional_check_at_owner_still_maps_to_owner_full() {
        // Regression guard: ConditionalCheckFailed must still map correctly.
        let reasons = enqueue_reasons_with_failure_code(2, false, "ConditionalCheckFailed");
        assert_eq!(
            classify_enqueue_reasons(&reasons, false),
            EnqueueCancellation::OwnerFull
        );
    }

    #[test]
    fn enqueue_conditional_check_at_global_still_maps_to_global_full() {
        let reasons = enqueue_reasons_with_failure_code(3, true, "ConditionalCheckFailed");
        assert_eq!(
            classify_enqueue_reasons(&reasons, true),
            EnqueueCancellation::GlobalFull
        );
    }

    // ─── No-reasons transaction-conflict fallback ─────────────────────────
    //
    // A `TransactionInProgressException` or a message-only `TransactionConflict`
    // reaches the conflict branch of `is_transaction_conflict` with no populated
    // item-level cancellation reasons. These are transient write-contention
    // failures where capacity may exist, so they must surface as
    // `TransientConflict` (→ `JobsError::Conflict` for retry) rather than fall
    // through to the owner-cap `QueueFull` default. A message-only
    // `ConditionalCheckFailed` is a capacity-style rejection and must stay on
    // the `QueueFull` default.

    #[test]
    fn enqueue_no_reasons_retryable_conflict_is_transient_not_queue_full() {
        // Core regression: a no-reasons retryable conflict (e.g.
        // TransactionInProgressException) must NOT become a false quota-full
        // rejection.
        assert_eq!(
            classify_enqueue_cancellation_from(None, false, true),
            EnqueueCancellation::TransientConflict,
            "no-reasons retryable conflict must be transient, not QueueFull"
        );
    }

    #[test]
    fn enqueue_no_reasons_retryable_conflict_with_global_item_is_transient() {
        // Same regression with a global counter item in the transaction.
        assert_eq!(
            classify_enqueue_cancellation_from(None, true, true),
            EnqueueCancellation::TransientConflict
        );
    }

    #[test]
    fn enqueue_no_reasons_conditional_check_stays_on_queue_full_fallback() {
        // A message-only ConditionalCheckFailed is a capacity-style rejection.
        // It must stay Unknown so the caller applies the dedupe-recheck /
        // owner-cap QueueFull fallback. It must NOT be upgraded to
        // TransientConflict.
        assert_eq!(
            classify_enqueue_cancellation_from(None, false, false),
            EnqueueCancellation::Unknown,
            "message-only ConditionalCheckFailed must stay on the QueueFull fallback"
        );
    }

    #[test]
    fn enqueue_unresolved_reasons_retryable_conflict_is_transient() {
        // Reasons present but none recognized (all "None") → classified
        // Unknown → upgraded to TransientConflict when the error is retryable.
        let reasons = vec![
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder().code("None").build(),
        ];
        assert_eq!(
            classify_enqueue_cancellation_from(Some(&reasons), false, true),
            EnqueueCancellation::TransientConflict
        );
    }

    #[test]
    fn enqueue_resolved_reasons_take_precedence_over_fallback() {
        // When item-level reasons resolve to a definitive cause, the fallback
        // must not override it — even if the retryable flag is set (the flag is
        // only meaningful when reasons are absent/unresolved).
        let reasons = enqueue_reasons_with_failure_code(2, false, "ConditionalCheckFailed");
        assert_eq!(
            classify_enqueue_cancellation_from(Some(&reasons), false, true),
            EnqueueCancellation::OwnerFull,
            "resolved reasons must take precedence over the no-reasons fallback"
        );
    }

    #[test]
    fn enqueue_resolved_transaction_conflict_reason_is_transient() {
        // An item-level TransactionConflict reason still classifies as
        // TransientConflict (retryable flag is irrelevant here).
        let reasons = enqueue_reasons_with_failure_code(2, false, "TransactionConflict");
        assert_eq!(
            classify_enqueue_cancellation_from(Some(&reasons), false, false),
            EnqueueCancellation::TransientConflict
        );
    }

    // ─── Release cancellation classification ──────────────────────────────

    /// Build cancellation reasons matching the release transaction item order:
    /// `[token_delete(0), quota_update(1)]`.
    fn release_reasons_with_failure(failing_index: usize) -> Vec<CancellationReason> {
        release_reasons_with_failure_code(failing_index, "ConditionalCheckFailed")
    }

    /// Same as [`release_reasons_with_failure`] but lets the caller pick the
    /// cancellation reason code.
    fn release_reasons_with_failure_code(
        failing_index: usize,
        code: &str,
    ) -> Vec<CancellationReason> {
        (0..2)
            .map(|i| {
                let reason_code = if i == failing_index { code } else { "None" };
                CancellationReason::builder().code(reason_code).build()
            })
            .collect()
    }

    #[test]
    fn release_cancel_at_token_delete_is_token_gone() {
        let reasons = release_reasons_with_failure(0);
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::TokenGone
        );
    }

    #[test]
    fn release_cancel_at_quota_update_is_quota_conflict() {
        let reasons = release_reasons_with_failure(1);
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::QuotaConflict
        );
    }

    #[test]
    fn release_cancel_at_global_token_delete_is_quota_conflict() {
        let reasons = vec![
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder()
                .code("ConditionalCheckFailed")
                .build(),
        ];
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::QuotaConflict
        );
    }

    #[test]
    fn release_cancel_with_no_failure_is_unknown() {
        let reasons = vec![
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder().code("None").build(),
        ];
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::Unknown
        );
    }

    #[test]
    fn release_transaction_conflict_at_token_is_transient_not_token_gone() {
        // TransactionConflict at the token delete means concurrent contention
        // — the token likely still exists. Must NOT be classified as TokenGone
        // (which would skip the counter decrement and leak a running slot).
        let reasons = release_reasons_with_failure_code(0, "TransactionConflict");
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::TransientConflict,
            "TransactionConflict at token must be transient, not TokenGone"
        );
    }

    #[test]
    fn release_transaction_conflict_at_quota_is_transient_not_quota_conflict() {
        let reasons = release_reasons_with_failure_code(1, "TransactionConflict");
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::TransientConflict
        );
    }

    #[test]
    fn release_conditional_check_at_token_still_maps_to_token_gone() {
        // Regression guard: ConditionalCheckFailed must still map correctly.
        let reasons = release_reasons_with_failure_code(0, "ConditionalCheckFailed");
        assert_eq!(
            classify_release_reasons(&reasons),
            ReleaseCancellation::TokenGone
        );
    }

    // ─── Global queue dispatch decrement ──────────────────────────────────

    #[test]
    fn global_queue_dispatch_update_decrements_shard_counter() {
        let update = global_queue_dispatch_update("tbl", "07").unwrap();
        let values = update.expression_attribute_values().unwrap();
        // Must include -1 for the ADD decrement.
        assert!(values
            .get(":minus_one")
            .and_then(|v| v.as_n().ok())
            .is_some_and(|v| v == "-1"));
        // Must guard against underflow.
        let condition = update.condition_expression().unwrap();
        assert!(
            condition.contains("queued_count >= :one"),
            "dispatch decrement must guard underflow: {condition}"
        );
    }

    #[test]
    fn global_queue_dispatch_update_targets_correct_shard() {
        let update = global_queue_dispatch_update("tbl", "03").unwrap();
        let key = update.key();
        let pk = key.get("pk").and_then(|v| v.as_s().ok()).unwrap();
        assert_eq!(pk, "GLOBAL#QUEUE#03");
    }

    // ─── Running token carries real shard ─────────────────────────────────

    #[test]
    fn running_token_item_with_real_shard() {
        let owner = BacklogOwner::caller("alice");
        let item = running_token_item("job-9", "now", &owner, "07", Some("GLOBAL#RUNNING_TOKEN#1"));
        assert_eq!(
            item.get("queue_shard").and_then(|v| v.as_s().ok()),
            Some(&"07".to_string()),
            "token must carry the real shard from the job record"
        );
        assert_eq!(
            item.get("global_running_token_pk")
                .and_then(|v| v.as_s().ok()),
            Some(&"GLOBAL#RUNNING_TOKEN#1".to_string())
        );
    }

    #[test]
    fn global_running_token_item_claims_slot_for_job() {
        let item = global_running_token_item(1, "job-9", "now");
        assert_eq!(
            item.get("pk").and_then(|v| v.as_s().ok()),
            Some(&"GLOBAL#RUNNING_TOKEN#1".to_string())
        );
        assert_eq!(
            item.get("job_id").and_then(|v| v.as_s().ok()),
            Some(&"job-9".to_string())
        );
    }

    fn sample_record() -> JobRecord {
        JobRecord {
            job_id: "job-1".to_string(),
            status: JobStatus::Queued,
            source: "git:custom".to_string(),
            package: "serde".to_string(),
            revision: "main".to_string(),
            source_url: "https://example.com".to_string(),
            source_url_hash: "sha256:x".to_string(),
            source_kind: "git".to_string(),
            caller_id: "alice".to_string(),
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "0".to_string(),
            updated_at: "0".to_string(),
            owner_kind: None,
            owner_id: None,
            queue_shard: None,
            queue_sort_key: None,
            next_eligible_at: None,
            dispatched_at: None,
        }
    }
}
