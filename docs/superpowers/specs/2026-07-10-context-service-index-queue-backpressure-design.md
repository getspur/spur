# Spur Context Service — DynamoDB Backlog and Backpressure for `external_index`

- Status: approved design, not yet implemented
- Companion specs:
  - `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md`
  - `docs/superpowers/specs/2026-06-28-spur-context-medallion-design.md`
  - `crates/spur-context-service/docs/ARCHITECTURE.md`

## Problem

The live `external_index` admission path supports concurrent indexing, but the
current DynamoDB control plane uses the per-caller active-job cap as a hard
admission limit. With `SPUR_INDEX_MAX_CONCURRENT_JOBS_PER_CALLER=2`, a caller
that submits 10 unique cold index requests gets two accepted jobs and eight
immediate `concurrent_job_limit` rejections. That protects the service, but it
pushes queueing and retry behavior onto clients.

The desired behavior is bounded server-side queueing:

- Accept a burst of valid index requests while a caller/global backlog has room.
- Return a durable `job_id` for each accepted request.
- Run only a configured number of jobs concurrently.
- Apply explicit backpressure only when the backlog is full.
- Keep the quota model extensible so the current caller-scoped backlog can grow
  into per-user or tenant-user backlog limits later.

## Current Architecture Facts

- `external_index` already validates source URLs, applies a DynamoDB-backed
  per-caller request rate limit, checks for warm catalog hits, and creates a job
  record plus dedupe pointer in DynamoDB before starting Step Functions.
- The current active-job cap is enforced inside
  `create_or_get_active_job_with_limit`: job creation, dedupe creation, active
  marker creation, and caller quota acquisition happen in one DynamoDB
  transaction.
- Step Functions starts work immediately after admission. It invokes the Lambda
  worker first and falls back to ECS for Lambda platform failures/timeouts.
- Workers can overlap, but DuckLake catalog attach/schema/publish mutations are
  serialized by the recent Postgres advisory lock, so the data-plane conflict is
  bounded to the publish critical section.
- `external_index_status` already serves DynamoDB job state and repairs stale
  jobs only when an execution ARN exists. Queued jobs and pre-ARN dispatching
  jobs require drainer-side repair.

## Goals

1. Replace over-cap immediate rejection with a bounded DynamoDB backlog.
2. Preserve idempotency: duplicate requests for the same active queued/running
   coordinate return the existing job.
3. Preserve abuse protection: invalid URLs, request-rate overflow, per-owner
   queue overflow, and global queue overflow still reject immediately.
4. Add a drainer that starts queued jobs when running capacity is available.
5. Model backlog ownership explicitly so the first implementation can use the
   current caller identity while future deployments can scope backlog by user or
   tenant-user identity.

## Non-Goals

- Unlimited queueing. The queue is a bounded backpressure mechanism, not a
  promise to eventually run arbitrary public traffic.
- Priority scheduling beyond FIFO/fairness across owners.
- Private tenant source isolation. This design is still for public external
  packages in the shared context-service catalog.
- Replacing Step Functions or the Lambda/ECS worker split.

## Proposed Behavior

For 10 unique cold requests from one backlog owner with:

- `max_running_jobs_per_owner = 2`
- `max_queued_jobs_per_owner = 20`
- enough global capacity

the service returns 10 accepted jobs:

```json
{
  "job_id": "...",
  "status": "queued",
  "execution_arn": null,
  "revision": "..."
}
```

Two jobs are dispatched to Step Functions as capacity permits; the remaining
eight stay in DynamoDB as `queued`. When a running job reaches `complete` or
`failed`, the drainer starts the next queued job for that owner, subject to both
owner and global running caps.

If the owner backlog is full, the response is explicit:

```json
{
  "status": "rejected",
  "reason": "queue_full",
  "max_queued_jobs_per_owner": 20,
  "retry_after_seconds": 60
}
```

If the whole service backlog is full, the reason is `global_queue_full`. The
existing request-rate limiter remains separate and still protects the API from
high-frequency callers before queue admission.

## Backlog Ownership Model

Introduce a small ownership abstraction used by all quota and queue keys:

```rust
struct BacklogOwner {
    kind: BacklogOwnerKind,
    id: String,
}

enum BacklogOwnerKind {
    Anonymous,
    Caller,
    User,
    TenantUser,
}
```

Initial production behavior:

- Authenticated traffic maps to `BacklogOwnerKind::Caller` using the current
  `caller_id`.
