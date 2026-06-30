# Context Service Fetch-Split Design

Date: 2026-06-30

## Verdict

Fetch-split is the right design for the deployed medallion stack. It keeps
Aurora private, keeps the indexing worker NAT-free, and confines internet egress
to a small stateless source fetcher that does not need catalog credentials. This
is better than a NAT gateway, which would give the VPC worker broad egress while
it also holds Aurora and DuckLake privileges, and better than public Aurora,
which exposes the catalog to solve a problem that only exists in the source
fetch stage.

There is one important correction to the proposed handoff: the existing worker
cannot consume a raw `s3://.../fetch/<job_id>/source.tar.gz` as `SOURCE_URL`.
It validates source URLs through the abuse validator, whose accepted schemes are
only `https`, `git+https`, and `git+ssh` (`crates/spur-context-service/src/abuse.rs:225-233`).
The current green S3 tarball smoke uses an S3 object plus `aws s3 presign`, then
passes the presigned HTTPS URL to `external_index` (`infra/spur-context-service/smoke-staging-e2e.py:152-175`,
`infra/spur-context-service/smoke-staging-e2e.py:399-421`). Therefore the
fetcher should upload the archive to S3, return a presigned HTTPS S3 URL, and
set `source_kind` to `tarball`.

With that correction, the design is correct. The existing worker tarball path
downloads the URL, extracts it, and enforces the tarball source-tree cap
(`crates/spur-context-service/src/worker.rs:2429-2467`,
`crates/spur-context-service/src/worker.rs:2708-2755`,
`crates/spur-context-service/src/worker.rs:2795-2804`). No worker behavior
change is required for the first implementation.

## Grounding

- Step Functions currently starts at `RunLambdaBuild` and invokes the worker
  Lambda alias with `job_id`, `package`, `revision`, `source`, `source_url`,
  `source_kind`, and `limits` (`infra/spur-context-service/index_build_asl.json:1-20`).
- The Lambda worker catches Lambda service/timeouts to `RunBuild`, then checks
  `lambdaResult.Payload.status`; deterministic worker failures go to
  `LambdaBuildFailed`, while non-complete/non-failed responses fall back to ECS
  (`infra/spur-context-service/index_build_asl.json:34-72`).
- ECS fallback receives the same source fields from the state input
  (`infra/spur-context-service/index_build_asl.json:72-180`,
  `infra/spur-context-service/index_build_asl.json:182-270`).
- The ASL is loaded through `templatefile` in Terraform, currently with only
  `worker_lambda_arn` as a Lambda target
  (`infra/spur-context-service/state_machine.tf:1-24`).
- The worker Lambda is VPC-attached in the worker subnets
  (`infra/spur-context-service/lambda_worker.tf:14-33`).
- Aurora is private: the cluster uses the catalog security group, and the writer
  instance has `publicly_accessible = false`
  (`infra/spur-context-service/main.tf:75-112`). The catalog security group only
  allows Postgres from the worker security group
  (`infra/spur-context-service/state_machine.tf:61-77`).
- NAT-free worker access is intentional: worker subnets are described as private,
  with S3/DynamoDB gateway endpoints and service interface endpoints
  (`infra/spur-context-service/variables.tf:189-209`,
  `infra/spur-context-service/vpc_endpoints.tf:1-45`).
- `external_index` already validates source URLs, resolves DNS, rate-limits,
  dedupes active jobs, and starts Step Functions with the source fields in the
  payload (`crates/spur-context-service/src/mcp.rs:395-533`).
- `external_index` already accepts `source_kind` as optional `git` or `tarball`
  and infers it when omitted (`crates/spur-context-service/src/mcp.rs:681-718`,
  `crates/spur-context-service/src/mcp.rs:1403-1427`).
