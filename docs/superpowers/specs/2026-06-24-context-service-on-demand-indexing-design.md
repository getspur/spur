# Spur Context Service — On-Demand Indexing (v2)

**Date:** 2026-06-24
**Status:** Approved (brainstormed; pending implementation plan)
**Scope:** Agent-triggered indexing pipeline for `crates/spur-context-service`. Adds a Step Functions + Fargate Spot build layer and two new MCP tools (`external_index`, `external_index_status`) to the existing Lambda-served query surface.
**Builds on:** `2026-06-22-code-context-service-design.md` (the serve layer + DuckLake catalog model).

## Problem

The v1 context service design (2026-06-22) shipped a Lambda-served MCP query layer for external packages but explicitly listed on-demand indexing as **Out of Scope (v1)**:

> Agent requests a package that isn't pre-indexed. v2 feature — requires synchronous build trigger + wait.

Today `crates/spur-context-service` contains the serve path (`lambda.rs`, `mcp.rs`, `catalog.rs`, `query.rs`, `knowledge.rs`) and a manual operator-only translator (`src/bin/index.rs`). There is no agent-facing surface to request indexing, no source fetcher, no `spur-cli graph build` orchestration, and no build queue. An agent that queries `external_code_search({package: "serde", revision: "1.0.197"})` against an unindexed revision receives `not_found` and has no recourse.

This spec covers the v2 on-demand ingest path.

## Goal

An agent can request indexing of any fetchable source — `(package, revision, source_url)` — and poll until the resulting DuckLake revision is queryable by the existing five external MCP tools. The same build pipeline is the seam that future background pollers (crates.io / git) will feed.

**Invariants:**

1. Every `external_index` call returns in under 1 second with either `{status: "complete"}` (warm path), `{status: "queued", job_id}` (cold path), or `{status: "rejected", reason}` (abuse / rate limit). Never blocks on the build.
2. The agent discovers build completion by polling `external_index_status({job_id})`. No MCP subscription/notification transport is required.
3. Spot interruption is transparent to the agent — the job status remains `running` across retries.
4. Identical concurrent requests collapse to a single build and a single DuckLake snapshot.
5. The state machine contract is identical for agent-triggered and future poller-triggered jobs.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AGENT (MCP CLIENT)                                                      │
│                                                                          │
│  external_index({package, revision, source_url, source_kind?})           │
│    → { job_id, status: "queued" }  (<1s)                                 │
│                                                                          │
│  ...agent does other work...                                             │
│                                                                          │
│  external_index_status({job_id})                                         │
│    → { status: "complete", revision, snapshot_id } (or "running")        │
│                                                                          │
│  external_code_search({package, revision, ...})   ← existing serve path  │
└─────────────────────────────────────────────────────────────────────────┘
        │                                       │
        │ /index, /index_status                 │ /query (unchanged)
        ▼                                       ▼
┌──────────────────────────┐         ┌─────────────────────────────────┐
│  SERVE LAMBDA            │         │  SERVE LAMBDA (existing)        │
│  (existing function)     │         │  catalog.rs / mcp.rs / etc.     │
│                          │         │  DuckLake ATTACH + SELECT       │
│  + new routes:           │         └─────────────────────────────────┘
│    /index   ──┐           │
│    /index_   │           │
│     status   │           │
│               ▼          │
│  abuse-pre-check ───────▶│  URL allow/deny list, size cap, rate-limit
│  dedup check   ────────▶ │  hit catalog.package_catalog first;
│                          │    return complete immediately if present
│  StartExecution ─┐       │
└──────────────────┼───────┘
                   ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  STEP FUNCTIONS STATE MACHINE (index_build_v1)                          │
│                                                                          │
│  One execution per UUID job_id (from index_jobs row).                   │
│  Concurrent identical requests dedup via UNIQUE constraint              │
│  on (source, package, revision, source_url_hash).                       │
│                                                                          │
│  ┌────────────┐    ┌──────────────────────┐    ┌──────────────┐         │
│  │ RecordJob  │───▶│ RunBuild             │───▶│ CommitCatalog│         │
│  │ (Lambda)   │    │  (ecs:runTask.sync)  │    │ (Lambda)     │         │
│  │ write      │    │  capacity strategy:  │    │ UPDATE       │         │
│  │ index_jobs │    │   FARGATE_SPOT w=4   │    │ package_     │         │
│  │ row        │    │   FARGATE  base=1,w=1│    │ catalog;     │         │
│  │            │    │                      │    │ refs          │         │
│  │            │    │  Catch:              │    └──────┬───────┘         │
│  │            │    │   Retry 2x on spot   │           ▼                  │
│  │            │    │   → FallbackBuild    │    ┌──────────────┐         │
│  │            │    │     (FARGATE only)   │───▶│ MarkComplete │         │
│  └────────────┘    └──────────────────────┘    │ (Lambda)     │         │
│                                                 └──────────────┘         │
└─────────────────────────────────────────────────────────────────────────┘
                   │
                   ▼ kicks off per-job