- Anonymous public traffic maps to `Anonymous` only when anonymous mutations are
  enabled. The initial live/default stack may keep this enabled for evaluation;
  secure staging/prod stacks should keep anonymous mutations disabled. If a
  public anonymous stack needs a useful backlog, the owner ID must be derived
  from a stable abuse-control key, such as a signed client token or normalized
  source-IP bucket, not one global anonymous queue.
- Job records continue to store `caller_id` for authorization/status lookup.
- Job records also store `owner_kind` and `owner_id` for backlog accounting.

Future per-user extension:

- Lambda auth can map JWT/user claims to `BacklogOwnerKind::User` or
  `TenantUser` without changing queue/drainer logic.
- DynamoDB keys use `OWNER#<kind>#<id>`, so per-user backlog limits are a config
  and identity-mapping change, not a queue schema rewrite.
- A tenant-user mode can use an ID such as `<tenant_id>#<user_id>` while keeping
  global protection unchanged.

## DynamoDB Control Plane Shape

Keep one DynamoDB table for index jobs, but add one explicit sparse GSI for
drainer reads. The existing table has only `pk` as its primary key, so a
`QUEUE#<shard>#<queued_at>#<job_id>` primary-key item would not be queryable in
FIFO order without a full table scan. The queue must be queryable through a
Terraform-managed GSI before non-zero queue caps are enabled.

### Item Shapes

| Item | Purpose |
|---|---|
| `JOB#<job_id>` | Durable job record with `queued`, `dispatching`, `running`, `complete`, `failed`, or `partial` status |
| `DEDUPE#<source>#<package>#<revision>#<source_url_hash>` | Pointer to current queued/running job for idempotent admission |
| `OWNER#<kind>#<id>#QUOTA` | Per-owner `queued_count` and `running_count` |
| `GLOBAL#QUEUE#<shard>` | Sharded service-wide queued counters for approximate global queued backpressure |
| `RUNNING#<job_id>` | Release token for exactly-once running quota release |
| `GLOBAL#RUNNING_TOKEN#<n>` | Optional hard global running token claimed at dispatch time |
| `CURSOR#<shard>` | Durable CAS-versioned drainer continuation key, stored under cursor-only attributes so it stays out of the sparse queue GSI |

### Sparse Queue GSI

Queued job records carry GSI attributes while they are eligible for dispatch:

| Attribute | Meaning |
|---|---|
| `queue_shard` | Stable shard, e.g. `hash(owner_kind, owner_id, job_id) % index_queue_shard_count` |
| `queue_sort_key` | Lexicographic key `<next_eligible_at>#<queued_at>#<job_id>` |
| `next_eligible_at` | Unix seconds; used for requeue backoff |

Terraform adds a GSI keyed by `(queue_shard, queue_sort_key)`. Only queued jobs
have these attributes, so the GSI is sparse. Dispatch removes the GSI
attributes in the same transaction that changes the job to `dispatching`.

The first implementation should use 16 queue shards unless load testing shows a
need for a different value. The drainer rotates both shard order and last-seen
cursor per shard so jobs near the tail are eventually scanned.

### Global Backpressure

Per-owner caps are hard and transactionally enforced on `OWNER#...#QUOTA`.
Global queued caps use sharded counters (`GLOBAL#QUEUE#<shard>`) to avoid one
write-hot `GLOBAL#QUOTA` item. This makes global queued enforcement
conservative/approximate under heavy contention; operators should set the
global queued cap with slack and rely on the per-owner cap as the hard fairness
boundary.

To prevent a small global cap from inflating total capacity, the number of
counter shards is `min(shard_count, max_queued_global)` (the *effective* shard
count). Each effective shard gets a budget of
`floor(max_queued_global / effective_shards) ≥ 1`. Total service-wide queued
capacity is `effective_shards × per_shard_budget`, which is always
`≤ max_queued_global`. For example, `max_queued_global=1, shard_count=16`
uses 1 effective counter shard with a budget of 1, not 16 shards each allowing
1. The global counter shard for a job is computed deterministically from
`(owner_kind, owner_id, job_id)` modulo the effective count — it is separate
from the queue GSI shard (which uses `shard_count`). Dispatch recomputes the
same shard from the job record + config so it decrements the exact counter
that enqueue incremented.

Global running caps are hard when configured: dispatch claims one
`GLOBAL#RUNNING_TOKEN#<n>` item before starting Step Functions and releases that
token in the same terminal-release transaction as the owner running count. Small
deployments can omit a global running cap and rely on per-owner caps plus API
Gateway throttles; production stacks should configure the token pool.

