from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"
REMOTE_BUILD_SH = ROOT / "infra" / "spur-context-service" / "build-and-push-remote.sh"
INFRA_DIR = ROOT / "infra" / "spur-context-service"
VERSIONS_TF = INFRA_DIR / "versions.tf"
CONTEXT_SERVICE_WORKFLOW = ROOT / ".github" / "workflows" / "context-service.yml"
STAGING_SMOKE = INFRA_DIR / "smoke-staging-e2e.py"


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
    assert "--context-dir \"$worker_context\"" not in script
    assert 'worker_image_uri="$(build_and_push_worker_image)"' not in script
    assert "scripts/spur-cargo run --workdir crates/spur-context-service" not in script
    assert "scripts/spur-cargo build -p spur-context-service" not in script


def test_deploy_worker_image_contains_worker_and_spur_binaries():
    deploy_sh = DEPLOY_SH.read_text()

    assert "COPY spur-context-worker /usr/local/bin/spur-context-worker" in deploy_sh
    assert "COPY spur /usr/local/bin/spur" in deploy_sh
    assert "spur --version" in deploy_sh
    assert "spur-context-worker" in deploy_sh


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
    assert "infra/spur-context-service/build-and-push-remote.sh" in workflow
    assert "infra/spur-context-service/deploy.sh --skip-worker --package-only" in workflow
    assert "infra/spur-context-service/smoke-staging-e2e.py" in workflow
    assert "CONTEXT_SERVICE_AWS_ROLE_ARN" in workflow
    assert "context-service-staging" in workflow
    assert "terraform apply" not in workflow


def test_staging_smoke_codifies_e1_real_worker_and_frozen_serving():
    script = STAGING_SMOKE.read_text()
    readme = (INFRA_DIR / "README.md").read_text()

    assert "external_index" in script
    assert "external_index_status" in script
    assert "external_code_search" in script
    assert "external_code_read" in script
    assert "external_knowledge_context" in script
    assert "symbol_embeddings" in script
    assert "bronze/{source}/{package}/{revision}/source.tar.gz" in script
    assert "silver/{source}/{package}/{revision}/" in script
    assert "gold/catalog-snapshot/current.json" in script
    assert "get-function-configuration" in script
    assert "SPUR_CATALOG_DSN" in script
    assert "postgres" in script
    assert "aws lambda invoke" in script
    assert "aws s3 presign" in script
    assert "SPUR_CONTEXT_SMOKE_SOURCE_BUCKET" in readme
    assert "smoke-staging-e2e.py" in readme


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
        "'${worker_checkpoint_uri_template}', $.job_id"
        ')"'
    ) in asl
    assert "/jobs/{}/checkpoint.json" in variables_tf


def test_state_machine_does_not_retry_worker_reported_failures():
    asl = (INFRA_DIR / "index_build_asl.json").read_text()

    assert '"States.TaskFailed"' not in asl
    assert '"States.ALL"' not in asl


def test_state_machine_invokes_lambda_worker_before_ecs_fallback():
    asl = (INFRA_DIR / "index_build_asl.json").read_text()
    state_machine_tf = (INFRA_DIR / "state_machine.tf").read_text()
    iam_tf = (INFRA_DIR / "iam.tf").read_text()

    assert '"StartAt": "RunLambdaBuild"' in asl
    assert '"Resource": "arn:aws:states:::lambda:invoke"' in asl
    assert '"FunctionName": "${worker_lambda_arn}"' in asl
    assert '"Next": "CheckLambdaBuild"' in asl
    assert '"Next": "RunBuild"' in asl
    assert '"ErrorEquals": ["States.Timeout"' in asl
    assert '"Lambda.Unknown"' in asl
    assert '"Sandbox.Timedout"' in asl
    assert "worker_lambda_arn" in state_machine_tf
    assert 'Action = ["lambda:InvokeFunction"]' in iam_tf


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


def test_catalog_lease_ttl_uses_worker_expiry_field():
    main_tf = (INFRA_DIR / "main.tf").read_text()

    catalog_leases = main_tf.split('resource "aws_dynamodb_table" "catalog_leases"', 1)[1]

    assert 'attribute_name = "expires_at_unix_secs"' in catalog_leases
