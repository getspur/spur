from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"
REMOTE_BUILD_SH = ROOT / "infra" / "spur-context-service" / "build-and-push-remote.sh"
INFRA_DIR = ROOT / "infra" / "spur-context-service"


def test_deploy_builds_standalone_context_service_from_crate_workdir():
    script = DEPLOY_SH.read_text()

    assert "scripts/spur-cargo --workdir crates/spur-context-service build --features lambda --release" in script
    assert "scripts/spur-cargo --workdir crates/spur-context-service build --features worker --release" in script
    assert "scripts/spur-cargo build -p spur-cli --release" in script
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
    assert "scripts/spur-cargo --workdir crates/spur-context-service build --features worker-lambda --release" in script
    assert "WORKER_LAMBDA_ECR_REPO" in script
    assert "COPY spur-context-worker-lambda /usr/local/bin/spur-context-worker-lambda" in script
    assert 'ENTRYPOINT ["/usr/local/bin/spur-context-worker-lambda"]' in script
    assert '-var "worker_lambda_image=' in script


def test_deploy_rebuilds_lambda_zip_by_default():
    script = DEPLOY_SH.read_text()
    main_tf = (INFRA_DIR / "main.tf").read_text()

    assert 'elif [[ ! -f "$zip_path" ]]' not in script
    assert 'rm -f "$zip_path"' in script
    lambda_zip = main_tf.split('resource "aws_s3_object" "lambda_zip"', 1)[1]
    assert "source_hash = filemd5(var.lambda_zip_path)" in lambda_zip
    assert "etag   = filemd5(var.lambda_zip_path)" not in lambda_zip


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