- The worker source dispatcher already supports `git` and `tarball`
  (`crates/spur-context-service/src/worker.rs:1337-1350`,
  `crates/spur-context-service/src/worker.rs:2299-2335`). The `git` path shells
  out to `git clone --filter=blob:none` and checks out `revision`
  (`crates/spur-context-service/src/worker.rs:2338-2376`). The tarball path
  downloads and extracts a tarball or zip
  (`crates/spur-context-service/src/worker.rs:2418-2467`,
  `crates/spur-context-service/src/worker.rs:2708-2755`).
- Bronze raw-source metadata is derived from the worker environment, so if the
  worker receives the fetched S3 tarball URL, `bronze.raw_sources.source_url`
  records that staged URL rather than the original upstream URL
  (`crates/spur-context-service/src/worker.rs:2569-2592`). The original URL still
  remains in the DynamoDB job record created before Step Functions starts
  (`crates/spur-context-service/src/mcp.rs:454-465`).

## Proposed Data Flow

1. `external_index` receives the original `source_url`.
2. `mcp.rs` validates the URL exactly as it does today, infers or honors
   `source_kind`, and adds a boolean `prefetch_source` to the Step Functions
   payload.
3. Step Functions routes:
   - `prefetch_source == false`: prepare worker input from the original payload.
   - `prefetch_source == true`: invoke `FetchSource`, then prepare worker input
     from the fetcher output.
4. `FetchSource` is a non-VPC Lambda. It downloads or clones the source into
   `/tmp`, normalizes it to `source.tar.gz`, uploads it to
   `s3://<data-bucket>/fetch/<job_id>/source.tar.gz`, and returns:

   ```json
   {
     "source_url": "https://<presigned-s3-url>",
     "source_kind": "tarball",
     "source_archive_s3_uri": "s3://<bucket>/fetch/<job_id>/source.tar.gz",
     "original_source_url": "https://github.com/owner/repo",
     "original_source_kind": "git",
     "content_sha256": "...",
     "bytes": 1234
   }
   ```

5. Both the Lambda worker and ECS fallback read a normalized `workerInput`
   object, not the original top-level source fields. That ensures ECS fallback
   consumes the fetched tarball instead of trying to reach GitHub from the VPC.
6. The existing worker sees `source_kind = "tarball"` and `source_url =
   <presigned HTTPS S3 URL>`, so it follows the current green tarball path.

## Fetch Routing Decision

Add a small routing helper in `crates/spur-context-service/src/mcp.rs`:

- `SourceKind::Git` -> `prefetch_source = true`.
- `SourceKind::Tarball` and HTTPS S3 hostname -> `prefetch_source = false`.
- `SourceKind::Tarball` and non-S3 hostname -> `prefetch_source = true`.

This preserves the current staging smoke, where a presigned S3 tarball should
continue to go straight to the worker, while also fixing generic internet
tarballs that the VPC worker cannot fetch.

Recognized S3 HTTPS hosts should cover at least:

- `bucket.s3.amazonaws.com`
- `bucket.s3.<region>.amazonaws.com`
- `s3.amazonaws.com`
- `s3.<region>.amazonaws.com`

Do not accept raw `s3://` as `source_url` in `external_index` in the first
implementation. That would require changing the existing abuse validator and
worker download path, and the current smoke proves presigned HTTPS is the
established contract.

## Component Shape

Use a separate small crate and image:

- Create `crates/spur-context-source/` for shared URL validation and source-kind
  inference. Move the current `abuse.rs` logic there and keep
  `spur_context_service::abuse` as a re-export so existing service imports and
  tests remain stable.
- Create `crates/spur-context-fetcher/` for the non-VPC Lambda.

This is preferable to adding another binary to `spur-context-service` because
the current crate unconditionally carries heavy DuckDB dependencies
(`crates/spur-context-service/Cargo.toml:39-57`). A separate fetcher crate keeps
the internet-exposed image small and keeps DuckDB/Aurora code out of the fetch
trust boundary.

The fetcher image should include `git`, `curl`, `tar`, `unzip`, and
`ca-certificates`, mirroring the existing worker image tools without DuckDB or
the `spur` binary (`infra/spur-context-service/deploy.sh:324-364`).

## Fetch Implementation Decisions

### Git Strategy

