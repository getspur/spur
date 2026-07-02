import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"
REMOTE_BUILD_SH = ROOT / "infra" / "spur-context-service" / "build-and-push-remote.sh"
INFRA_DIR = ROOT / "infra" / "spur-context-service"
VERSIONS_TF = INFRA_DIR / "versions.tf"
CONTEXT_SERVICE_WORKFLOW = ROOT / ".github" / "workflows" / "context-service.yml"
STAGING_SMOKE = INFRA_DIR / "smoke-staging-e2e.py"
STAGING_SMOKE_ENTRYPOINT = INFRA_DIR / "smoke-staging-e2e.sh"


def render_index_build_asl():
    template = (INFRA_DIR / "index_build_asl.json").read_text()
    values = {
        "cluster_arn": "arn:aws:ecs:ap-southeast-5:123456789012:cluster/spur-context",
        "worker_taskdef_arn": (
            "arn:aws:ecs:ap-southeast-5:123456789012:"
            "task-definition/spur-context-worker:1"
        ),
        "worker_lambda_arn": (
            "arn:aws:lambda:ap-southeast-5:123456789012:"
            "function:spur-context-worker:live"
        ),
        "source_fetch_lambda_arn": (
            "arn:aws:lambda:ap-southeast-5:123456789012:"
            "function:spur-context-source-fetcher:live"
        ),
        "worker_lambda_timeout_sec": "900",
        "source_fetcher_lambda_timeout_sec": "900",
        "worker_ecs_timeout_sec": "2700",
        "index_jobs_table_name": "spur-context-index-jobs",
        "catalog_leases_table_name": "spur-context-catalog-leases",
        "catalog_dsn": (
            "postgres:host=writer.example.com port=5432 dbname=spur_context "
            "user=spur_context sslmode=require"
        ),
        "context_ducklake_data_path": "s3://spur-context/gold/data/",
        "worker_checkpoint_uri_template": (
            "s3://spur-context/jobs/{}/checkpoint.json"
        ),
        "subnets_json": json.dumps(["subnet-123"]),
        "security_groups_json": json.dumps(["sg-123"]),
    }

    rendered = re.sub(r"\${([A-Za-z0-9_]+)}", lambda m: values[m.group(1)], template)
    return json.loads(rendered)


def test_deploy_builds_standalone_context_service_from_crate_workdir():
    script = DEPLOY_SH.read_text()

    assert 'run_graviton2_safe_cargo "serving Lambda bootstrap"' in script
    assert "--workdir crates/spur-context-service build --features lambda --release" in script
    assert 'run_graviton2_safe_cargo "Fargate worker binary"' in script
    assert "--workdir crates/spur-context-service build --features worker --release" in script
    assert 'run_graviton2_safe_cargo "spur CLI worker image dependency"' in script
    assert "build -p spur-cli --release" in script
    assert (
        'fetch_remote_worktree_file '
        'crates/spur-context-service/target/release/spur-context-service '
        '"$BUILD_DIR/bootstrap"'
    ) in script
    assert (
        '--remote-binary "$(remote_worktree_path '
        'crates/spur-context-service/target/release/spur-context-worker)"'
    ) in script
    assert '--remote-binary "$(remote_target_path target/release/spur)"' in script
    assert "fetch_remote_target_file target/release/spur" not in script
    assert '--context-dir "$worker_context"' in script
    assert '--context-dir "$worker_lambda_context"' in script
    assert 'worker_image_uri="$(build_and_push_worker_image)"' not in script
    assert "scripts/spur-cargo run --workdir crates/spur-context-service" not in script
    assert "scripts/spur-cargo build -p spur-context-service" not in script


