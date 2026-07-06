import os
import shlex
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT / "scripts" / "sccache-worktree.sh"
SPUR_CARGO = ROOT / "scripts" / "spur-cargo"


def make_isolated_bin(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()

    bash = shutil.which("bash")
    assert bash is not None
    (bin_dir / "bash").symlink_to(bash)

    return bin_dir


def test_sccache_worktree_falls_back_to_rustc_when_sccache_is_missing(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    rustc_log = tmp_path / "rustc.log"
    rustc = tmp_path / "rustc"
    rustc.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$0\" \"$@\" > {shlex.quote(str(rustc_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(rustc_log))}",
            ]
        )
        + "\n"
    )
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert rustc_log.read_text().splitlines() == [
        str(rustc),
        "-vV",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_uses_sccache_when_available(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)
    worktree_root = tmp_path / "worktree"
    worktree_root.mkdir()

    git = bin_dir / "git"
    git.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                'if [ "$1" = "rev-parse" ] && [ "$2" = "--show-toplevel" ]; then',
                f"  printf '%s\\n' {shlex.quote(str(worktree_root))}",
                "  exit 0",
                "fi",
                "exit 1",
            ]
        )
        + "\n"
    )
    git.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$@\" > {shlex.quote(str(sccache_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        str(rustc),
        "-vV",
        f"basedirs={worktree_root}:{ROOT}",
    ]