Use the `git` binary, not GitHub codeload, for the baseline.

Reasoning:

- It matches the current worker semantics: `git clone --filter=blob:none`, then
  checkout the requested revision (`crates/spur-context-service/src/worker.rs:2350-2376`).
- It works for GitHub, GitLab, Bitbucket, self-hosted public Git, tags, branches,
  and SHAs.
- It gives a path to private repos through standard HTTPS credentials or SSH
  keys without baking GitHub-specific URL logic into the service.

Use hardening flags/environment:

- `GIT_TERMINAL_PROMPT=0`
- `git -c protocol.file.allow=never -c protocol.ext.allow=never clone --filter=blob:none ...`
- no submodule recursion in v1
- strip `.git` from the output tarball

GitHub codeload can be a later optimization for public GitHub repos, but it
should not be the primary contract because it is provider-specific and changes
revision/auth behavior.

### Private Repos And Auth

First ship public HTTPS sources only. Add private source support as an explicit
follow-up:

- HTTPS token auth via Secrets Manager, configured by trusted deployment env
  rather than user-supplied tokens in `source_url`.
- `GIT_ASKPASS` or credential helper files created in `/tmp` and deleted before
  return.
- Optional `git+ssh` support only after adding a known-hosts policy and an SSH
  private key secret.

Reject or ignore credentials embedded in URLs in v1 to avoid leaking tokens into
job records, Step Functions input, S3 metadata, or logs.

### Non-Git HTTP Sources

For `source_kind = "tarball"` and non-S3 HTTPS hosts, the fetcher downloads the
archive with explicit byte caps and redirect validation. If the input is zip,
extract and repack to `source.tar.gz` so the worker can always consume the
fetcher output as a tarball URL whose key ends in `.tar.gz`.

### Huge Repos

Do not add a fetch ECS fallback in the first implementation. Lambda's 15-minute
runtime and 10 GB `/tmp` are enough for the intended fast path, and a fetch ECS
fallback would require designing a separate public-egress task that still has no
Aurora access. If fetch fails because of timeout or size, return a deterministic
`fetch_failed` or `source_too_large` failure and let the user retry with a
smaller source or a prebuilt tarball. Add a dedicated public-subnet fetch ECS
task later only if real workloads need it.

## ASL Plan

Modify `infra/spur-context-service/index_build_asl.json`:

1. Change `StartAt` from `RunLambdaBuild` to `RouteSource`.
2. Add `RouteSource` Choice:
   - `$.prefetch_source == true` -> `FetchSource`
   - default -> `PrepareOriginalWorkerInput`
3. Add `FetchSource` Task:
   - `Resource`: `arn:aws:states:::lambda:invoke`
   - `FunctionName`: `${source_fetch_lambda_arn}`
   - payload: `job_id`, `package`, `revision`, `source`, `source_url`,
     `source_kind`, `limits`
   - retry Lambda service errors
   - fail deterministic fetch failures; do not route fetch failures to the VPC
     worker
4. Add `PrepareFetchedWorkerInput` Pass state:
   - copies `job_id`, `package`, `revision`, `source`, `limits`
   - uses `$.fetchResult.Payload.source_url`
   - sets `source_kind` from `$.fetchResult.Payload.source_kind`
5. Add `PrepareOriginalWorkerInput` Pass state:
   - copies the original fields unchanged
6. Change `RunLambdaBuild`, `RunBuild`, and `FallbackBuild` to read
   `$.workerInput.source_url` and `$.workerInput.source_kind`.

This preserves the existing Lambda-worker-first and ECS fallback model, while
ensuring both worker backends use the same normalized source input.

## Terraform Plan

Add or modify these files:

- `infra/spur-context-service/source_fetcher_lambda.tf`
  - `aws_cloudwatch_log_group.source_fetcher_lambda`
  - `aws_lambda_function.source_fetcher`
  - `aws_lambda_alias.source_fetcher_live`
  - no `vpc_config`
  - env:
    - `SPUR_CONTEXT_FETCH_BUCKET = aws_s3_bucket.data.bucket`
    - `SPUR_CONTEXT_FETCH_PREFIX = "fetch"`
    - `SPUR_CONTEXT_MAX_TARBALL_BYTES`
    - `SPUR_CONTEXT_MAX_GIT_BYTES`
    - `SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS`
    - `SPUR_CONTEXT_FETCH_PRESIGN_SECONDS`, default 21600
