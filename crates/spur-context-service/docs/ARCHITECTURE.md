# SPUR Context Service — Architecture

The context service indexes **external (third-party) packages** into a code-context
lakehouse and serves graph + knowledge queries over them. It is a public,
on-demand indexer: a caller asks for `pkg@rev`, a worker fetches and builds it,
and the results become queryable with low-latency, zero-database serving.

The design is a **medallion lakehouse** (bronze → silver → gold) with a strict
split between an **ingest plane** (write path, Postgres-backed) and a
**serving plane** (read path, S3-only). This document describes the architecture
as implemented across `crates/spur-context-service` and
`infra/spur-context-service`.

> Companion specs:
> - Design: `docs/superpowers/specs/2026-06-28-spur-context-medallion-design.md`
> - Infra/runtime details: `infra/spur-context-service/README.md`

---

## 1. The two planes at a glance

```mermaid
flowchart LR
    client([Caller]):::ext

    subgraph edge[API Gateway HTTP · AWS_IAM]
      gw{{SigV4 route<br/>execute-api:Invoke}}
    end

    subgraph lambda[Serving Lambda · arm64 / Graviton2]
      ingestTools[external_index<br/>external_index_status]
      serveTools[external_code_search / read / callers / callees<br/>external_knowledge_context]
    end

    client -->|signed request| gw --> lambda

    %% ---- Ingest plane (write) ----
    ingestTools -->|job + quota| ddb[(DynamoDB<br/>jobs · dedupe · quota · leases)]
    ingestTools -->|start| sfn[Step Functions<br/>index_build]
    sfn --> worker[Indexing Worker<br/>Lambda fast-start → Fargate fallback]
    worker -->|fetch| src[(Upstream source<br/>git / tarball)]
    worker -->|raw| bronze[[Bronze · S3]]
    worker -->|graph artifact| silver[[Silver · S3]]
    worker -->|translate / publish| aurora[(Aurora Serverless v2<br/>Postgres ingest catalog · scale-to-zero)]
    worker -->|export frozen snapshot| gold[[Gold · S3<br/>frozen DuckLake snapshot]]

    %% ---- Serving plane (read) ----
    serveTools -->|read pointer + parquet| gold
    serveTools -.->|NEVER| aurora

    classDef ext fill:#eee,stroke:#999;
    linkStyle 10 stroke:#c0392b,stroke-dasharray:4 3;
```

Key invariant (**"B1"**): **the serving plane never touches Postgres.** Reads are
served from an immutable, frozen DuckLake snapshot on S3. Aurora exists only so
concurrent ingest workers can share a transactional catalog while they build.

---

## 2. Medallion data layers

| Layer | Produced by | Lives in | Contents | Identity |
|---|---|---|---|---|
| **Bronze** | worker fetch | S3 (`bronze/…`) + registry row | Raw source exactly as fetched (tarball / git bundle) + content hash | `source / package / revision` |
| **Silver** | worker `spur graph build` | S3 (`silver/…`) + manifest row | Graph artifact (symbols, edges, sections) | same coordinate + artifact manifest |
| **Gold** | worker `translate` | Aurora (ingest) → frozen snapshot on S3 (serving) | Serving rows: symbols, refs, `symbol_embeddings`, knowledge sections, catalog metadata | coordinate + **generation** |

```mermaid
flowchart TD
    req[external_index<br/>source/package/revision] --> fetch

    subgraph B[Bronze - raw, persisted]
      fetch[fetch git/tarball] --> capR{registered<br/>hash match?}
      capR -->|yes| dedupB[reuse bronze<br/>skip upstream]
      capR -->|no| putB[put raw archive to S3<br/>register row]
    end

    B --> S
    subgraph S[Silver - graph artifact]
      build[spur graph build<br/>neoverse-n1 binary] --> putS[upload artifact + manifest]
    end

    S --> G
    subgraph G[Gold - serving]
      tx[translate rows into<br/>Aurora DuckLake catalog] --> gen[reserve generation<br/>Postgres sequence]
      gen --> publish[publish gold tables<br/>under advisory lock]
      publish --> snap[export frozen snapshot<br/>data-only copy + index replay]
      snap --> ptr[flip current.json pointer<br/>last, monotonic]
    end

    ptr --> serve[(Serving reads<br/>frozen snapshot)]
```