┌─────────────────────────────────────────────────────────────────────────┐
│  FARGATE TASK (worker container, spur-context-worker binary)            │
│                                                                          │
│  1. Validate job inputs                                                  │
│  2. Fetch source (git clone --filter=blob:none OR tarball dl + extract) │
│       - size cap, deny-link-local, deny-AWS-metadata                     │
│  3. spur_cli::commands::graph::build(GraphBuildOptions {                 │
│       root: <fetched-source>,                                            │
│       artifact_dir: /tmp/artifact,                                       │
│       embed_model: JinaEmbeddingsV2BaseCode (fallback BGE),              │
│     })                                                                   │
│  4. spur_context_service::translate::translate_artifact_to_ducklake(...) │
│       → writes Parquet to S3, DuckLake snapshot, returns TranslateStats  │
│  5. SendTaskSuccess(stats)        ← SF callback (runTask.sync uses this) │
│     On SIGTERM (spot 2-min warning): checkpoint + SendTaskFailure        │
└─────────────────────────────────────────────────────────────────────────┘
```

### Key flows

- **Cold path (new index):** agent → `/index` → SF execution → Fargate worker → DuckLake commit → agent polls → agent queries.
- **Warm path (already indexed):** agent → `/index` → serve Lambda queries `package_catalog` first → returns `{status: "complete", ...}` without ever starting an SF execution. No job created, no cost.
- **Dedup (concurrent identical):** two simultaneous `external_index` calls for the same `(source, package, revision, source_url)` race to INSERT into `index_jobs`; the UNIQUE constraint rejects the loser, which re-SELECTs and returns the winner's `job_id`. Exactly one SF execution runs.
- **Spot interruption:** worker receives SIGTERM + 2-min window → writes checkpoint to S3 + `SendTaskFailure` → SF `Catch` retries up to 2x on spot, then routes to `FallbackBuild` (FARGATE on-demand only).
- **Future pollers** (the "shared queue" property): crates.io / git pollers simply call `StartExecution` against the same state machine — zero new infra.

## Why Step Functions + Fargate Spot

| | **SF + Fargate Spot (chosen)** | **SQS → Fargate** | **Spot Fleet + SQS** |
|---|---|---|---|
| Status API | Free (`DescribeExecution`) | Build your own table+endpoint | Build your own |
| Retry / compensation | Declarative (`Catch` / `Retry`) | Hand-roll in consumer | Hand-roll + checkpoint |
| Spot fallback to on-demand | One extra state | Logic in consumer | ASG strategy |
| Vendor lock-in | Higher (SF ASL) | Lower | Lowest |
| Cost at our scale | ~$0.00015/job orchestration | ~$0 (SQS free tier) | ~$0 + EC2 spot |
| Operational surface | SF + ECS cluster + task def | SQS + ECS cluster + task def | SQS + ASG + launch template + ECS |
| Background-poller compatibility | Poller just calls `StartExecution` | Poller enqueues to same SQS | Already native |

SF was selected because the state-transition cost (~$0.025 per 1,000 transitions after the 4,000/mo free tier) is negligible for a 6-state workflow, while the built-in `DescribeExecution` API gives a free status surface for the agent's poll and the declarative `Catch`/`Retry` handles the gnarly spot-fallback path without bespoke consumer logic.

### Verified AWS primitives

| Claim | Source |
|---|---|
| `arn:aws:states:::ecs:runTask.sync` is a first-class optimized SF integration that waits for task completion | [connect-ecs](https://docs.aws.amazon.com/step-functions/latest/dg/connect-ecs.html) |
| `CapacityProviderStrategy` is a plain `RunTask` parameter; SF forwards it. Use `[FARGATE_SPOT, FARGATE]` with weights + base | [RunTask API](https://docs.aws.amazon.com/AmazonECS/latest/APIReference/API_RunTask.html) |
| Fargate Spot: 2-min SIGTERM warning, EventBridge event with `stopCode: "SpotInterruption"` | [fargate-capacity-providers](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/fargate-capacity-providers.html) |
| Standard Workflow max execution duration: 1 year; Express: 5 min (out for our 2-5 min builds) | [quotas](https://docs.aws.amazon.com/step-functions/latest/dg/limits-overview.html) |
| Pricing: $0.025 per 1,000 state transitions, 4,000/mo free tier | [pricing](https://aws.amazon.com/step-functions/pricing/) |
| `DescribeExecution` returns `RUNNING` / `SUCCEEDED` / `FAILED` + error + I/O — the execution ARN IS the job_id | quotas doc, SF API |
| `Catch` / `Retry` provide declarative error handling for `States.TaskFailed` from spot interruption with on-demand fallback state | [error handling](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html) |

## Components

```
crates/spur-context-service/
  Cargo.toml                    + new feature "worker"
                                  + dep on spur-cli (graph build)
  src/
    lib.rs                      + pub mod abuse; pub mod jobs; pub mod worker
    mcp.rs                      + external_index, external_index_status handlers
                                  + tool definitions
    catalog.rs                  (unchanged — already does DuckLake connect/resolve)
    translate.rs                (unchanged — already public, worker calls it)
    lambda.rs                   + new routes: /index, /index_status
                                  + abuse pre-check + dedup check before StartExecution
    abuse.rs     (NEW)          URL allow/deny list, link-local & metadata
                                  block, size cap, rate-limit-per-caller
    jobs.rs      (NEW)          index_jobs table CRUD; status enum:
                                    queued | running | complete | failed | partial
    worker.rs    (NEW)          FetchSource + spur_cli::graph::build +
                                  translate_artifact_to_ducklake + SendTaskSuccess
  src/bin/
    index.rs                    (existing, unchanged — manual operator CLI)
    worker.rs   (NEW)           thin main() that calls worker::run_job()
                                feature-gated behind "worker"

