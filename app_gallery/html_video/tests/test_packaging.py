from __future__ import annotations

import json
import subprocess
import zipfile
from pathlib import Path


APP_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = APP_DIR / "spur-app.json"
EXPECTED_ARCHIVE_FILES = {
    "spur-app.json",
    "server/main.py",
    "templates/index.json",
    "app.ipynb",
}


# TODO(U4/U7): replace this hand-rolled packer with the canonical notebook_export_spur_app
# packer once the Rust tooling front door (U4) is available from tests.
def create_spurapp_archive(output_path: Path) -> None:
    with zipfile.ZipFile(output_path, "w", compression=zipfile.ZIP_STORED) as archive:
        for path in sorted(APP_DIR.rglob("*")):
            if not path.is_file() or should_skip(path):
                continue
            archive.write(path, path.relative_to(APP_DIR).as_posix())


def should_skip(path: Path) -> bool:
    relative_parts = path.relative_to(APP_DIR).parts
    return any(part in {"__pycache__", ".pytest_cache"} for part in relative_parts)


def load_manifest() -> dict:
    with MANIFEST_PATH.open() as file:
        return json.load(file)


def test_manifest_is_valid_json() -> None:
    manifest = load_manifest()

    assert manifest["schema"] == "spur.app/v1"
    assert manifest["name"] == "HTML Video"
    assert manifest["entry_notebook"] == "app.ipynb"
    assert manifest["open_mode"] == "app"
    assert manifest["runtime"]["jute_min"]
    assert isinstance(manifest["runtime"]["features"], list)
    assert manifest["mcp_server"] == {
        "type": "python",
        "entry": "server/main.py",
        "requirements": "server/requirements.txt",
        "env": {},
    }
    # T6c: capabilities and skill fields
    caps = manifest["capabilities"]
    assert caps["ports"]["read"] == ["spur-ad-capture"]
    assert caps["canvas_capture"] is True
    assert caps["active_output_scripts"] is True
    assert caps["artifacts_dir"] is True
    assert manifest["skill"] == "skill/SKILL.md"


def test_archive_contains_expected_files(tmp_path: Path) -> None:
    archive_path = tmp_path / "html_video.spurapp"
    extract_dir = tmp_path / "round_trip"

    create_spurapp_archive(archive_path)

    with zipfile.ZipFile(archive_path) as archive:
        names = set(archive.namelist())
        assert EXPECTED_ARCHIVE_FILES.issubset(names)
        assert json.loads(archive.read("spur-app.json")) == load_manifest()
        archive.extractall(extract_dir)

    for relative_path in EXPECTED_ARCHIVE_FILES:
        assert (extract_dir / relative_path).read_bytes() == (
            APP_DIR / relative_path
        ).read_bytes()


def test_template_files_non_empty() -> None:
    template_paths = sorted((APP_DIR / "templates").glob("*/template.html"))

    assert template_paths
    for template_path in template_paths:
        assert template_path.read_text().strip()


def test_mcp_server_imports_cleanly() -> None:
    subprocess.run(
        ["python3", "-c", "import main"],
        cwd=APP_DIR / "server",
        check=True,
    )