def test_deploy_has_selectable_self_contained_buildx_path_with_baseline_flags():
    script = DEPLOY_SH.read_text()

    assert 'BUILD_MODE="${SPUR_CONTEXT_SERVICE_BUILD_MODE:-remote}"' in script
    assert "--build-mode) BUILD_MODE=\"$2\"" in script
    assert "--no-push) PUSH_IMAGES=false" in script
    assert "build_self_contained_artifacts()" in script
    assert "prepare_self_contained_build_context()" in script
    assert 'git -C "$REPO_ROOT" archive --format=tar HEAD' in script
    assert "buildx build" in script
    assert "--platform linux/arm64 --provenance=false" in script
    assert "--output \"type=local,dest=$export_dir\"" in script
    assert 'assert_graviton2_safe_flags "self-contained buildx artifacts"' in script
    assert 'RUSTFLAGS="$SPUR_CONTEXT_GRAVITON2_RUSTFLAGS"' in script
    assert 'CFLAGS="$SPUR_CONTEXT_GRAVITON2_CFLAGS"' in script
    assert 'CXXFLAGS="$SPUR_CONTEXT_GRAVITON2_CXXFLAGS"' in script
    assert "SPUR_CONTEXT_GRAVITON2_RUSTFLAGS" in script
    assert "SPUR_CONTEXT_GRAVITON2_CFLAGS" in script
    assert "SPUR_CONTEXT_GRAVITON2_CXXFLAGS" in script
    assert "fetch_remote_worktree_file" in script


def test_self_contained_worker_images_build_locally_without_ecr_mutations_by_default():
    script = DEPLOY_SH.read_text()

    assert "build_local_worker_images()" in script
    assert 'local output_dir="$REPO_ROOT/target/lambda"' in script
    assert '--output "type=docker,dest=$output_dir/spur-context-worker-image.tar"' in script
    assert '--output "type=docker,dest=$output_dir/spur-context-worker-lambda-image.tar"' in script
    assert '--output "type=docker,dest=$output_dir/spur-context-source-fetcher-image.tar"' in script
    assert 'if [[ "$PUSH_IMAGES" == "true" ]]' in script
    assert "build_and_push_worker_image" in script
    assert "build_and_push_worker_lambda_image" in script
    assert "build_and_push_source_fetcher_lambda_image" in script
    assert 'aws ecr describe-repositories --repository-names "$WORKER_ECR_REPO"' in script
    assert 'aws ecr describe-repositories --repository-names "$WORKER_LAMBDA_ECR_REPO"' in script
    assert 'aws ecr describe-repositories --repository-names "$SOURCE_FETCHER_LAMBDA_ECR_REPO"' in script


def test_deploy_tags_worker_images_with_immutable_tag_and_latest_pointer():
    script = DEPLOY_SH.read_text()

    assert 'IMAGE_TAG="${SPUR_CONTEXT_SERVICE_IMAGE_TAG:-$(resolve_image_tag)}"' in script
    assert 'WORKER_IMAGE_TAG="$IMAGE_TAG"' in script
    assert 'LATEST_IMAGE_TAG="latest"' in script
    assert 'git -C "$REPO_ROOT" rev-parse --short HEAD' in script
    assert 'git -C "$REPO_ROOT" status --porcelain --untracked-files=normal' in script
    assert 'ecr_latest_image_tag "$WORKER_ECR_REPO"' in script
    assert 'ecr_latest_image_tag "$WORKER_LAMBDA_ECR_REPO"' in script
    assert 'ecr_latest_image_tag "$SOURCE_FETCHER_LAMBDA_ECR_REPO"' in script
    assert 'tag_ecr_image_as_latest "$WORKER_ECR_REPO" "$IMAGE_TAG"' in script
    assert 'tag_ecr_image_as_latest "$WORKER_LAMBDA_ECR_REPO" "$IMAGE_TAG"' in script
    assert 'tag_ecr_image_as_latest "$SOURCE_FETCHER_LAMBDA_ECR_REPO" "$IMAGE_TAG"' in script


def test_deploy_worker_image_contains_worker_and_spur_binaries():
    deploy_sh = DEPLOY_SH.read_text()

    assert "COPY spur-context-worker /usr/local/bin/spur-context-worker" in deploy_sh
    assert "COPY spur /usr/local/bin/spur" in deploy_sh
    assert "spur --version" in deploy_sh
    assert "spur-context-worker" in deploy_sh