sql/
  init_catalog.sql              + index_jobs table DDL
  + index_jobs.sql   (NEW)      CREATE TABLE index_jobs (
                                    job_id TEXT PRIMARY KEY,           -- = SF exec ARN suffix
                                    source TEXT, package TEXT,
                                    revision TEXT, source_url TEXT,
                                    source_url_hash TEXT,              -- for dedup
                                    status TEXT,                       -- queued|running|complete|failed|partial
                                    execution_arn TEXT,                -- SF execution ARN
                                    error TEXT, snapshot_id BIGINT,
                                    row_counts JSON,
                                    created_at TIMESTAMPTZ,
                                    updated_at TIMESTAMPTZ,
                                    UNIQUE(source, package, revision, source_url_hash)
                                  );

infra/spur-context-service/     + state machine (ASL JSON)
                                  + ECS cluster + Fargate SPOT/on-demand
                                    capacity providers
                                  + worker task definition (ECR image)
                                  + new Lambda routes /index, /index_status
                                  + IAM: Lambda → SF:StartExecution,
                                    SF → ECS:RunTask, ECS → S3/RDS/SF:SendTask*
```

### Component decisions

1. **Worker lives in `spur-context-service` as a second `[[bin]]`**, gated by a `worker` feature. Shares `translate.rs` and `catalog.rs` with the serve path; avoids a new crate that just re-exports them. The feature gate keeps the heavy `spur-cli` dep (tree-sitter, embedding runtime) out of the Lambda image.

2. **`index_jobs` lives in the existing catalog PostgreSQL** (RDS), not DynamoDB. One DB to operate; we already pay for it; query volume is low (one row per build, not per query); and `package_catalog` UPDATEs in the same transaction as `index_jobs` gives atomic status transitions.

3. **`external_index` and `external_index_status` are added to the same `mcp.rs`** as the five existing query tools — same Lambda, same `tool_definitions()`, same catalog connection. The agent sees one MCP server with seven tools, not two.

4. **Abuse prevention is a pure module** (`abuse.rs`) called by the Lambda handler before `StartExecution`. v2 MVP rule set: deny link-local (`169.254.0.0/16`), deny AWS metadata (`fd00:ec2::`), deny localhost, enforce size cap (default 500 MB tarball / 2 GB git clone depth-bounded), rate-limit per caller identity (API Gateway authorizer principal). Allow-list is a config table the operator can extend.

5. **Container image** is published to ECR; both `spur-cli` and `spur-context-service` baked in. Heavy (~hundreds of MB with tree-sitter + Jina runtime), built once per release, tagged by git SHA. ECS pulls on task start.

## State Machine Data Flow

```json
{
  "StartAt": "RecordJob",
  "States": {
    "RecordJob": {
      "Type": "Task",
      "Resource": "arn:aws:states:::lambda:invoke",
      "Parameters": { "FunctionName": "spur-context-jobs", "Payload.$": "$" },
      "ResultPath": "$.recordResult",
      "Next": "RunBuild"
    },
    "RunBuild": {
      "Type": "Task",
      "Resource": "arn:aws:states:::ecs:runTask.sync",
      "Parameters": {
        "Cluster": "${cluster_arn}",
        "TaskDefinition": "${worker_taskdef_arn}",
        "LaunchType": "FARGATE",
        "CapacityProviderStrategy": [
          { "CapacityProvider": "FARGATE_SPOT", "Weight": 4 },
          { "CapacityProvider": "FARGATE", "Base": 1, "Weight": 1 }
        ],
        "NetworkConfiguration": { "AwsvpcConfiguration": "${net_config}" },
        "Overrides": {
          "ContainerOverrides": [{
            "Name": "worker",
            "Environment": [
              { "Name": "TASK_TOKEN", "Value.$": "$$.Task.Token" },
              { "Name": "JOB_ID",     "Value.$": "$.job_id" },
              { "Name": "PACKAGE",    "Value.$": "$.package" },
              { "Name": "REVISION",   "Value.$": "$.revision" },
              { "Name": "SOURCE",     "Value.$": "$.source" },
              { "Name": "SOURCE_URL", "Value.$": "$.source_url" },
              { "Name": "SOURCE_KIND","Value.$": "$.source_kind" }
            ]
          }]
        }
      },
      "ResultPath": "$.buildStats",
      "Retry": [
        { "ErrorEquals": ["States.TaskFailed"],
          "MaxAttempts": 2, "BackoffRate": 2.0,
          "MaxDelaySeconds": 30 }
      ],
      "Catch": [
        { "ErrorEquals": ["States.ALL"],
          "Next": "FallbackBuild", "ResultPath": "$.buildError" }
      ],
      "Next": "CommitCatalog"
    },
    "FallbackBuild": {
      "Type": "Task",
      "Resource": "arn:aws:states:::ecs:runTask.sync",
      "Parameters": {
        "Cluster": "${cluster_arn}",
        "TaskDefinition": "${worker_taskdef_arn}",
        "LaunchType": "FARGATE",
        "CapacityProviderStrategy": [
          { "CapacityProvider": "FARGATE", "Weight": 1 }
        ],
        "NetworkConfiguration": { "AwsvpcConfiguration": "${net_config}" },
        "Overrides": { "ContainerOverrides": [{
          "Name": "worker",
          "Environment.$": "$.recordResult.env"
        }]}
      },
      "ResultPath": "$.buildStats",
      "Next": "CommitCatalog"
    },
    "CommitCatalog": {
      "Type": "Task",
      "Resource": "arn:aws:states:::lambda:invoke",
      "Parameters": {
        "FunctionName": "spur-context-commit",
        "Payload": {
          "job_id.$": "$.job_id",
          "buildStats.$": "$.buildStats",
          "source.$": "$.source",
          "package.$": "$.package",
          "revision.$": "$.revision"
        }
      },
      "ResultPath": "$.commitResult",
      "Next": "MarkComplete"
    },
    "MarkComplete": {
      "Type": "Task",
      "Resource": "arn:aws:states:::lambda:invoke",
      "Parameters": {
        "FunctionName": "spur-context-jobs",
        "Payload": {
          "action": "mark_complete",
          "job_id.$": "$.job_id",
          "snapshot_id.$": "$.commitResult.snapshot_id"
        }
      },
      "End": true
    }
  }
}
```

### Per-state responsibility

| State | Runs in | Does | Failure mode |
|---|---|---|---|
| `RecordJob` | Lambda (`spur-context-jobs`) | UPDATE `index_jobs` (status=`running`, execution_arn). The row was already INSERTed with status=`queued` by the serve-Lambda routing (which needed the row for UNIQUE-constraint dedup before `StartExecution`). If a future poller calls `StartExecution` directly without the routing, RecordJob UPSERTs the row instead. | Lambda retries (SF default ×3) |
| `RunBuild` | Fargate worker | Fetch source → `spur_cli::graph::build` → `translate_artifact_to_ducklake` → `SendTaskSuccess(TranslateStats)`. DuckLake snapshot committed here. | Spot SIGTERM → checkpoint to `s3://spur-context/jobs/<job_id>/` + `SendTaskFailure` |
| `Retry` (on `RunBuild`) | SF runtime | Re-runs `RunBuild`. Idempotent — DuckLake detects duplicate revision INSERTs, second run is mostly a no-op except for re-fetch. | Exhausts → `FallbackBuild` |
| `FallbackBuild` | Fargate (on-demand) | Same as `RunBuild`, FARGATE capacity only. Guaranteed capacity after spot flakiness. | Hard failure → SM fails → `index_jobs.status = failed` via Catch on `States.ALL` at SM top level |
| `CommitCatalog` | Lambda (`spur-context-commit`) | UPDATE `package_catalog` (status=complete, snapshot_id, row_counts) + UPSERT `refs` (`latest`→revision if newest semver). Atomic in one PG transaction. | Lambda retries; if persistently failing, DuckLake snapshot already exists → operator can manually backfill `package_catalog` |
| `MarkComplete` | Lambda (`spur-context-jobs`) | UPDATE `index_jobs` (status=complete). Idempotent — if SM redrive runs this twice, second is a no-op. | Lambda retries |