- `infra/spur-context-service/iam.tf`
  - separate fetcher Lambda execution role
  - CloudWatch Logs permissions
  - S3 `PutObject`, `GetObject`, `AbortMultipartUpload`, and scoped `ListBucket`
    for `fetch/*`
  - no Secrets Manager, DynamoDB, Step Functions, Aurora, or VPC-access policy
    in v1
  - add a dedicated Step Functions `lambda:InvokeFunction` policy scoped to the
    fetcher function and live alias
- `infra/spur-context-service/state_machine.tf`
  - pass `source_fetch_lambda_arn` into the ASL template
- `infra/spur-context-service/variables.tf`
  - `source_fetcher_lambda_image`
  - `source_fetcher_lambda_timeout_sec`, default 900
  - `source_fetcher_lambda_memory_mb`, default 1024
  - `source_fetcher_lambda_ephemeral_storage_mb`, default 10240
  - `source_fetch_presign_seconds`, default 21600
  - `fetch_artifact_retention_days`, default 7
- `infra/spur-context-service/main.tf`
  - S3 lifecycle rule expiring `fetch/` objects and noncurrent versions after
    `fetch_artifact_retention_days`
- `infra/spur-context-service/deploy.sh`
  - build and push the new fetcher image
  - pass `-var source_fetcher_lambda_image=...` during Terraform
  - include the fetcher image in the normal worker-image build path and log its
    URI alongside the ECS and worker-Lambda images
- `infra/spur-context-service/terraform.tfvars.example`
  - document the new fetcher image variable and tuning knobs
- `infra/spur-context-service/README.md`
  - document the fetch-split architecture and the presigned HTTPS handoff

## Idempotency And Cleanup

- Use deterministic staging key `fetch/<job_id>/source.tar.gz`.
- On retry, `HeadObject` the key. If metadata matches `original_source_url_hash`,
  `revision`, and `source_kind`, return a fresh presigned URL instead of
  refetching.
- If the key exists with mismatched metadata, overwrite it for the same `job_id`.
  The job ID is already scoped to one active external-index request.
- Upload directly to the final key or use `fetch/<job_id>/source.tar.gz.tmp`
  followed by copy if multipart partial-state becomes a practical issue.
- Add lifecycle expiration for `fetch/` objects. Seven days is a safe default:
  it is much longer than Lambda/ECS execution windows, while bronze archives
  remain stored under `bronze/...` after successful ingestion.

## Risks And Mitigations

- Raw `s3://` handoff would fail today. Mitigation: fetcher returns presigned
  HTTPS S3 URL, not `s3://`.
- Presigned URL expiry could break ECS fallback after Lambda timeout. Mitigation:
  use a presign TTL comfortably above worker Lambda timeout plus ECS timeout,
  with `SPUR_CONTEXT_FETCH_PRESIGN_SECONDS = 21600` (6 hours) in v1.
- Source provenance changes if the worker remains unchanged. Mitigation: accept
  that bronze `source_url` records the staged tarball in v1; keep the original
  source URL in the job record and fetcher output. If bronze provenance must
  show the original upstream URL, add a later worker field such as
  `ORIGINAL_SOURCE_URL`.
- SSRF and arbitrary download abuse move to the non-VPC fetcher. Mitigation:
  reuse the existing validation logic, honor `allowed_source_domains`, reject
  private DNS/IP targets, revalidate redirects for HTTP tarballs, cap bytes, cap
  unpacked tree size, set API and per-caller rate limits already present in
  `mcp.rs`, and avoid logging full presigned URLs.