def test_worker_images_bundle_duckdb_extensions_for_offline_loads():
    script = DEPLOY_SH.read_text()

    assert 'WORKER_DUCKDB_EXTENSION_DIR="/opt/duckdb/extensions"' in script
    assert (
        'EXTENSIONS=("httpfs" "ducklake" "postgres_scanner" "sqlite_scanner" "aws" "parquet" "json")'
        in script
    )
    assert 'copy_worker_extensions "$worker_context"' in script
    assert 'copy_worker_extensions "$worker_lambda_context"' in script
    assert "COPY duckdb-extensions/ /opt/duckdb/extensions/" in script
    assert (
        "ENV SPUR_CONTEXT_DUCKDB_EXTENSION_DIR=/opt/duckdb/extensions"
        in script
    )
    assert (
        "test -f /opt/duckdb/extensions/v${DUCKDB_VERSION}/${EXT_PLATFORM}/httpfs.duckdb_extension"
        in script
    )
    assert (
        "test -f /opt/duckdb/extensions/v${DUCKDB_VERSION}/${EXT_PLATFORM}/ducklake.duckdb_extension"
        in script
    )
    assert (
        "test -f /opt/duckdb/extensions/v${DUCKDB_VERSION}/${EXT_PLATFORM}/postgres_scanner.duckdb_extension"
        in script
    )
    assert (
        "test -f /opt/duckdb/extensions/v${DUCKDB_VERSION}/${EXT_PLATFORM}/sqlite_scanner.duckdb_extension"
        in script
    )
    assert script.index("download_extensions") < script.index("build_and_push_worker_image")


def test_deploy_builds_lambda_worker_image_for_fast_start():
    script = DEPLOY_SH.read_text()
    cargo_toml = (ROOT / "crates" / "spur-context-service" / "Cargo.toml").read_text()

    assert "worker-lambda" in cargo_toml
    assert "spur-context-worker-lambda" in cargo_toml
    assert 'run_graviton2_safe_cargo "worker Lambda image binary"' in script
    assert "--workdir crates/spur-context-service build --features worker-lambda --release" in script
    assert "WORKER_LAMBDA_ECR_REPO" in script
    assert "COPY spur-context-worker-lambda /usr/local/bin/spur-context-worker-lambda" in script
    assert 'ENTRYPOINT ["/usr/local/bin/spur-context-worker-lambda"]' in script
    assert '-var "worker_lambda_image=' in script


def test_deploy_builds_source_fetcher_lambda_image_and_passes_to_terraform():
    script = DEPLOY_SH.read_text()
    cargo_toml = (ROOT / "crates" / "spur-context-fetcher" / "Cargo.toml").read_text()
    outputs_tf = (INFRA_DIR / "outputs.tf").read_text()

    assert "spur-context-fetcher-lambda" in cargo_toml
    assert 'SOURCE_FETCHER_LAMBDA_ECR_REPO="spur-context-source-fetcher"' in script
    assert "SOURCE_FETCHER_LAMBDA_IMAGE_URI" in script
    assert 'run_graviton2_safe_cargo "source fetcher Lambda image binary"' in script
    assert "build -p spur-context-fetcher --release" in script
    assert "build_and_push_source_fetcher_lambda_image" in script
    assert 'aws ecr describe-repositories --repository-names "$SOURCE_FETCHER_LAMBDA_ECR_REPO"' in script
    assert "write_source_fetcher_lambda_image_dockerfile" in script
    assert "COPY spur-context-fetcher-lambda /usr/local/bin/spur-context-fetcher-lambda" in script
    assert "/usr/local/bin/spur-context-fetcher-lambda --smoke" in script
    assert 'ENTRYPOINT ["/usr/local/bin/spur-context-fetcher-lambda"]' in script
    assert "spur-context-source-fetcher-image.tar" in script
    assert '--remote-binary "$(remote_target_path target/release/spur-context-fetcher-lambda)"' in script
    assert '-var "source_fetcher_lambda_image=' in script
    assert "terraform output -raw source_fetcher_lambda_image_uri" in script
    assert "source fetcher Lambda image URI:" in script
    assert 'output "source_fetcher_lambda_image_uri"' in outputs_tf


def test_deploy_rebuilds_lambda_zip_by_default():
    script = DEPLOY_SH.read_text()
    main_tf = (INFRA_DIR / "main.tf").read_text()

    assert 'local tf_zip_path="../../target/lambda/spur-context-service.zip"' in script
    assert 'tf_zip_path="$local_zip"' in script
    assert 'tf_vars=(-var-file="$var_file" -var "lambda_zip_path=$tf_zip_path")' in script
    assert 'elif [[ ! -f "$zip_path" ]]' not in script
    assert 'rm -f "$zip_path"' in script
    lambda_zip = main_tf.split('resource "aws_s3_object" "lambda_zip"', 1)[1]
    assert "source_hash = filemd5(var.lambda_zip_path)" in lambda_zip
    assert "etag   = filemd5(var.lambda_zip_path)" not in lambda_zip


