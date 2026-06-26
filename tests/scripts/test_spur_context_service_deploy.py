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


def test_deploy_rebuilds_lambda_zip_by_default():
    script = DEPLOY_SH.read_text()

    assert 'elif [[ ! -f "$zip_path" ]]' not in script
    assert 'rm -f "$zip_path"' in script


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


def test_catalog_lease_ttl_uses_worker_expiry_field():
    main_tf = (INFRA_DIR / "main.tf").read_text()

    catalog_leases = main_tf.split('resource "aws_dynamodb_table" "catalog_leases"', 1)[1]

    assert 'attribute_name = "expires_at_unix_secs"' in catalog_leases
