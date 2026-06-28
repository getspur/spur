import os
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPUR_CARGO = ROOT / "scripts" / "spur-cargo"


def write_executable(path: Path, body: str) -> None:
    path.write_text(body)
    path.chmod(0o755)


def base_env(tmp_path: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "PATH": f"{tmp_path}{os.pathsep}{env['PATH']}",
            "SPUR_SCCACHE_S3": "0",
            "SPUR_SCCACHE_GCS": "0",
            "RUSTC_WRAPPER": "",
        }
    )
    return env


def test_spur_cargo_workdir_changes_local_cargo_directory(tmp_path: Path):
    record = tmp_path / "cargo-record"
    write_executable(
        tmp_path / "cargo",
        f"""#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s\\n' "$PWD" > {record}
printf 'args=%s\\n' "$*" >> {record}
""",
    )
    env = base_env(tmp_path)
    env["SPUR_REMOTE"] = "0"

    subprocess.run(
        [
            str(SPUR_CARGO),
            "--workdir",
            "crates/spur-context-service",
            "test",
            "--",
            "--ignored",
        ],
        cwd=ROOT,
        env=env,
        check=True,
    )

    assert record.read_text().splitlines() == [
        f"pwd={ROOT / 'crates' / 'spur-context-service'}",
        "args=test -- --ignored",
    ]


def test_spur_cargo_workdir_changes_remote_build_directory(tmp_path: Path):
    record = tmp_path / "remote-record"
    remote = tmp_path / "remote-build.sh"
    write_executable(
        remote,
        f"""#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s\\n' "$PWD" > {record}
printf 'args=%s\\n' "$*" >> {record}
""",
    )
    env = base_env(tmp_path)
    env.update(
        {
            "SPUR_REMOTE": "1",
            "SPUR_CLOUD_BUILD_SH": str(remote),
            "SPUR_CLOUD": "test-cloud",
            "SPUR_CLOUD_FALLBACK": "",
        }
    )

    subprocess.run(
        [str(SPUR_CARGO), "--workdir=crates/spur-context-service", "test"],
        cwd=ROOT,
        env=env,
        check=True,
    )

    assert record.read_text().splitlines() == [
        f"pwd={ROOT / 'crates' / 'spur-context-service'}",
        "args=--auto-spin -- test",
    ]


def test_spur_cargo_graph_embed_routes_through_remote_graph_build(tmp_path: Path):
    record = tmp_path / "remote-record"
    remote = tmp_path / "remote-build.sh"
    write_executable(
        remote,
        f"""#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s\\n' "$PWD" > {record}
printf 'args=%s\\n' "$*" >> {record}
""",
    )
    write_executable(
        tmp_path / "cargo",
        """#!/usr/bin/env bash
exit 99
""",
    )
    env = base_env(tmp_path)
    env.update(
        {
            "SPUR_CLOUD_BUILD_SH": str(remote),
            "SPUR_CLOUD": "test-cloud",
            "SPUR_CLOUD_FALLBACK": "",
        }
    )

    subprocess.run(
        [str(SPUR_CARGO), "graph-embed", "--quiet"],
        cwd=ROOT,
        env=env,
        check=True,
    )

    assert record.read_text().splitlines() == [
        f"pwd={ROOT}",
        "args=--auto-spin -- run -p spur-cli -- graph build --workspace --quiet",
    ]


def test_remote_build_helpers_run_cargo_from_invocation_directory():
    for relative_path in [
        "scripts/cloud-build/build.sh",
        "scripts/gcp-build/build.sh",
    ]:
        script = (ROOT / relative_path).read_text()

        assert "LOCAL_CWD=$(pwd -P)" in script
        assert "WORKDIR_REL=" in script
        assert "remote_workdir_rel=$WORKDIR_REL_ESCAPED" in script
        assert 'cd \\"\\$remote_workdir_rel\\"' in script