def test_deploy_can_package_lambda_without_terraform_apply():
    script = DEPLOY_SH.read_text()

    assert "./deploy.sh --skip-worker --package-only" in script
    assert "package_only=false" in script
    assert "--package-only) package_only=true" in script
    assert 'if [[ "$package_only" == "true" ]]' in script
    assert script.index('if [[ "$package_only" == "true" ]]') < script.index("terraform init")


def test_terraform_uses_partial_s3_backend_and_environment_files():
    versions_tf = VERSIONS_TF.read_text()

    assert 'backend "s3" {}' in versions_tf

    for environment in ("staging", "prod"):
        backend_config = INFRA_DIR / "backends" / f"{environment}.s3.tfbackend"
        var_file = INFRA_DIR / "env" / f"{environment}.tfvars"

        assert backend_config.exists()
        backend_text = backend_config.read_text()
        assert "bucket" in backend_text
        assert "key" in backend_text
        assert "region" in backend_text
        assert "dynamodb_table" in backend_text

        assert var_file.exists()
        assert "vpc_id" in var_file.read_text()


def test_deploy_passes_backend_config_and_var_file_to_terraform():
    script = DEPLOY_SH.read_text()

    assert 'local environment="staging"' in script
    assert "--env)" in script
    assert "--backend-config|-backend-config)" in script
    assert "--var-file|-var-file)" in script
    assert 'backend_config="${backend_config:-backends/${environment}.s3.tfbackend}"' in script
    assert 'var_file="${var_file:-env/${environment}.tfvars}"' in script
    assert 'terraform init -upgrade -backend-config="$backend_config"' in script
    assert 'tf_vars=(-var-file="$var_file" -var "lambda_zip_path=$tf_zip_path")' in script
    assert 'terraform plan "${tf_vars[@]}"' in script
    assert 'terraform apply "${tf_vars[@]}" -auto-approve' in script


def test_remote_worker_image_script_delegates_to_canonical_deploy_path():
    script = REMOTE_BUILD_SH.read_text()

    assert 'SPUR_CONTEXT_SERVICE_BUILD_MODE="${SPUR_CONTEXT_SERVICE_BUILD_MODE:-remote}"' in script
    assert "export SPUR_CONTEXT_SERVICE_BUILD_MODE" in script
    assert 'exec "$SCRIPT_DIR/deploy.sh"' in script
    assert "COPY spur-context-worker /usr/local/bin/spur-context-worker" not in script
    assert "COPY spur /usr/local/bin/spur" not in script
    assert "docker build" not in script


def test_remote_docker_build_accepts_multiple_remote_binaries():
    script = (ROOT / "scripts" / "cloud-build" / "docker-build.sh").read_text()

    assert "REMOTE_BINARIES=()" in script
    assert 'REMOTE_BINARIES+=("$2")' in script
    assert 'for remote_binary in "${REMOTE_BINARIES[@]}"' in script
    assert "docker build --platform linux/arm64 --provenance=false" in script


def test_context_service_workflow_runs_tests_and_gated_aws_artifacts():
    workflow = CONTEXT_SERVICE_WORKFLOW.read_text()

    assert "workflow_dispatch:" in workflow
    assert "build_aws_artifacts:" in workflow
    assert "run_staging_smoke:" in workflow
    assert "scripts/spur-cargo --workdir crates/spur-context-service test --all-features" in workflow
    assert 'SPUR_CONTEXT_SERVICE_BUILD_MODE: "self-contained"' in workflow
    assert 'SPUR_CONTEXT_SERVICE_PUSH_IMAGES: "0"' in workflow
    assert 'SPUR_REMOTE: "1"' not in workflow.split("build-aws-artifacts:", 1)[1].split("staging-smoke:", 1)[0]
    assert "docker/setup-qemu-action@v3" in workflow
    assert "docker/setup-buildx-action@v3" in workflow
    assert "infra/spur-context-service/build-and-push-remote.sh" in workflow
    assert "infra/spur-context-service/deploy.sh --skip-worker --package-only" in workflow
    assert "spur-context-service-worker-images" in workflow
    assert "target/lambda/*worker*image.tar" in workflow
    assert "infra/spur-context-service/smoke-staging-e2e.py" in workflow
    assert "CONTEXT_SERVICE_AWS_ROLE_ARN" in workflow
    assert "context-service-staging" in workflow
    assert "terraform apply" not in workflow