All counters are updated only by DynamoDB transactions. There must be no
read-then-write quota checks outside a transaction. Quota items may use TTL only
when the TTL is much larger than the maximum job lifetime and is refreshed on
every non-zero update; the reconciler must recreate missing counters from job
state if TTL or operator repair removes an item unexpectedly.

## Admission Flow

`external_index` changes from "create and start" to "create and enqueue":

1. Parse and validate args.
2. Validate and resolve source URL abuse controls.
3. Apply the existing per-owner request-rate limit.
4. Return a warm catalog hit unless `force=true`.
5. Compute `BacklogOwner` from auth context.
6. Try to find an existing queued/running dedupe job. If found, return it.
7. Transactionally create:
   - `JOB#<job_id>` with `status=queued`
   - dedupe pointer
   - queue GSI attributes on the job record
   - owner queued counter increment
   - sharded global queued counter increment, when configured
8. If owner/global queued cap would be exceeded, reject with `queue_full` or
   `global_queue_full`.
9. Return the queued job response.
10. Optionally invoke a small best-effort dispatch kick for low latency. The
    EventBridge drainer remains the correctness path.

The enqueue transaction may be cancelled atomically by DynamoDB. A
`ConditionalCheckFailed` cancellation reason at the dedupe item means a
duplicate; at the owner/global counter item it means the cap is genuinely full
(`queue_full`/`global_queue_full`). A `TransactionConflict` reason is transient
write contention — the cap may have room — and is surfaced as a retryable
conflict instead. This distinction also covers the no-reasons paths
(`TransactionInProgressException`, or a message-only `TransactionConflict`
without populated item reasons): these are surfaced for retry, never reported as
quota-full, so concurrent enqueue contention does not produce false capacity
rejections.

`force=true` bypasses the warm-catalog hit only. It does not bypass request
rate limits, owner/global queued caps, owner/global running caps, URL abuse
checks, or dedupe against an already queued/running job for the same coordinate
and source URL.

This keeps the client contract simple: an accepted request always has a durable
job ID and can be polled with `external_index_status`.

## Drainer Design

Use an EventBridge-scheduled drainer Lambda as the correctness path, plus
best-effort dispatch kicks on admission and terminal worker updates for lower
latency. The kicks may be an async Lambda invoke or EventBridge event; failure
to kick must not affect job completion because the scheduled drainer is the
fallback.

The drainer runs frequently, for example every 30-60 seconds, and:

1. Acquires a DynamoDB lease per queue shard to avoid duplicate drainers.
2. Reads queued jobs from the sparse queue GSI where
   `queue_sort_key <= <now>#...`.
3. For each candidate, attempts a transaction:
   - condition `JOB.status = queued`
   - condition owner running count is below cap
   - condition a global running token is available, when a hard global cap is
     configured
   - decrement owner/global queued counts
   - increment owner/global running counts
   - create `RUNNING#<job_id>` release token with owner and global token/shard
     metadata
   - remove queue GSI attributes from the job
   - set `JOB.status = dispatching`
4. Starts Step Functions with `name = job_id`.
5. Records `execution_arn` and sets `status = running`.
6. If Step Functions start fails:
   - deterministic input/config errors mark the job `failed` and release running
     quota.
   - transient service errors requeue the job with bounded retry metadata and
     release running quota.

### Drainer Leases

Each shard lease has:

- `lease_owner`: drainer invocation ID
- `lease_expires_at`: now plus twice the expected per-shard scan budget
- optional heartbeat renewal for long scans

Concurrent drainers are still safe because dispatch is conditional on
`JOB.status = queued`. A stale lease only delays that shard until expiry; it
must not require manual cleanup.

The drainer first checks whether the hard global running cap is already
saturated. If it is, the drainer exits without scanning queued jobs. When only
some owners are at cap, the drainer skips those candidates and continues until a
bounded scan limit; rotating shard cursors ensures every queued job is examined
within a configured number of drainer runs.

### Requeue Semantics

Transient dispatch failures move the job back to the queue with:

- `attempt += 1`
- exponential backoff, capped, stored as `next_eligible_at`
- a new `queue_sort_key` so the job moves behind eligible work until backoff
  expires
- `running_count--` and `queued_count++` in the same transaction

After `index_dispatch_max_attempts` (default 3), the job is marked `failed`
with `error_code = "dispatch_exhausted"` and running quota is released.

Step Functions execution names deduplicate dispatch, but `StartExecution` is
not treated as a complete idempotency mechanism. If a drainer crashes after
starting Step Functions but before recording the ARN, repair reconstructs the
execution ARN from the state-machine ARN and `job_id`:

`arn:aws:states:<region>:<account>:execution:<state_machine_name>:<job_id>`