Each layer has **bronze dedup** (skip the upstream fetch when the registered
content hash matches) and source/build caps applied before promotion, so a job
that re-requests a cached package short-circuits cheaply.

---

## 3. Ingest plane (write path)

```mermaid
sequenceDiagram
    autonumber
    participant C as Caller (SigV4)
    participant GW as API GW (AWS_IAM)
    participant L as Serving Lambda
    participant D as DynamoDB
    participant SF as Step Functions
    participant W as Worker (Lambda→Fargate)
    participant A as Aurora (Postgres)
    participant S3 as S3 (bronze/silver/gold)

    C->>GW: external_index(pkg@rev, source_url)
    GW->>L: event + IAM authorizer identity
    L->>L: authenticated_caller_id()  (401 if none)
    L->>D: rate-limit + concurrent-cap (atomic)
    alt over quota
        L-->>C: rejected (rate_limit / concurrent_job_limit)
    else admitted
        L->>D: create job + dedupe + active_job + quota (TransactWrite)
        L->>SF: start execution (job_id, limits)
        SF->>W: run worker (max_source_bytes, max_build_seconds)
        W->>S3: fetch → bronze (cap-enforced)
        W->>S3: spur graph build → silver
        W->>A: translate rows (advisory lock for gold publish)
        W->>A: reserve generation (sequence)
        W->>S3: export frozen snapshot, flip current.json
        W->>D: record_completed → release active_job + quota
        L-->>C: queued (job_id)
    end
    C->>L: external_index_status(job_id)  (caller-scoped)
```

### Aurora Serverless v2 — ingest-only catalog
- `aurora-postgresql`, min **0 ACU** (scale-to-zero) — pay only while ingesting.
- RDS-managed master password in **Secrets Manager**; the worker injects it via
  `SPUR_CATALOG_PASSWORD_SECRET_ARN`.
- **Concurrency model:** unique generations come from a Postgres **sequence**;
  ordering of the publish critical section is enforced by a catalog-wide
  **advisory session lock** (`pg_advisory_lock`) around schema setup → generation
  reserve → gold writes → publish → snapshot export. Throughput ceiling is one
  gold-publish lane per catalog (fetch/build stay parallel).

### Worker runtime
- **Lambda fast-start** first; **Fargate** fallback for long/large builds
  (`runTask.waitForTaskToken`, timeout = `context_max_build_seconds + 900`).
- Resource caps: source-tree size (git 2 GB / tarball 500 MB) enforced after
  fetch; `spur graph build` killed after `context_max_build_seconds`.
- Spot-interruption handler writes a checkpoint so work can resume.

---

## 4. Serving plane (read path) — frozen DuckLake snapshot

```mermaid
flowchart LR
    q[external_code_* /<br/>external_knowledge_context] --> ptrget[GET gold/catalog-snapshot/current.json]
    ptrget --> gen[resolve generation<br/>+ snapshot URI + sha256]
    gen --> cache{local cache<br/>fresh?}
    cache -->|hit by etag+gen+sha| dq[DuckDB query<br/>frozen .ducklake + parquet via httpfs]
    cache -->|miss| dl[download immutable snapshot<br/>verify sha256] --> dq
    dq --> resp[symbols / source / hybrid vector evidence]
```

The **frozen snapshot** is a self-contained, data-only copy of the 29
`ducklake_*` metadata tables plus a **replay of the 5 UNIQUE primary-key
indexes** (a plain `COPY FROM DATABASE` fails on DuckLake metadata indexes).

- Each generation writes an **immutable** snapshot object + manifest; a mutable
  `current.json` pointer is flipped **last**.
- Publication is **monotonic**: the pointer never moves to an older generation;
  combined with a transaction-consistent metadata copy, a concurrent or failed
  publish can never produce a torn/rolled-back serving view.
- **Rollback** = repoint `current.json` to a previous generation's manifest.
- Serving cache invalidates on `pointer-etag + generation + snapshot-uri + sha256`.

---

## 5. Security, tenancy & abuse controls