def test_staging_smoke_codifies_e1_real_worker_and_frozen_serving():
    script = STAGING_SMOKE.read_text()
    readme = (INFRA_DIR / "README.md").read_text()

    assert "argparse" in script
    assert "--preflight" in script
    assert "run_preflight" in script
    assert "external_index" in script
    assert "external_index_status" in script
    assert "external_code_search" in script
    assert "external_code_read" in script
    assert "external_knowledge_context" in script
    assert "symbol_embeddings" in script
    assert "bronze/{source}/{package}/{revision}/source.tar.gz" in script
    assert "silver/{source}/{package}/{revision}/{builder_version}/manifest.json" in script
    assert "gold/catalog-snapshot/current.json" in script
    assert "get-function-configuration" in script
    assert "sts" in script
    assert "get-caller-identity" in script
    assert "SPUR_CATALOG_DSN" in script
    assert "postgres" in script
    assert "aws lambda invoke" in script
    assert "aws s3 presign" in script
    assert "prefetch_source=false" in script
    assert "SPUR_CONTEXT_SMOKE_SOURCE_BUCKET" in readme
    assert "smoke-staging-e2e.sh --preflight" in readme
    assert "smoke-staging-e2e.sh" in readme


def test_staging_smoke_codifies_github_fetch_source_path():
    script = STAGING_SMOKE.read_text()
    readme = (INFRA_DIR / "README.md").read_text()

    assert "--github-source" in script
    assert "SPUR_CONTEXT_SMOKE_GITHUB_URL" in script
    assert "git+https://github.com/" in script
    assert '"source_kind": "git"' in script
    assert "assert_stepfunctions_visited_state" in script
    assert "get-execution-history" in script
    assert "FetchSource" in script
    assert "SPUR_CONTEXT_SMOKE_GITHUB_SYMBOL_QUERY" in script
    assert "smoke-staging-e2e.sh --github-source" in readme
    assert "FetchSource" in readme
    assert "presigned HTTPS" in readme


def test_staging_smoke_entrypoint_runs_python_script():
    script = STAGING_SMOKE_ENTRYPOINT.read_text()

    assert "set -euo pipefail" in script
    assert 'exec python3 "$SCRIPT_DIR/smoke-staging-e2e.py" "$@"' in script


def test_staging_smoke_preflight_does_not_ingest():
    script = STAGING_SMOKE.read_text()

    preflight = script.split("def run_preflight", 1)[1].split("def ", 1)[0]
    assert "caller_identity_arn" in preflight
    assert "verify_serving_uses_frozen_s3_catalog" in preflight
    assert "upload_fixture_source" not in preflight
    assert "presign_fixture_source" not in preflight
    assert "external_index" not in preflight
    assert "LambdaInvoker" not in preflight


def test_worker_checkpoint_uri_is_per_job_object_from_state_machine():
    ecs_tf = (INFRA_DIR / "ecs.tf").read_text()
    state_machine_tf = (INFRA_DIR / "state_machine.tf").read_text()
    variables_tf = (INFRA_DIR / "variables.tf").read_text()
    asl = (INFRA_DIR / "index_build_asl.json").read_text()

    assert "SPUR_CONTEXT_WORKER_CHECKPOINT_URI" not in ecs_tf
    assert "worker_checkpoint_uri_template" in state_machine_tf
    assert '"Name": "SPUR_CONTEXT_WORKER_CHECKPOINT_URI"' in asl
    assert (
        '"Value.$": "States.Format('
        "'${worker_checkpoint_uri_template}', $.workerInput.job_id"
        ')"'
    ) in asl
    assert "/jobs/{}/checkpoint.json" in variables_tf


def test_state_machine_does_not_retry_worker_reported_failures():
    asl = (INFRA_DIR / "index_build_asl.json").read_text()

    assert '"States.TaskFailed"' not in asl
    assert '"States.ALL"' not in asl


