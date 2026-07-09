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
  running/queued jobs when an execution ARN exists.

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
- Anonymous public traffic maps to one shared `Anonymous` owner.
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

Keep one DynamoDB table for index jobs and add queue-specific item shapes:

| Item | Purpose |
|---|---|
| `JOB#<job_id>` | Durable job record with `queued`, `dispatching`, `running`, `complete`, or `failed` status |
| `DEDUPE#<source>#<package>#<revision>#<source_url_hash>` | Pointer to current queued/running job for idempotent admission |
| `OWNER#<kind>#<id>#QUOTA` | Per-owner `queued_count` and `running_count` |
| `GLOBAL#QUOTA` | Service-wide `queued_count` and `running_count` |
| `QUEUE#<shard>#<queued_at>#<job_id>` | Queue index entry for drain scans |

Queue indexes can live as table items or as a GSI over job records. The
implementation should choose the smallest change that gives ordered reads by
`queued_at` and enough sharding to avoid hot partitions.

All counters are updated only by DynamoDB transactions. There must be no
read-then-write quota checks outside a transaction.

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
   - queue index item
   - owner queued counter increment
   - global queued counter increment
8. If owner/global queued cap would be exceeded, reject with `queue_full` or
   `global_queue_full`.
9. Return the queued job response.
10. Optionally invoke a small best-effort dispatch kick for low latency. The
    EventBridge drainer remains the correctness path.

This keeps the client contract simple: an accepted request always has a durable
job ID and can be polled with `external_index_status`.

## Drainer Design

Use an EventBridge-scheduled drainer Lambda as the primary dispatch path.

The drainer runs frequently, for example every 30-60 seconds, and:

1. Acquires a short DynamoDB lease per queue shard to avoid duplicate drainers.
2. Reads queued jobs in FIFO order from one or more shards.
3. For each candidate, attempts a transaction:
   - condition `JOB.status = queued`
   - condition owner/global running counts are below cap
   - decrement owner/global queued counts
   - increment owner/global running counts
   - set `JOB.status = dispatching`
4. Starts Step Functions with `name = job_id`.
5. Records `execution_arn` and sets `status = running`.
6. If Step Functions start fails:
   - deterministic input/config errors mark the job `failed` and release running
     quota.
   - transient service errors requeue the job with a bounded attempt counter and
     release running quota.

`StartExecution` remains idempotent through the job ID execution name. If a
drainer crashes after starting Step Functions but before recording the ARN, a
repair path can use Step Functions execution-name lookup or mark the
`dispatching` job stale and retry safely.

## Completion and Quota Release

Worker terminal updates release running quota:

- `complete` decrements owner/global running counts, removes queue-active
  markers, and can retain the dedupe pointer briefly for status lookups.
- `failed` does the same release and records `error_code` / `error_detail`.
- Stale `dispatching` or `running` jobs are repaired by the drainer and by
  `external_index_status`, extending the current repair behavior.

Quota release must be idempotent. A repeated terminal update must not decrement
running counters twice.

## Fairness

The first version should avoid complicated schedulers while preventing a single
owner from monopolizing all worker slots:

- Per-owner running cap controls local concurrency.
- Global running cap protects the whole service.
- Drainer scans should rotate queue shards between runs.
- If a candidate owner is at running cap, the drainer skips that job and looks
  at later jobs up to a bounded scan limit, so other owners can make progress.

Exact queue position is not required for the first version. A future status
response can add a best-effort `queue_position_hint` if operators need it.

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

Existing over-cap clients should now see `queued` until backlog capacity is
exhausted. Queue-full and global-queue-full rejections are new explicit failure
reasons.

## Testing

TDD per repository convention:

- Job-store unit tests:
  - create queued job increments queued counters.
  - queue-full rejects without partial writes.
  - duplicate request returns existing queued job.
  - dispatch transition atomically moves queued -> dispatching and queued
    counters -> running counters.
  - terminal update releases running counters exactly once.
- MCP tests:
  - 10 unique requests with queue capacity return 10 queued/active job IDs.
  - over owner queue capacity returns `queue_full`.
  - over global queue capacity returns `global_queue_full`.
  - anonymous callers share the anonymous backlog owner.
  - configured owner mode can map through a fake user owner.
- Drainer tests:
  - dispatches up to owner/global running caps.
  - skips owners at cap and dispatches other owners.
  - handles Step Functions start failure by failing or requeueing with quota
    release.
  - stale dispatching repair is idempotent.
- Staging smoke follow-up:
  - submit a burst above running capacity.
  - assert all accepted jobs eventually complete in bounded-backlog mode.
  - assert serving queries work after completion.

## Rollout Plan

1. Add schema-compatible job-store queue primitives behind config defaults that
   preserve current behavior.
2. Add tests for queue admission and dispatch transitions.
3. Add EventBridge drainer Lambda and Terraform wiring.
4. Deploy with queued cap still zero to prove no behavior change.
5. Enable a small queued cap on the default/live stack and run burst smoke.
6. Increase backlog limits deliberately after observing DynamoDB, Step
   Functions, Lambda worker, and DuckLake publish metrics.

