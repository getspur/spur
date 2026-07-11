# Disposable Cognito authentication POC

This directory is an isolated Terraform root and state contract for `bd-2hv5u`.
It models only the Cognito authentication ingress and a validation-only job
boundary. It does not import or reference the production API, Lambda, table,
state machine, user pool, domain, secret, invoke policy, backend, or state key.

The committed defaults are inert: `poc_enabled=false` plans no AWS resources.
The root contains no credentials, real resource IDs, client-secret values, or
production identifiers. This implementation task runs offline checks only; it
does **not** run `terraform apply`, live smoke tests, or `terraform destroy`.

## Isolation contract

- Use a sandbox account whenever possible and a POC-only backend bucket, lock
  table, and unique state key.
- Replace the `poc_suffix`, `Owner`, and `CostCenter` placeholders. Every
  taggable resource gets `PocId`, `Environment`, `ManagedBy`, and ownership tags.
- The root creates its own Lite pool/domain/clients, API/JWT route, published
  Lambda and alias, job table, log groups, IAM role, and invoke policy.
- It creates no S3 bucket, worker, Step Functions state machine, VPC, queue
  drainer, or production data-plane reference. Lambda receives an empty state
  machine ARN and global/per-owner running and queued caps of zero. A uniquely
  named DuckLake SQLite catalog under Lambda's writable `/tmp` directory only
  satisfies the handler's local catalog bootstrap; it contains no persistent or
  production catalog data and is discarded with the execution environment.
- Generated M2M client secrets may exist in POC Terraform state. State is
  secret-bearing even though outputs expose client IDs only. Never publish a
  plan, state, logs, environment dump, authorization code, verifier, or token.
- `external_index` uses
  `fixtures/external-index-validation-only.json`. The Lambda allowlist contains
  only the nonmatching `poc-no-source.invalid` sentinel, so the fixture host
  `validation-only.invalid` is synchronously rejected as not allow-listed by
  pure URL validation before DNS, rate accounting, DynamoDB, enqueue, or
  dispatch. Zero queue/running caps and the empty state-machine ARN remain
  defense in depth; the reserved `.invalid` hostname alone is not the guard.

## Offline verification run in this task

From the repository root:

```sh
infra/spur-context-service/poc/scripts/offline-smoke.sh
```

The runner uses `scripts/spur-cargo --dir` for both Rust packages, executes the
standalone client's PKCE/OIDC, Basic-auth, cache, redirect, and redaction tests,
runs the context-service semantic auth tests, validates the sanitized evidence
matrix, checks shell syntax and teardown fixtures, and runs only Terraform
`fmt`, `init -backend=false`, `validate`, and mock-provider `test`/`plan` actions.
It does not read AWS credentials or call AWS.

## Operator sequence for a separately approved live POC

The commands in this section are a runbook, not authorization to run them. A
separate approval must name the sandbox account/profile, backend, unique suffix,
candidate commit, evidence location, retention deadline, and teardown owner.

1. **Capture the production baseline outside this root.** Create sanitized
   production plan JSON and resource-inventory JSON without credentials or
   secret values in the artifacts. Retain them as `before` evidence.
2. **Build committed candidates.** Build the service Lambda through
   `scripts/spur-cargo --dir crates/spur-context-service` and verify the
   standalone client through
   `scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client`.
   Package the committed Lambda bootstrap at the reviewed `lambda_zip_path`.
3. **Create local configuration outside version control.** Copy
   `terraform.tfvars.example` and `backends/poc.s3.tfbackend.example`; replace
   every marker with sandbox-only values. Set `poc_enabled=true` and set
   `creation_confirmation` to the exact guard string only after review.
4. **Initialize only the POC state.** From this directory, run
   `terraform init -reconfigure -backend-config=/secure/path/poc.s3.tfbackend`.
   Reject any backend key or inventory that overlaps the production baseline.
5. **Validate and plan only.** Run `terraform fmt -check -recursive`,
   `terraform validate`, `terraform test`, then
   `terraform plan -var-file=/secure/path/poc.tfvars -out=evidence/poc.tfplan`.
   Review every address, unique name/tag, zero cap, IAM ARN, and output. Scan the
   plan locally; never upload it because generated secret values can be in plan
   or state.

   Convert any plan selected for sanitized evidence with `terraform show -json`
   and scan it together with sanitized logs before retention:

   ```sh
   scripts/scan-secrets.py evidence/poc-plan.json evidence/sanitized-logs.json
   ```
6. **Apply only after separate approval.** The reviewed command is
   `terraform apply evidence/poc.tfplan`. This repository task intentionally
   does not run it.
7. **Run live smoke only against POC outputs.** Configure the standalone client
   from an approved secret channel/environment, never arguments or fixtures.
   Execute the objective cases in `fixtures/evidence-cases.json`, using the
   validation-only index input. Capture only request IDs, statuses, bounded
   reason enums, non-secret claim names, resource addresses, and metrics.
8. **Prove production unchanged.** Recreate sanitized `after` plan/inventory
   artifacts and run:

   ```sh
   scripts/compare-production.sh \
     before-plan.json after-plan.json \
     before-inventory.json after-inventory.json
   ```

9. **Destroy from POC state only.** Run
   `terraform plan -destroy -var-file=/secure/path/poc.tfvars`, review the full
   address set, then—after teardown approval—run
   `terraform destroy -var-file=/secure/path/poc.tfvars`.
10. **Verify teardown.** Read and review the POC ID from this root's isolated
    state, capture a fresh read-only inventory for that exact ID, and pass the
    independently retained expected ID to the verifier. The verifier rejects
    malformed suffixes and fails if the inventory's embedded `poc_id` differs,
    preventing a typo from proving an unrelated empty namespace:

    ```sh
    terraform output -raw poc_id
    reviewed_poc_id=UNIQUE_SUFFIX_FROM_REVIEWED_OUTPUT
    scripts/inventory.sh SANDBOX_PROFILE SANDBOX_REGION "$reviewed_poc_id" \
      >evidence/after-destroy-inventory.json
    scripts/verify-teardown.sh "$reviewed_poc_id" \
      evidence/after-destroy-inventory.json
    ```

The teardown inventory explicitly covers the tagged-resource API, user pool,
domain, resource server, all human/M2M app clients and generated-secret owners,
API Gateway API, Lambda function, published version and alias, DynamoDB table, both log groups,
IAM role, and invoke policy. Finally re-run the production comparison and delete
the POC backend state under the sandbox retention policy. Keep only approved,
sanitized evidence.
