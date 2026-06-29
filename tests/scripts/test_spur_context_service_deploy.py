from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"
REMOTE_BUILD_SH = ROOT / "infra" / "spur-context-service" / "build-and-push-remote.sh"
INFRA_DIR = ROOT / "infra" / "spur-context-service"
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
    assert 'if [[ "$PUSH_IMAGES" == "true" ]]' in script
    assert "build_and_push_worker_image" in script
    assert "build_and_push_worker_lambda_image" in script
    assert 'aws ecr describe-repositories --repository-names "$WORKER_ECR_REPO"' in script
    assert 'aws ecr describe-repositories --repository-names "$WORKER_LAMBDA_ECR_REPO"' in script


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
    assert 'tf_vars=(-var "lambda_zip_path=$tf_zip_path")' in script
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
