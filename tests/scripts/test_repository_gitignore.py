import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def test_code_eval_benchmark_evidence_is_ignored_from_git_discovery() -> None:
    generated_source = (
        ".spur/bench-evidence/code-eval-run/repository-cache/"
        "repositories/repository-example/src/lib.rs"
    )

    result = subprocess.run(
        [
            "git",
            "-C",
            str(ROOT),
            "check-ignore",
            "--no-index",
            "--verbose",
            generated_source,
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, (
        "code-eval benchmark evidence must be excluded before Git and code-graph "
        f"discovery; stderr={result.stderr!r}"
    )
    assert ".spur/bench-evidence/" in result.stdout