The drainer then calls `DescribeExecution` on that ARN. If it exists, the
drainer records it and moves the job to `running` or a terminal state based on
the execution status. If it does not exist, the drainer may retry
`StartExecution`. If retry returns `ExecutionAlreadyExists`, repair falls back
to the reconstructed ARN path. Stale pre-ARN `dispatching` jobs are repaired by
the drainer only; `external_index_status` can repair only jobs that already have
an execution ARN.

## Completion and Quota Release

Worker terminal updates release running quota:

- `complete` decrements owner/global running counts, removes queue-active
  markers, and deletes the active dedupe pointer so a later non-warm re-index
  request can create a fresh job.
- `failed` does the same release and records `error_code` / `error_detail`.
- `partial` is the spot-interruption/checkpoint state. It is terminal for quota
  purposes: it releases running quota exactly once and records enough checkpoint
  metadata for a future explicit resume/retry path. It must not hold a running
  slot indefinitely.
- Stale `running` jobs with an execution ARN can be repaired by
  `external_index_status`. Stale `queued` and pre-ARN `dispatching` jobs are
  repaired by the drainer.

Quota release must be idempotent. A repeated terminal update must not decrement
running counters twice. The terminal transition is a single DynamoDB
transaction:

1. condition `JOB.status IN ("dispatching", "running")`
2. update job to terminal status
3. delete `RUNNING#<job_id>` with `attribute_exists(pk)`
4. decrement owner running count and release any global running token
5. delete active dedupe pointer only if it still points at this job

If the transaction fails because the job is already terminal or the
`RUNNING#<job_id>` token is gone, the release is treated as already completed
and no counters are decremented. A periodic reconciler compares stored counters
against `JOB` and `RUNNING#` items and repairs drift.

## Fairness

The first version should avoid complicated schedulers while preventing a single
owner from monopolizing all worker slots:

- Per-owner running cap controls local concurrency.
- Global running cap protects the whole service.
- Drainer scans rotate queue shards by the configured schedule tick (not raw
  wall-clock seconds), so a whole-minute cadence visits every shard even when
  the schedule interval and shard count share factors.
- Each shard cursor stores the complete DynamoDB `LastEvaluatedKey`
  (`queue_shard`, `queue_sort_key`, and base-table `pk`) plus a CAS version.
  Each invocation queries at most one configured candidate page per shard.
- If a candidate owner is at running cap, the drainer skips that job and looks
  at later jobs up to a bounded scan limit, so other owners can make progress.
- If the hard global running cap is saturated, the drainer exits before scanning
  and relies on the completion kick or next schedule tick.

Exact queue position is not required for the first version, but starvation is
not allowed. Let `A` be the eligible candidates ahead of a job in the persisted
cursor's circular scan order (including the remaining tail and wrapped prefix),
`L` the page limit, `B` the global dispatch budget, and `R = min(L, B)`. When
the target shard is the scheduled starting shard and hard global capacity is
available, at least `R` candidates are examined, so the job is reached within
`ceil((A + 1) / R) + 1` shard starts. Since the schedule makes every shard the
start once per `shard_count` ticks, this is at most
`shard_count * (ceil((A + 1) / R) + 1)` scheduled invocations. The extra start
covers the conservative case where a full final page has a `LastEvaluatedKey`
and an empty follow-up page is needed to prove the tail. Total eligible
per-shard depth bounds `A`; the bound cannot be derived from
`max_queued_jobs_per_owner` alone. Continuous arrivals that sort behind the
target do not increase `A`. A future status response can add a best-effort
`queue_position_hint` if operators need it.

## Configuration

Rename or supplement the existing cap to reflect running capacity rather than
queue admission:

| Variable | Initial meaning |
|---|---|
| `index_max_running_jobs_per_owner` | Concurrent running/dispatching jobs per backlog owner |
| `index_max_queued_jobs_per_owner` | Accepted queued backlog per owner |
| `index_max_running_jobs_global` | Global concurrent running/dispatching jobs |
| `index_max_queued_jobs_global` | Global accepted queued backlog |
| `index_backlog_owner_mode` | `caller` initially; future `user` / `tenant_user` |
| `index_drainer_schedule_rate` | EventBridge schedule, e.g. 1 minute |
| `index_queue_shard_count` | Queue GSI shard count, default 16 |
| `index_dispatch_max_attempts` | Transient dispatch retry cap, default 3 |
| `index_dispatch_backoff_base_seconds` | Requeue backoff base |

The current live behavior maps cleanly to:

- `index_max_running_jobs_per_owner = 2`
- `index_max_queued_jobs_per_owner = 0`