def test_sccache_worktree_enables_gcs_cache_on_darwin_when_requested(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    uname = bin_dir / "uname"
    uname.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
    uname.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'bucket=%s\\n' \"${{SCCACHE_GCS_BUCKET-}}\" > {shlex.quote(str(sccache_log))}",
                f"printf 'rw=%s\\n' \"${{SCCACHE_GCS_RW_MODE-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'chain=%s\\n' \"${{SCCACHE_MULTILEVEL_CHAIN-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env["SPUR_SCCACHE_GCS"] = "1"
    env["SCCACHE_BUCKET"] = "spur-test-sccache"
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        "bucket=spur-test-sccache",
        "rw=READ_WRITE",
        "chain=disk,gcs",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_enables_s3_cache_by_default(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    uname = bin_dir / "uname"
    uname.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
    uname.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'bucket=%s\\n' \"${{SCCACHE_BUCKET-}}\" > {shlex.quote(str(sccache_log))}",
                f"printf 'region=%s\\n' \"${{SCCACHE_REGION-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'chain=%s\\n' \"${{SCCACHE_MULTILEVEL_CHAIN-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'dir=%s\\n' \"${{SCCACHE_DIR-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    home = tmp_path / "home"
    home.mkdir()

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["HOME"] = str(home)
    env["SPUR_ROOT"] = str(ROOT)
    env.pop("SPUR_SCCACHE_S3", None)
    env.pop("SPUR_SCCACHE_GCS", None)
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        "bucket=wiilearn-spur-sccache-apne1",
        "region=ap-northeast-1",
        "chain=disk,s3",
        f"dir={home / 'Library' / 'Caches' / 'Mozilla.sccache'}",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_can_disable_default_s3_cache(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'bucket=%s\\n' \"${{SCCACHE_BUCKET-}}\" > {shlex.quote(str(sccache_log))}",
                f"printf 'chain=%s\\n' \"${{SCCACHE_MULTILEVEL_CHAIN-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env["SPUR_SCCACHE_S3"] = "0"
    env.pop("SPUR_SCCACHE_GCS", None)
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        "bucket=",
        "chain=",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_enables_s3_cache_when_requested(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    uname = bin_dir / "uname"
    uname.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
    uname.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'bucket=%s\\n' \"${{SCCACHE_BUCKET-}}\" > {shlex.quote(str(sccache_log))}",
                f"printf 'region=%s\\n' \"${{SCCACHE_REGION-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'aws_region=%s\\n' \"${{AWS_REGION-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'chain=%s\\n' \"${{SCCACHE_MULTILEVEL_CHAIN-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'policy=%s\\n' \"${{SCCACHE_MULTILEVEL_WRITE_ERROR_POLICY-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'dir=%s\\n' \"${{SCCACHE_DIR-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'size=%s\\n' \"${{SCCACHE_CACHE_SIZE-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'gcs=%s\\n' \"${{SCCACHE_GCS_BUCKET-}}\" >> {shlex.quote(str(sccache_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    home = tmp_path / "home"
    home.mkdir()

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["HOME"] = str(home)
    env["SPUR_ROOT"] = str(ROOT)
    env["SPUR_SCCACHE_S3"] = "1"
    env["SPUR_SCCACHE_GCS"] = "1"
    env["SCCACHE_BUCKET"] = "spur-test-s3"
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        "bucket=spur-test-s3",
        "region=ap-northeast-1",
        "aws_region=ap-northeast-1",
        "chain=disk,s3",
        "policy=l0",
        f"dir={home / 'Library' / 'Caches' / 'Mozilla.sccache'}",
        "size=10G",
        "gcs=",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_bypasses_sccache_in_codex_sandbox(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'called\\n' > {shlex.quote(str(sccache_log))}",
                "exit 99",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc_log = tmp_path / "rustc.log"
    rustc = tmp_path / "rustc"
    rustc.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$0\" \"$@\" > {shlex.quote(str(rustc_log))}",
                f"printf 'basedirs=%s\\n' \"$SCCACHE_BASEDIRS\" >> {shlex.quote(str(rustc_log))}",
            ]
        )
        + "\n"
    )
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env["CODEX_SANDBOX"] = "seatbelt"

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert not sccache_log.exists()
    assert rustc_log.read_text().splitlines() == [
        str(rustc),
        "-vV",
        f"basedirs={ROOT}",
    ]


def test_sccache_worktree_allows_sccache_outside_codex_sandbox(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$@\" > {shlex.quote(str(sccache_log))}",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    rustc = tmp_path / "rustc"
    rustc.write_text("#!/bin/sh\nexit 3\n")
    rustc.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_ROOT"] = str(ROOT)
    env.pop("CODEX_SANDBOX", None)

    result = subprocess.run(
        [str(WRAPPER), str(rustc), "-vV"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert sccache_log.read_text().splitlines() == [
        str(rustc),
        "-vV",
    ]


def test_project_cargo_config_does_not_force_unix_rustc_wrapper():
    cargo_config = ROOT / ".cargo" / "config.toml"

    for line in cargo_config.read_text().splitlines():
        assert not line.strip().startswith("rustc-wrapper")


def test_spur_cargo_sets_worktree_wrapper_when_unset(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'wrapper=%s\\n' \"${{RUSTC_WRAPPER-}}\" > {shlex.quote(str(cargo_log))}",
                f"printf 'args=%s\\n' \"$*\" >> {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env.pop("RUSTC_WRAPPER", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata", "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == [
        f"wrapper={WRAPPER}",
        "args=metadata --version",
    ]


def test_spur_cargo_disables_incremental_when_gcs_cache_is_requested(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'incremental=%s\\n' \"${{CARGO_INCREMENTAL-}}\" > {shlex.quote(str(cargo_log))}",
                f"printf 'wrapper=%s\\n' \"${{RUSTC_WRAPPER-}}\" >> {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_SCCACHE_GCS"] = "1"
    env["CODEX_SANDBOX"] = "seatbelt"
    env.pop("RUSTC_WRAPPER", None)
    env.pop("CARGO_INCREMENTAL", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata", "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == [
        "incremental=0",
        f"wrapper={WRAPPER}",
    ]


def test_spur_cargo_disables_incremental_for_default_s3_cache(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'incremental=%s\\n' \"${{CARGO_INCREMENTAL-}}\" > {shlex.quote(str(cargo_log))}",
                f"printf 'wrapper=%s\\n' \"${{RUSTC_WRAPPER-}}\" >> {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["CODEX_SANDBOX"] = "seatbelt"
    env.pop("SPUR_SCCACHE_S3", None)
    env.pop("SPUR_SCCACHE_GCS", None)
    env.pop("RUSTC_WRAPPER", None)
    env.pop("CARGO_INCREMENTAL", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata", "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == [
        "incremental=0",
        f"wrapper={WRAPPER}",
    ]


def test_spur_cargo_strips_sccache_c_compiler_wrappers_in_codex_sandbox(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'CC=%s\\n' \"${{CC-}}\" > {shlex.quote(str(cargo_log))}",
                f"printf 'CXX=%s\\n' \"${{CXX-}}\" >> {shlex.quote(str(cargo_log))}",
                f"printf 'HOST_CC=%s\\n' \"${{HOST_CC-}}\" >> {shlex.quote(str(cargo_log))}",
                f"printf 'HOST_CXX=%s\\n' \"${{HOST_CXX-}}\" >> {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["CODEX_SANDBOX"] = "seatbelt"
    env["CC"] = "sccache cc"
    env["CXX"] = "/opt/homebrew/bin/sccache c++"
    env["HOST_CC"] = "sccache clang"
    env["HOST_CXX"] = "/opt/homebrew/bin/sccache clang++"
    env.pop("RUSTC_WRAPPER", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata", "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == [
        "CC=cc",
        "CXX=c++",
        "HOST_CC=clang",
        "HOST_CXX=clang++",
    ]


def test_spur_cargo_replaces_direct_sccache_rustc_wrapper_in_codex_sandbox(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'wrapper=%s\\n' \"${{RUSTC_WRAPPER-}}\" > {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["CODEX_SANDBOX"] = "seatbelt"
    env["RUSTC_WRAPPER"] = "sccache"

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata", "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == [f"wrapper={WRAPPER}"]


def test_spur_cargo_restarts_when_s3_cache_is_not_active(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'called\\n' > {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$1\" >> {shlex.quote(str(sccache_log))}",
                'case "$1" in',
                "  --show-stats) printf 'Cache location                  Multi-level (2 levels)\\n  L0 (disk)                     Local disk: test\\n  L1 (gcs)                      GCS: test\\n'; exit 0 ;;",
                "  --stop-server) exit 0 ;;",
                "  --start-server) exit 0 ;;",
                "esac",
                "exit 0",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env.pop("SPUR_SCCACHE_S3", None)
    env.pop("SPUR_SCCACHE_GCS", None)
    env.pop("CODEX_SANDBOX", None)
    env.pop("RUSTC_WRAPPER", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.exists()
    assert sccache_log.read_text().splitlines() == [
        "--show-stats",
        "--stop-server",
        "--start-server",
    ]


def test_spur_cargo_reports_gcs_sccache_startup_failure(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'called\\n' > {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    sccache_log = tmp_path / "sccache.log"
    sccache = bin_dir / "sccache"
    sccache.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf '%s\\n' \"$1\" >> {shlex.quote(str(sccache_log))}",
                'case "$1" in',
                "  --show-stats) printf '{\"cache_location\":\"Local disk: test\"}\\n'; exit 0 ;;",
                "  --stop-server) exit 0 ;;",
                "  --start-server) exit 7 ;;",
                "esac",
                "exit 0",
            ]
        )
        + "\n"
    )
    sccache.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["SPUR_SCCACHE_GCS"] = "1"
    env["SPUR_SCCACHE_GCS_FORCE"] = "1"
    env.pop("CODEX_SANDBOX", None)
    env.pop("RUSTC_WRAPPER", None)

    result = subprocess.run(
        [str(SPUR_CARGO), "--version"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 1
    assert "failed to start GCS sccache server" in result.stderr
    assert not cargo_log.exists()
    assert sccache_log.read_text().splitlines() == [
        "--show-stats",
        "--stop-server",
        "--start-server",
        "--start-server",
    ]


def test_spur_cargo_preserves_explicit_rustc_wrapper_override(tmp_path):
    bin_dir = make_isolated_bin(tmp_path)

    cargo_log = tmp_path / "cargo.log"
    cargo = bin_dir / "cargo"
    cargo.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                f"printf 'wrapper=%s\\n' \"${{RUSTC_WRAPPER-}}\" > {shlex.quote(str(cargo_log))}",
            ]
        )
        + "\n"
    )
    cargo.chmod(0o755)

    env = os.environ.copy()
    env["PATH"] = str(bin_dir)
    env["RUSTC_WRAPPER"] = ""

    result = subprocess.run(
        [str(SPUR_CARGO), "metadata"],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
    )

    assert result.returncode == 0, result.stderr
    assert cargo_log.read_text().splitlines() == ["wrapper="]