### Execution name + dedup

Each `external_index` call generates a fresh UUID `job_id`. The SF execution name is set to this `job_id` for traceability (SF execution ARN embeds the name) — **not** as the dedup mechanism.

Dedup is application-layer, via the `index_jobs` UNIQUE constraint on `(source, package, revision_as_given, source_url_hash)` plus race handling in the routing logic. SF execution names cannot be reused within 90 days (even after the prior execution terminates), so a name-based dedup strategy would block retry-after-failure; UUID-per-call avoids that entirely.

### Top-level Catch

Each state has its own catch routing to a `MarkFailed` terminal Lambda that sets `index_jobs.status = failed` with the error message, so the agent's `external_index_status` poll always gets a definitive answer.

## MCP Tool Surface

```
external_index(
  package:     string,   required  — e.g., "serde", "tokio", or any identifier
  revision:    string,   required  — semver "1.0.197" | git SHA | tag | branch
  source_url:  string,   required  — fetchable URL (see source_kind)
  source_kind: string,   optional  — "git" | "tarball", default inferred from URL:
                                       .git suffix or git+ssh → "git"
                                       http(s) to .tar.gz/.tgz/.zip → "tarball"
  source:      string,   optional  — catalog source label, default "git:custom"
                                       (anything other than source_url is informational;
                                        used to namespace the revision in package_catalog)
  force:       bool,     optional  — bypass warm-path catalog check, default false
) → {
  job_id:       string,             // also the SF execution name suffix
  status:       "queued" | "complete" | "rejected",
  execution_arn: string,            // returned for transparency; agent ignores
  revision:     string,             // resolved revision (echoed)
  snapshot_id?: number,             // present only when status="complete" (warm path)
  reason?:      string,             // present only when status="rejected"
  retry_after_seconds?: number      // present only on rate_limit rejection
}
```

