import json
import os
import shlex
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
GCLOUD_AUTH = ROOT / "scripts" / "gcloud_auth"


def write_executable(path: Path, body: str) -> None:
    path.write_text(body)
    path.chmod(0o755)


def test_gcloud_auth_token_uses_impersonated_service_account(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gcloud_log = tmp_path / "gcloud.log"
    write_executable(
        bin_dir / "gcloud",
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$*\" > {shlex.quote(str(gcloud_log))}",
                "printf 'test-access-token\\n'",
            ]
        )
        + "\n",
    )

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"
    env["GCP_PROJECT"] = "wiilearn"
    env["SPUR_SCCACHE_SERVICE_ACCOUNT"] = "spur-sccache-local@wiilearn.iam.gserviceaccount.com"

    result = subprocess.run(
        [str(GCLOUD_AUTH), "token"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["accessToken"] == "test-access-token"
    assert "expireTime" in payload
    assert gcloud_log.read_text().strip() == (
        "auth print-access-token "
        "--impersonate-service-account=spur-sccache-local@wiilearn.iam.gserviceaccount.com"
    )


def test_gcloud_auth_token_reports_reauth_without_traceback(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    write_executable(
        bin_dir / "gcloud",
        "\n".join(
            [
                "#!/bin/sh",
                "printf 'reauth required\\n' >&2",
                "exit 1",
            ]
        )
        + "\n",
    )

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"

    result = subprocess.run(
        [str(GCLOUD_AUTH), "token"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 1
    assert "reauth required" in result.stderr
    assert "run scripts/gcloud_auth auth" in result.stderr
    assert "Traceback" not in result.stderr


def test_gcloud_auth_start_sccache_uses_local_disk_then_gcs(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    sccache_log = tmp_path / "sccache.log"
    write_executable(
        bin_dir / "sccache",
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'arg=%s\\n' \"$1\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'dir=%s\\n' \"$SCCACHE_DIR\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'size=%s\\n' \"$SCCACHE_CACHE_SIZE\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'chain=%s\\n' \"$SCCACHE_MULTILEVEL_CHAIN\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'bucket=%s\\n' \"$SCCACHE_GCS_BUCKET\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'rw=%s\\n' \"$SCCACHE_GCS_RW_MODE\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'url=%s\\n' \"$SCCACHE_GCS_CREDENTIALS_URL\" >> {shlex.quote(str(sccache_log))}",
                "exit 0",
            ]
        )
        + "\n",
    )

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"
    env["GCP_PROJECT"] = "wiilearn"
    env["SCCACHE_BUCKET"] = "spur-test-sccache"
    env["SPUR_SCCACHE_GCS_CREDENTIALS_URL"] = "http://127.0.0.1:9999/token"
    env["SPUR_SCCACHE_DIR"] = str(tmp_path / "cache")
    env["SPUR_SCCACHE_CACHE_SIZE"] = "123M"
    env["SPUR_SCCACHE_GCS_RW_MODE"] = "READ_ONLY"

    result = subprocess.run(
        [str(GCLOUD_AUTH), "start-sccache"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        "arg=--stop-server",
        f"dir={tmp_path / 'cache'}",
        "size=123M",
        "chain=disk,gcs",
        "bucket=spur-test-sccache",
        "rw=READ_ONLY",
        "url=http://127.0.0.1:9999/token",
        "arg=--start-server",
        f"dir={tmp_path / 'cache'}",
        "size=123M",
        "chain=disk,gcs",
        "bucket=spur-test-sccache",
        "rw=READ_ONLY",
        "url=http://127.0.0.1:9999/token",
    ]
