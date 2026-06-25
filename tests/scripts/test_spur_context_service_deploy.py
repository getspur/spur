from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"


def test_deploy_builds_standalone_context_service_from_crate_workdir():
    script = DEPLOY_SH.read_text()

    assert "scripts/spur-cargo --workdir crates/spur-context-service build --features lambda --release" in script
    assert "scripts/spur-cargo --workdir crates/spur-context-service build --features worker --release" in script
    assert (
        'fetch_remote_worktree_file '
        'crates/spur-context-service/target/release/spur-context-service '
        '"$BUILD_DIR/bootstrap"'
    ) in script
    assert (
        '--remote-binary "$(remote_worktree_path '
        'crates/spur-context-service/target/release/spur-context-worker)"'
    ) in script
    assert 'worker_image_uri="$(build_and_push_worker_image)"' not in script
    assert "scripts/spur-cargo run --workdir crates/spur-context-service" not in script
    assert "scripts/spur-cargo build -p spur-context-service" not in script