```
external_index_status(
  job_id: string, required        — value returned by external_index
) → {
  job_id:     string,
  status:     "queued" | "running" | "complete" | "failed" | "partial" | "not_found",
  revision?:  string,
  snapshot_id?: number,
  embeddings_status?: "complete" | "pending" | "skipped",   // when status=complete|partial
  row_counts?: { nodes, edges, section_bodies, symbol_embeddings, ... },
  error?: {                                                  // when status=failed
    code: "fetch" | "build" | "commit" | "spot_interrupted" | "rate_limit" | "abuse" | "timeout",
    detail: string,
    retriable: bool
  },
  created_at:  string,
  updated_at:  string
}
```

### Source URL contract

| `source_kind` | Accepted `source_url` shapes | Fetch behavior |
|---|---|---|
| `git` (inferred from `.git` suffix, `git+https://`, `git+ssh://`, or github.com URL) | `https://github.com/tokio-rs/tokio`, `https://github.com/tokio-rs/tokio.git`, `git+ssh://git@github.com/tokio-rs/tokio` | `git clone --filter=blob:none <url> && git checkout <revision>`. Revision may be branch, tag, or SHA. |
| `tarball` (inferred from `.tar.gz`, `.tgz`, `.zip` suffix on http(s) URL) | `https://crates.io/api/v1/crates/serde/1.0.197/download`, `https://example.com/pkg.tar.gz` | HTTP GET with size cap → extract to temp dir. **Revision is informational only** — the tarball is treated as the truth. |

### Abuse rules applied to `source_url`

Applied in `abuse.rs` before `StartExecution`:

- DNS resolution of the hostname must not land in: `127.0.0.0/8`, `169.254.0.0/16` (link-local + AWS metadata v1), `fd00:ec2::/16` (AWS metadata v6), `10.0.0.0/8` / `172.16.0.0/12` / `192.168.0.0/16` (RFC1918) unless explicitly allow-listed.
- Scheme must be `https`, `git+https`, or `git+ssh`.
- Per-fetch size cap: 500 MB for tarball, 2 GB for git clone (depth + blob filter keep actual transfer small).
- Operator-maintained allow-list of domains. **Semantics:** an empty allow-list means "no restriction, all public internet allowed"; a populated allow-list means "only these domains are fetchable." Default is empty. Operator can lock down to `github.com`, `crates.io`, etc.
- Per-caller rate limit: 10 `external_index` calls/min default (configurable).

### Revision resolution edge case (git)

The agent may pass a branch name, tag, or SHA. Worker runs `git rev-parse <revision>` after clone to get the SHA, and stores **both** in `index_jobs` and `package_catalog` (revision column = SHA, ref_name = the original). This matches the catalog schema from the original spec (`refs` table holds symbolic → SHA mapping).

### Routing decision in serve Lambda

The `/index` handler performs these steps before any SF call:

```
1. abuse::validate(source_url)          → on fail: return rejected
2. rate_limit::check(caller)            → on fail: return rejected with retry_after
3. resolved_revision = git_rev_parse_if_git(source_url, revision)
                                       // git_https URLs and Lambdas cannot run git.
                                       // For git sources this returns None and
                                       // the routing below uses revision_as_given.
4. catalog::lookup(source, package, resolved_revision.unwrap_or(revision_as_given))
   → if row exists with index_status='complete' && !force:
       return {status: "complete", snapshot_id, revision}
5. jobs::existing(source, package, revision_as_given, source_url_hash)
   → if row exists in queued|running:
       return {job_id: existing, status: <current>, execution_arn: existing}
   → if row exists in failed|partial:
       UPSERT will replace this row in step 6; proceed
6. jobs::insert(job_id = new UUID, status = queued, ...)
   → on UNIQUE violation (concurrent identical request won the race):
       re-SELECT, return {job_id: existing, status: <current>, execution_arn: existing}
7. StartExecution(index_build_v1, name = job_id, payload)
8. return {job_id, status: "queued", execution_arn}
```

Note step 3 — for git sources, the serve Lambda **cannot** resolve the SHA (would require a git operation in the Lambda, which we don't want). When unresolved, the dedup uses `(source, package, revision_as_given, source_url_hash)` — if two agents request the same branch name while a job is active, they share an execution even if the branch moves between requests. The worker resolves the SHA after clone and the catalog stores the canonical mapping.

### Retry-after-failure

The routing step 5 does not short-circuit on `failed` or `partial` rows, so a retry falls through to step 6. The `jobs::insert` UPSERTs over the prior failed/partial row (UNIQUE constraint on `(source, package, revision, source_url_hash)`), generating a fresh `job_id` for the new attempt. This is why the SF execution name is a per-call UUID rather than a deterministic hash — SF rejects name reuse within 90 days even for terminal executions, so a fresh UUID per attempt sidesteps that entirely.

For active-job dedup, the routing step 5 short-circuits before `StartExecution`, and the UNIQUE-constraint race handling in step 6 covers the SELECT-then-INSERT window. Two callers hitting an in-flight job will see exactly one SF execution.

### Tool definitions in `mcp.rs`

Added to the existing `tool_definitions()` vec alongside `external_code_search`, `external_code_read`, `external_code_callers`, `external_code_callees`, `external_knowledge_context`. Same Lambda serves all seven tools.

## Error Handling

| Failure | Detected by | Recovery | Agent-visible result |
|---|---|---|---|
| **Already indexed** (warm path) | Serve Lambda queries `package_catalog` before `StartExecution` | None — return immediately | `{status: "complete", revision, snapshot_id}` |
| **Concurrent identical request** | `jobs::insert` UNIQUE constraint on `(source, package, revision, source_url_hash)` rejects the second INSERT; loser re-SELECTs and returns the in-flight `job_id` | None — both callers share one execution | Both agents get same `job_id`; both poll to completion |
| **Source URL abuse** (link-local, AWS metadata, localhost, deny-listed, size cap exceeded) | `abuse.rs` in serve Lambda before `StartExecution` | Reject; never start SM | `{status: "rejected", reason: "source_url: <detail>"}` |
| **Rate limit exceeded** (per-caller) | Serve Lambda (token bucket in PG or API Gateway usage plan) | Reject with retry-after | `{status: "rejected", reason: "rate_limit", retry_after_seconds: N}` |
| **Source fetch failure** (git clone timeout, tarball 404, auth required, size cap during fetch) | Worker `FetchSource` step | `SendTaskFailure(reason="fetch:<detail>")` → SF `Catch` → `MarkFailed` terminal | `{status: "failed", error: "fetch:<detail>", retriable: true}` — agent may retry with corrected URL |
| **`spur-cli graph build` failure** (tree-sitter fatal, OOM, embedding model load fail) | Worker after `graph::build` returns Err | Distinguish two paths (below) | See below |
| ↳ **Embedding model load fail only** (structural extraction succeeded) | Worker catches the specific error variant from `graph::build` | Continue: translate runs, writes structural tables; `package_catalog.embeddings_status = 'pending'`; row_counts include zero embeddings | `{status: "complete", embeddings_status: "pending", note: "structural queries available; vector search degraded"}` |
| ↳ **Hard build failure** (tree-sitter panic, OOM, all models fail) | Worker `SendTaskFailure(reason="build:<detail>")` | SF `Catch` → `FallbackBuild` (same taskdef, FARGATE on-demand capacity only — taskdef is already sized at Fargate's upper end, 4 vCPU / 30 GB, to fit tokio-sized crates in either capacity) → if still failing, `MarkFailed` | `{status: "failed", error: "build:<detail>", retriable: false}` |
| **Spot interruption** | Worker receives SIGTERM + 2-min window (Fargate Spot termination notice) | Worker: (1) flush in-flight DuckLake writes, (2) write checkpoint to `s3://spur-context/jobs/<job_id>/checkpoint.json` with `last_completed_stage`, (3) `SendTaskFailure(reason="spot_interrupted")`. SF `Retry` ×2 on `States.TaskFailed`. v2 MVP: retry reruns from scratch (DuckLake dedupes revision INSERT). Checkpoint is observability-only — not used for resume in v2. | Transparent — agent sees `running` throughout. If retries exhaust and fallback also fails → `failed` |
| **DuckLake commit conflict** (concurrent workers committing different revisions of same package) | DuckLake snapshot serialization — second commit waits, does not corrupt | DuckLake handles internally (PostgreSQL catalog row lock) | None — both eventually `complete` |
| **DuckLake commit hard failure** (S3 write fail, catalog DB unreachable) | `translate_artifact_to_ducklake` returns Err | Worker `SendTaskFailure` → SF retries; if persistent → `MarkFailed`. No catalog corruption (snapshot is atomic) | `{status: "failed", error: "commit:<detail>", retriable: true}` |
| **Orchestration Lambda failure** (RecordJob / CommitCatalog / MarkComplete) | SF Lambda task fails | SF default retry ×3 with backoff. If `CommitCatalog` persistently fails, the DuckLake snapshot already exists → operator runbook to manually backfill `package_catalog` from `index_jobs.buildStats` | Agent sees `running` until SM terminal; on terminal-fail, sees `failed` with `error: "commit:<detail>"` |
| **Worker exceeds task timeout** (configurable, default 15 min — covers tokio-sized crates on slow spot capacity) | ECS task timeout | Task stopped → SF sees `States.Timeout` → routed through same `Catch` → `FallbackBuild` | Transparent or `failed` if fallback also times out |
| **Agent polls unknown `job_id`** | Serve Lambda `index_jobs` lookup returns empty | None | `{status: "not_found"}` with HTTP 200 (not an error) |

### Spot interruption sequence (detailed)

```
T+0    worker running RunBuild stage (e.g., mid translate)
T+0    ECS sends SIGTERM, sets stopTimeout=120s on container
T+0    worker SIGTERM handler triggers:
         1. cancel in-flight DuckLake transactions (don't commit partial snapshot)
         2. write s3://spur-context/jobs/<job_id>/checkpoint.json:
              { job_id, stage, fetched_source_bytes, build_completed, translate_partial }
         3. SendTaskFailure(task_token, error="spot_interrupted")
T+≤2m  ECS forcefully stops container if not exited
       SF sees task fail → Retry[States.TaskFailed] attempt #2
T+≤2m  New Fargate task starts (likely spot again), reruns from RecordJob output
       (idempotent: DuckLake sees same revision INSERT, dedupes)
```

### Why no resume-from-checkpoint in v2

DuckLake's revision-keyed dedup means a full rerun is correct and only marginally more expensive than resume (re-fetch is the expensive part and git clone is fast with `--filter=blob:none` + cached pack files). Resume logic adds worker state-machine complexity for ~30s savings. v3 can add resume if spot-retry frequency justifies it.

### Error envelope shape

Uniform across all `external_index` and `external_index_status` failure responses:

```json
{
  "job_id": "...",
  "status": "failed",
  "error": {
    "code": "fetch" | "build" | "commit" | "spot_interrupted" | "rate_limit" | "abuse" | "timeout",
    "detail": "<human-readable>",
    "retriable": true | false
  },
  "updated_at": "2026-..."
}
```

The `retriable` flag tells the agent whether to call `external_index` again with the same args (`fetch`, `commit`, `spot_interrupted`, `timeout`) or fix the inputs first (`build`, `abuse`).

## Testing Strategy

| Tier | Test | What it verifies |
|---|---|---|
| **Unit** (`tests/abuse_test.rs`) | URL validation matrix: link-local, AWS metadata, RFC1918, https-only, scheme rejection, size cap, allow-list | `abuse::validate` rejects/accepts correctly |
| **Unit** (`tests/jobs_test.rs`) | CRUD on `index_jobs` against embedded DuckDB/PG; dedup UNIQUE constraint; concurrent INSERT race; status transitions | `jobs::*` correct; idempotency holds |
| **Unit** (`tests/worker_test.rs`) | `worker::run_job` with a fixture crate: tempdir source, mock catalog DSN, assert `graph::build` artifact → `translate` → DuckLake snapshot. Covers happy path + each failure injection (bad source URL, missing revision, embedding model load fail simulated via env override) | Worker pipeline correctness without AWS |
| **Unit** (`tests/mcp_test.rs` — extend existing) | `external_index` and `external_index_status` arg parsing, validation, routing decision logic (warm-path catalog hit, dedup hit, StartExecution mock) | MCP handlers; serve-Lambda routing |
| **Integration** (`tests/state_machine_test.rs`) | LocalStack Step Functions + ECS + S3 + RDS (or PG in container). Submit `external_index` for a tiny fixture crate, poll `external_index_status` to `complete`, then call `external_code_search` against the freshly indexed revision | End-to-end happy path through real SF + real worker container |
| **Integration** | LocalStack with worker container killed mid-build (ECS `StopTask`) → assert SF `Retry` fires, second task succeeds, final status `complete` | Spot-interruption path |
| **Integration** | Concurrent identical `external_index` from two callers → assert single SF execution, both `job_id`s equal, single DuckLake snapshot | Dedup |
| **Integration** | Abuse cases against LocalStack: link-local URL, oversized tarball, rate-limit burst → assert `rejected` without `StartExecution` | Abuse prevention |
| **Smoke** (manual, documented) | Index `serde@1.0.197` via real agent → poll → `external_code_search({query: "Deserialize", package: "serde"})` → verify results | Production happy path |
| **Smoke** | Index `tokio` `main` branch via git URL → `external_knowledge_context({query: "spawn task", package: "tokio"})` → verify ranked evidence | Git source kind, large crate, embedding path |
| **Smoke** | Trigger spot interruption on a real Fargate Spot task (via AWS console capacity rebalance) → poll `external_index_status` → verify transparent retry → `complete` | Spot path on real infra |
| **Smoke** | Cold-start timing: invoke serve Lambda after idle, measure first-request latency → verify `< 1s` for `/index` (no heavy init; StartExecution is fast) | Lambda cold start acceptable |

### Test isolation principle

Unit tests must not touch AWS. The worker test uses an embedded DuckDB catalog file and tempdirs; the state-machine integration tests use LocalStack. No test should require real AWS credentials.

### Smoke tests are runbooks, not automation

They document the manual steps + expected output for verification before/after deployment. Live in `docs/superpowers/specs/` alongside this design (or `infra/spur-context-service/SMOKE.md`).

## Out of Scope for v2 (deferred to v3+)

- **Resume from checkpoint after spot interruption.** v2 reruns from scratch; DuckLake dedup makes this correct but slightly wasteful. Add resume only if spot-retry frequency justifies it.
- **Background pollers** (crates.io / git polling enqueuing into the same SM). The SM contract is ready; the poller itself is a separate workstream.
- **Webhook / notification on completion.** Agent polls in v2.
- **Multi-source resolvers** (e.g., auto-discover git URL from crates.io metadata when agent only gives `package@revision`). v2 requires explicit `source_url`.
- **Registry tarball cache.** Each fetch re-downloads. Cache layer can be added at the S3 path later (`s3://spur-context/sources/<hash>/`).
- **Tiered extraction.** Full graph for all packages in v2; lighter extraction for long-tail packages is v3.
- **Auth model beyond API Gateway authorizer principal.** v2 trusts the principal for rate-limit identity; richer ACLs (which callers can index which sources) are v3.
- **Embedding backfill job.** When embeddings fail and `embeddings_status='pending'`, v2 leaves them pending. A follow-up worker that fills embeddings without re-extracting structure is v3.
- **Cost & usage telemetry per caller.** Basic CloudWatch metrics in v2; per-caller attribution dashboard v3.

## Files Touched

**New:**
- `crates/spur-context-service/src/abuse.rs` — URL/rate-limit validation
- `crates/spur-context-service/src/jobs.rs` — `index_jobs` CRUD
- `crates/spur-context-service/src/worker.rs` — fetch + build + translate pipeline
- `crates/spur-context-service/src/bin/worker.rs` — Fargate entry binary (feature-gated)
- `crates/spur-context-service/sql/index_jobs.sql` — `index_jobs` DDL
- `crates/spur-context-service/tests/abuse_test.rs`, `jobs_test.rs`, `worker_test.rs`, `state_machine_test.rs`
- `infra/spur-context-service/` — Step Functions ASL, ECS cluster + capacity providers, worker task def, IAM roles
- `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md` — this file

**Modified:**
- `crates/spur-context-service/Cargo.toml` — `worker` feature, `spur-cli` dep
- `crates/spur-context-service/src/lib.rs` — module exports for `abuse`, `jobs`, `worker`
- `crates/spur-context-service/src/mcp.rs` — two new tool handlers + definitions
- `crates/spur-context-service/src/lambda.rs` — `/index`, `/index_status` routes + routing logic
- `crates/spur-context-service/sql/init_catalog.sql` — append `index_jobs` DDL
- `crates/spur-context-service/tests/mcp_test.rs` — extend for new tools

**Unchanged:**
- `crates/spur-context-service/src/catalog.rs`, `translate.rs`, `query.rs`, `knowledge.rs`
- `crates/spur-context-service/src/bin/index.rs`
- `crates/spur-graph/`, `crates/spur-analyst/`, `crates/spur-mcp/`