def test_state_machine_invokes_lambda_worker_before_ecs_fallback():
    asl = (INFRA_DIR / "index_build_asl.json").read_text()
    rendered = render_index_build_asl()
    state_machine_tf = (INFRA_DIR / "state_machine.tf").read_text()
    iam_tf = (INFRA_DIR / "iam.tf").read_text()

    assert rendered["StartAt"] == "RouteSource"
    assert '"StartAt": "RouteSource"' in asl
    assert rendered["States"]["RouteSource"]["Default"] == "PrepareOriginalWorkerInput"
    assert (
        rendered["States"]["RouteSource"]["Choices"][0]["Variable"]
        == "$.prefetch_source"
    )
    assert rendered["States"]["RouteSource"]["Choices"][0]["BooleanEquals"] is True
    assert rendered["States"]["RouteSource"]["Choices"][0]["Next"] == "FetchSource"
    assert rendered["States"]["PrepareOriginalWorkerInput"]["Next"] == "RunLambdaBuild"
    assert '"Resource": "arn:aws:states:::lambda:invoke"' in asl
    assert '"FunctionName": "${worker_lambda_arn}"' in asl
    assert '"Next": "CheckLambdaBuild"' in asl
    assert '"Next": "RunBuild"' in asl
    assert '"ErrorEquals": ["States.Timeout"' in asl
    assert '"Lambda.Unknown"' in asl
    assert '"Sandbox.Timedout"' in asl
    assert "worker_lambda_arn" in state_machine_tf
    assert "source_fetch_lambda_arn" in state_machine_tf
    assert 'Action = ["lambda:InvokeFunction"]' in iam_tf


def test_state_machine_fetches_source_and_normalizes_worker_input():
    rendered = render_index_build_asl()
    states = rendered["States"]

    fetch_source = states["FetchSource"]
    assert fetch_source["Resource"] == "arn:aws:states:::lambda:invoke"
    assert (
        fetch_source["Parameters"]["FunctionName"]
        == "arn:aws:lambda:ap-southeast-5:123456789012:"
        "function:spur-context-source-fetcher:live"
    )
    assert fetch_source["ResultPath"] == "$.fetchResult"
    # No Catch: a deterministic fetcher failure (it throws) fails the execution
    # and does NOT route to the VPC worker. The fetcher's success payload has no
    # `status` field, so FetchSource goes straight to PrepareFetchedWorkerInput
    # (a Choice on a missing path would be a runtime error).
    assert "Catch" not in fetch_source
    assert fetch_source["Next"] == "PrepareFetchedWorkerInput"
    assert fetch_source["Retry"][0]["ErrorEquals"] == [
        "Lambda.ServiceException",
        "Lambda.AWSLambdaException",
        "Lambda.SdkClientException",
        "Lambda.TooManyRequestsException",
    ]
    assert "CheckFetchSource" not in states
    assert "FetchSourceFailed" not in states

    original_worker_input = states["PrepareOriginalWorkerInput"]["Parameters"]
    assert original_worker_input["source_url.$"] == "$.source_url"
    assert original_worker_input["source_kind.$"] == "$.source_kind"

    fetched_worker_input = states["PrepareFetchedWorkerInput"]["Parameters"]
    assert fetched_worker_input["job_id.$"] == "$.job_id"
    assert fetched_worker_input["source_url.$"] == "$.fetchResult.Payload.source_url"
    assert fetched_worker_input["source_kind.$"] == "$.fetchResult.Payload.source_kind"
    assert states["PrepareFetchedWorkerInput"]["ResultPath"] == "$.workerInput"

    lambda_payload = states["RunLambdaBuild"]["Parameters"]["Payload"]
    assert lambda_payload["source_url.$"] == "$.workerInput.source_url"
    assert lambda_payload["source_kind.$"] == "$.workerInput.source_kind"

    for state_name in ("RunBuild", "FallbackBuild"):
        env = {
            item["Name"]: item
            for item in states[state_name]["Parameters"]["Overrides"][
                "ContainerOverrides"
            ][0]["Environment"]
        }
        assert env["SOURCE_URL"]["Value.$"] == "$.workerInput.source_url"
        assert env["SOURCE_KIND"]["Value.$"] == "$.workerInput.source_kind"


