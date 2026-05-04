import os
import shlex
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT / "scripts" / "sccache-worktree.sh"


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