- Git redirects and protocol abuse are harder to inspect than normal HTTP
  downloads. Mitigation: allow only HTTPS/git+HTTPS in v1, disable file/ext
  protocols, set `GIT_TERMINAL_PROMPT=0`, and prefer deployment allowlists for
  production.
- Lambda `/tmp` and timeout limits can reject large repos. Mitigation: fail
  deterministically with `source_too_large` or `fetch_timeout`; add a dedicated
  no-Aurora public-egress fetch ECS task later if needed.
- Private repo credentials can leak if accepted in URLs. Mitigation: reject
  URL-embedded credentials and defer private auth to Secrets Manager based
  deployment config.
- S3 bucket policies that require a VPC endpoint would block the non-VPC
  fetcher. The current Terraform has no bucket policy; if one is added later,
  it must explicitly allow the fetcher role to write and sign `fetch/*`.

## Test Plan

Rust tests:

- Move pure URL-validation coverage from
  `crates/spur-context-service/tests/abuse_test.rs` into
  `crates/spur-context-source/tests/abuse_test.rs`, and keep a small
  `spur_context_service::abuse` re-export smoke test in the service crate.
- Add `mcp.rs` tests proving:
  - GitHub repo URL produces `source_kind = git` and `prefetch_source = true`.
  - `https://example.com/source.tar.gz` with tarball kind produces
    `prefetch_source = true`.
  - a presigned S3 HTTPS tarball hostname produces `prefetch_source = false`.
  - existing job dedupe and rate-limit behavior remain unchanged.
- Add fetcher unit tests for:
  - git command construction through a command-runner abstraction, using a fake
    runner to prove clone, checkout, archive, and cleanup commands.
  - HTTP tarball download with redirect validation.
  - zip-to-tar.gz normalization.
  - S3 key and metadata construction.

Infra/static validation:

- Render the ASL template with test values and validate JSON syntax.
- `terraform fmt -check` and `terraform validate` in
  `infra/spur-context-service` without applying.
- Confirm the Step Functions IAM policy includes both worker and fetcher Lambda
  invoke permissions.

Integration smoke:

- Existing staging medallion smoke must remain green. It should take the
  `prefetch_source = false` path because it passes a presigned S3 tarball
  (`infra/spur-context-service/smoke-staging-e2e.py:152-175`).
- Add a new staging smoke that calls `external_index` with a public GitHub repo
  URL and verifies the job completes through `FetchSource`.
- Add a non-S3 HTTPS tarball smoke that verifies generic internet tarballs also
  complete through `FetchSource`.
- Do not run `terraform apply` or deploy from implementation PR checks unless
  explicitly requested.

## Smallest Shippable Build Sequence

1. Extract shared source URL validation into `crates/spur-context-source`, keep
   `spur_context_service::abuse` re-exported, and run the existing abuse/MCP
   tests through `scripts/spur-cargo test -p spur-context-service --test abuse_test`
   and `scripts/spur-cargo test -p spur-context-service --test mcp_test`.
2. Add `prefetch_source` computation and payload tests in
   `crates/spur-context-service/src/mcp.rs`.
3. Add `crates/spur-context-fetcher` with public HTTPS git clone and tarball
   fetch-to-`source.tar.gz`, local unit tests, and S3 upload/presign code behind
   a small interface.
4. Wire ASL `RouteSource`, `FetchSource`, `PrepareFetchedWorkerInput`, and
   `PrepareOriginalWorkerInput`; keep both worker backends reading
   `$.workerInput`.
5. Add Terraform for the non-VPC fetcher Lambda, least-privilege IAM, state
   machine template variables, and fetch artifact lifecycle.
6. Update `deploy.sh` and docs to build/push the fetcher image.
7. Run format, focused Rust tests, ASL render validation, and Terraform
   validation.
8. Run the existing staging smoke unchanged, then a new GitHub-source smoke in
   staging.

## Non-Goals For V1

- Public Aurora.
- NAT gateway for worker egress.
- Raw `s3://` `source_url` support.
- Private Git auth unless explicitly configured through Secrets Manager.
- Fetch ECS fallback for huge repos.