def test_source_fetcher_lambda_is_non_vpc_and_least_privilege():
    source_fetcher_tf = (INFRA_DIR / "source_fetcher_lambda.tf").read_text()
    variables_tf = (INFRA_DIR / "variables.tf").read_text()
    state_machine_tf = (INFRA_DIR / "state_machine.tf").read_text()
    iam_tf = (INFRA_DIR / "iam.tf").read_text()
    main_tf = (INFRA_DIR / "main.tf").read_text()

    assert 'resource "aws_lambda_function" "source_fetcher"' in source_fetcher_tf
    assert 'resource "aws_lambda_alias" "source_fetcher_live"' in source_fetcher_tf
    assert 'resource "aws_cloudwatch_log_group" "source_fetcher_lambda"' in source_fetcher_tf
    assert "image_uri     = var.source_fetcher_lambda_image" in source_fetcher_tf
    assert "timeout       = var.source_fetcher_lambda_timeout_sec" in source_fetcher_tf
    assert "memory_size   = var.source_fetcher_lambda_memory_mb" in source_fetcher_tf
    assert "source_fetcher_lambda_ephemeral_storage_mb" in source_fetcher_tf
    assert "vpc_config" not in source_fetcher_tf
    for env_name in (
        "SPUR_CONTEXT_FETCH_BUCKET",
        "SPUR_CONTEXT_FETCH_PREFIX",
        "SPUR_CONTEXT_MAX_TARBALL_BYTES",
        "SPUR_CONTEXT_MAX_GIT_BYTES",
        "SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS",
        "SPUR_CONTEXT_FETCH_PRESIGN_SECONDS",
    ):
        assert env_name in source_fetcher_tf

    for variable_name in (
        "source_fetcher_lambda_image",
        "source_fetcher_lambda_timeout_sec",
        "source_fetcher_lambda_memory_mb",
        "source_fetcher_lambda_ephemeral_storage_mb",
        "source_fetch_presign_seconds",
        "fetch_artifact_retention_days",
    ):
        assert f'variable "{variable_name}"' in variables_tf
    assert "default     = 900" in variables_tf
    assert "default     = 1024" in variables_tf
    assert "default     = 10240" in variables_tf
    assert "default     = 21600" in variables_tf
    assert "default     = 7" in variables_tf

    assert "source_fetch_lambda_arn" in state_machine_tf
    assert "aws_lambda_alias.source_fetcher_live.arn" in state_machine_tf

    assert 'resource "aws_iam_role" "source_fetcher_lambda"' in iam_tf
    source_fetcher_policy = iam_tf.split(
        'resource "aws_iam_role_policy" "source_fetcher_lambda"', 1
    )[1].split('resource "aws_iam_role_policy" "lambda_catalog_secret"', 1)[0]
    assert '"logs:CreateLogStream"' in source_fetcher_policy
    assert '"logs:PutLogEvents"' in source_fetcher_policy
    assert '"s3:PutObject"' in source_fetcher_policy
    assert '"s3:GetObject"' in source_fetcher_policy
    assert '"s3:AbortMultipartUpload"' in source_fetcher_policy
    assert '"s3:ListBucket"' in source_fetcher_policy
    assert '"${aws_s3_bucket.data.arn}/fetch/*"' in source_fetcher_policy
    assert '"s3:prefix"' in source_fetcher_policy
    for forbidden in (
        "secretsmanager:",
        "dynamodb:",
        "states:",
        "ec2:",
        "AWSLambdaVPCAccessExecutionRole",
    ):
        assert forbidden not in source_fetcher_policy

    assert 'resource "aws_iam_role_policy" "sfn_source_fetcher_lambda"' in iam_tf
    sfn_fetcher_policy = iam_tf.split(
        'resource "aws_iam_role_policy" "sfn_source_fetcher_lambda"', 1
    )[1]
    assert "aws_lambda_function.source_fetcher.arn" in sfn_fetcher_policy
    assert "aws_lambda_alias.source_fetcher_live.arn" in sfn_fetcher_policy

    assert 'resource "aws_s3_bucket_lifecycle_configuration" "data"' in main_tf
    assert 'prefix = "fetch/"' in main_tf
    assert "days = var.fetch_artifact_retention_days" in main_tf
    assert "noncurrent_version_expiration" in main_tf


def test_tfvars_example_documents_source_fetcher_image_and_tuning_knobs():
    tfvars = (INFRA_DIR / "terraform.tfvars.example").read_text()

    assert "source_fetcher_lambda_image" in tfvars
    assert "source_fetcher_lambda_timeout_sec" in tfvars
    assert "source_fetcher_lambda_memory_mb" in tfvars
    assert "source_fetcher_lambda_ephemeral_storage_mb" in tfvars
    assert "source_fetch_presign_seconds" in tfvars
    assert "fetch_artifact_retention_days" in tfvars