```mermaid
flowchart TD
    subgraph A[Authentication]
      a1[API GW route = AWS_IAM<br/>SigV4 + execute-api:Invoke policy]
      a2[Lambda authenticated_caller_id<br/>JWT sub / IAM userArn / principalId<br/>→ 401 if absent]
    end
    subgraph T[Tenancy isolation]
      t1[external_index_status scoped to caller_id<br/>cross-caller → not_found]
      t2[shared catalog = PUBLIC packages only<br/>no private data-plane isolation]
    end
    subgraph Q[Quotas / caps - DynamoDB atomic]
      q1[fixed-window rate limit / caller / min]
      q2[concurrent active-job cap / caller]
      q3[source-size caps + build timeout]
      q4[API GW route throttle rate/burst]
    end
    A --> T --> Q
```

- **Auth is defense-in-depth:** even if the route were misconfigured open, the
  Lambda refuses mutating tools (`external_index`, `external_index_status`)
  without an authenticated identity. The old source-IP fallback is test-only.
- **Quotas are atomic** DynamoDB conditional writes (rate counter per
  epoch-minute window; `active_count < limit` guard transacted with job
  creation; released on terminal state).

---

## 6. CPU baseline (Graviton2-safe)

Deployable arm64 artifacts target **Graviton2 / neoverse-n1**. Lambda and
Fargate arm64 run on Graviton2; the build VM is Graviton4 (neoverse-v2), and a
neoverse-v2 binary **SIGILLs** on Graviton2. `deploy.sh` builds through a
`run_graviton2_safe_cargo` wrapper, and `test-graviton2-baseline.sh` guards it in
CI.

---

## 7. CI/CD

```mermaid
flowchart LR
    pr[PR / push touching service] --> j1[crate test --all-features]
    pr --> j2[deploy + workflow guardrails<br/>+ graviton2 baseline guard]
    wd[workflow_dispatch · staging env · OIDC] --> j3[build_aws_artifacts<br/>worker images + Lambda zip]
    wd --> j4[run_staging_smoke<br/>real-AWS E2E mirroring E1]
    j1 --> j4
    j2 --> j4
```

- PR/push runs tests + guardrails only — **no AWS, no Terraform**.
- Real-AWS jobs are `workflow_dispatch` + `context-service-staging` environment +
  OIDC, and never run `terraform apply`.
- **Staging smoke** (`smoke-staging-e2e.py`) codifies the manual E1 run: publish a
  tiny fixture → `external_index` (IAM identity) → poll complete → assert
  non-zero `symbol_embeddings`, bronze/silver/gold objects, and serve
  search/read/knowledge with **vector-backed** evidence, asserting **zero-Postgres**
  serving.

---

## 8. AWS resource map

| Resource | Role |
|---|---|
| `aws_apigatewayv2_api/route/stage` | HTTP front door, `AWS_IAM` auth, route throttling |
| `aws_iam_policy.context_service_invoke` | SigV4 `execute-api:Invoke` for allowed callers |
| `aws_lambda_function.service` | arm64 serving Lambda (ingest control plane + serving) |
| `aws_rds_cluster` (Aurora Serverless v2) | Postgres **ingest-only** catalog, scale-to-zero |
| Secrets Manager | Aurora master password |
| `aws_dynamodb_table.index_jobs` | jobs, dedupe, active-job + caller quota records |
| `aws_dynamodb_table.catalog_leases` | DuckLake catalog write leases |
| `aws_sfn_state_machine.index_build` | on-demand indexing orchestration |
| `aws_lambda_function.worker` / ECS Fargate | indexing worker (fast-start → fallback) |
| S3 data bucket | `bronze/` · `silver/` · `gold/catalog-snapshot/` (frozen snapshot + `current.json`) |

---

## 9. Known follow-ups

1. **Quota-counter drift** under hard worker kill — make the active-job counter
   TTL-safe (count `ACTIVE_JOB#` items or reconciler sweep) so a job that dies
   without a terminal status cannot permanently consume a slot.
2. **Concurrency test in CI** — `concurrent_translates_…` is gated on
   `SPUR_CONTEXT_AURORA_TEST_DSN`; wire it into the staging job to validate the
   advisory lock against real Aurora.
3. **API-GW-level auth test** — the smoke uses `aws lambda invoke` (Lambda-side
   auth); add a check of the SigV4 edge gate (signed 2xx vs unsigned 401/403).
4. **VPC endpoints / NAT** for the now-in-VPC worker Lambda (S3 + Step Functions
   egress).
5. **Smoke string assumptions** (bronze/silver/gold S3 key layouts, `hybrid`
   grounding label) — validate on first real staging run.