Turning on queueing is then a deploy-time config change, not a semantic break
for sites that still want reject-over-cap behavior.

## API Compatibility

Existing clients that poll `external_index_status` continue to work. New
observable states are:

- `queued` with `execution_arn = null`
- `dispatching` with `execution_arn = null` or present depending on timing
- `running` with `execution_arn`
- `partial` for checkpointed interruption, terminal for quota accounting

Existing over-cap clients should now see `queued` until backlog capacity is
exhausted. Queue-full and global-queue-full rejections are new explicit failure
reasons.

## Observability and Reconciliation

Queueing is not shippable without CloudWatch metrics, alarms, and a reconciler.

Required metrics:

| Metric | Purpose |
|---|---|
| `queue_depth_per_owner` / `queue_depth_global` | Detect backlog growth |
| `dispatch_latency_seconds` | Time from queued to running |
| `drainer_run_duration_seconds` | Drainer health and scan cost |
| `drainer_skipped_owner_at_cap` / `drainer_global_cap_saturated` | Capacity tuning |
| `stuck_dispatching_count` | Crash-recovery failures |
| `quota_counter_drift` | Stored counters disagree with `JOB` / `RUNNING#` state |
| `rejection_count{reason}` | Backpressure and abuse signal |
| `requeue_attempts` / `requeue_exhausted` | Transient-dispatch failure loop detection |

Required alarms:

- `stuck_dispatching_count > 0` for 5 minutes.
- `quota_counter_drift != 0`.
- `dispatch_latency_seconds` above the configured SLO for 3 consecutive periods.
- sustained `global_queue_full` or `queue_full` rejection spikes.

A periodic reconciler recomputes queued/running counts from authoritative job
and running-token items. It repairs counter drift and can fail jobs that exceed
maximum queued/dispatching age. This reconciler is separate from the drainer:
the drainer keeps work moving; the reconciler repairs accounting.

## Testing

TDD per repository convention:

- Job-store unit tests:
  - create queued job increments queued counters.
  - queue-full rejects without partial writes.
  - duplicate request returns existing queued job.
  - queue GSI attributes are present only while jobs are queued and are removed
    on dispatch.
  - dispatch transition atomically moves queued -> dispatching and queued
    counters -> running counters.
  - terminal update releases running counters exactly once under concurrent
    complete/failed races.
  - `partial` releases running quota exactly once.
  - requeue restores queued accounting, moves behind eligible work, and honors
    the attempt cap.
- MCP tests:
  - 10 unique requests with queue capacity return 10 queued/active job IDs.
  - over owner queue capacity returns `queue_full`.
  - over global queue capacity returns `global_queue_full`.
  - anonymous callers share the anonymous backlog owner.
  - configured owner mode can map through a fake user owner.
- Drainer tests:
  - dispatches up to owner/global running caps.
  - skips owners at cap and dispatches other owners.
  - exits without scanning when the hard global running cap is saturated.
  - handles Step Functions start failure by failing or requeueing with quota
    release.
  - stale dispatching repair is idempotent.
  - stale pre-ARN dispatching repair reconstructs/describes execution ARN and
    does not double-dispatch.
  - lease expiry allows another drainer to continue the shard.
- Reconciler tests:
  - repairs stored queued/running counter drift from `JOB` / `RUNNING#` state.
  - fails or requeues jobs beyond maximum stale queued/dispatching age.
- Staging smoke follow-up:
  - submit a burst above running capacity.
  - assert all accepted jobs eventually complete in bounded-backlog mode.
  - assert serving queries work after completion.

## Rollout Plan

1. Add Terraform for the sparse queue GSI and wait for GSI backfill before any
   queue-enabled code path is deployed.
2. Add schema-compatible job-store queue primitives behind config defaults that
   preserve current behavior.
3. Add tests for queue admission, dispatch transitions, terminal idempotency,
   requeue, and reconciler repair.
4. Add EventBridge drainer Lambda, optional completion/admission dispatch kick,
   reconciler, metrics, and alarms.
5. Deploy with queued cap still zero. Either drain all in-flight jobs before the
   cutover or keep a compatibility release path that understands both old
   `ACTIVE_JOB#` / `CALLER_QUOTA#` items and new `RUNNING#` /
   `OWNER#...#QUOTA` items.
6. Enable a small queued cap on the default/live stack and run burst smoke.
7. Increase backlog limits deliberately after observing DynamoDB, Step
   Functions, Lambda worker, DuckLake publish-lock wait time, queue depth, and
   dispatch-latency metrics.