def test_lambda_worker_resource_is_configured_for_fast_start_mvp():
    lambda_tf = (INFRA_DIR / "lambda_worker.tf").read_text()
    variables_tf = (INFRA_DIR / "variables.tf").read_text()
    outputs_tf = (INFRA_DIR / "outputs.tf").read_text()
    iam_tf = (INFRA_DIR / "iam.tf").read_text()

    assert 'resource "aws_lambda_function" "worker"' in lambda_tf
    assert 'package_type  = "Image"' in lambda_tf
    assert "image_uri     = var.worker_lambda_image" in lambda_tf
    assert "timeout       = var.worker_lambda_timeout_sec" in lambda_tf
    assert "memory_size   = var.worker_lambda_memory_mb" in lambda_tf
    assert "ephemeral_storage" in lambda_tf
    assert "AWS_REGION" not in lambda_tf
    assert "worker_lambda_memory_mb" in variables_tf
    assert "default     = 3008" in variables_tf
    assert "worker_lambda_ephemeral_storage_mb" in variables_tf
    assert 'output "worker_image_uri"' in outputs_tf
    assert 'output "worker_lambda_image_uri"' in outputs_tf
    assert 'output "worker_lambda_function_name"' in outputs_tf
    lambda_s3_policy = iam_tf.split('resource "aws_iam_role_policy" "s3_access"', 1)[1]
    assert '"s3:DeleteObject"' in lambda_s3_policy


def test_nat_free_worker_vpc_endpoints_are_declared():
    variables_tf = (INFRA_DIR / "variables.tf").read_text()
    state_machine_tf = (INFRA_DIR / "state_machine.tf").read_text()
    endpoints_tf = (INFRA_DIR / "vpc_endpoints.tf").read_text()
    outputs_tf = (INFRA_DIR / "outputs.tf").read_text()

    assert 'variable "create_vpc_endpoints"' in variables_tf
    assert "default     = true" in variables_tf
    assert 'variable "worker_route_table_ids"' in variables_tf
    assert 'variable "vpc_endpoint_region"' in variables_tf

    for service in (
        "s3",
        "dynamodb",
        "states",
        "secretsmanager",
        "ecr.api",
        "ecr.dkr",
        "logs",
        "sts",
    ):
        assert f'com.amazonaws.${{local.vpc_endpoint_region}}.{service}' in endpoints_tf

    gateway_endpoint = endpoints_tf.split('resource "aws_vpc_endpoint" "gateway"', 1)[1]
    assert 'vpc_endpoint_type = "Gateway"' in gateway_endpoint
    assert "route_table_ids   = var.worker_route_table_ids" in gateway_endpoint

    interface_endpoint = endpoints_tf.split('resource "aws_vpc_endpoint" "interface"', 1)[1]
    assert 'vpc_endpoint_type   = "Interface"' in interface_endpoint
    assert "subnet_ids" in interface_endpoint
    assert "= var.worker_subnets" in interface_endpoint
    assert "private_dns_enabled = true" in interface_endpoint
    assert "security_group_ids  = [aws_security_group.vpc_endpoints[0].id]" in interface_endpoint

    endpoint_sg = state_machine_tf.split('resource "aws_security_group" "vpc_endpoints"', 1)[1]
    assert "count = var.create_vpc_endpoints ? 1 : 0" in endpoint_sg
    assert "from_port       = 443" in endpoint_sg
    assert "to_port         = 443" in endpoint_sg
    assert 'protocol        = "tcp"' in endpoint_sg
    assert "security_groups = [aws_security_group.worker.id]" in endpoint_sg

    assert 'output "gateway_vpc_endpoint_ids"' in outputs_tf
    assert 'output "interface_vpc_endpoint_ids"' in outputs_tf


def test_catalog_lease_ttl_uses_worker_expiry_field():
    main_tf = (INFRA_DIR / "main.tf").read_text()

    catalog_leases = main_tf.split('resource "aws_dynamodb_table" "catalog_leases"', 1)[1]

    assert 'attribute_name = "expires_at_unix_secs"' in catalog_leases
