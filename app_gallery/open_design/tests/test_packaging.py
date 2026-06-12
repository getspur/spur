from __future__ import annotations

import json
import zipfile
from pathlib import Path


APP_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = APP_DIR / "spur-app.json"
EXPECTED_ARCHIVE_FILES = {
    "spur-app.json",
    "app.ipynb",
    "skill/SKILL.md",
    "skill/references/artifact-tracks.md",
    "skill/references/critique.md",
    "skill/references/deck-artifact.md",
    "skill/references/deck-mode.md",
    "skill/references/design-systems.md",
    "skill/references/directions.md",
    "library/open-design-library/index.json",
    "library/open-design-deck-library/index.json",
    "library/open-design-deck-library/deck-skeleton.html",
    "library/skill-catalog/web-prototype/SKILL.md",
    "library/skill-catalog/dashboard/SKILL.md",
    "library/skill-catalog/replit-deck/SKILL.md",
}


def create_spurapp_archive(output_path: Path) -> None:
    with zipfile.ZipFile(output_path, "w", compression=zipfile.ZIP_STORED) as archive:
        for path in sorted(APP_DIR.rglob("*")):
            if path.is_file() and not should_skip(path):
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
    assert manifest["name"] == "Open Design"
    assert manifest["entry_notebook"] == "app.ipynb"
    assert manifest["open_mode"] == "app"
    assert manifest["runtime"]["jute_min"]
    assert "mcp-tools" in manifest["runtime"]["features"]
    assert manifest["skill"] == "skill/SKILL.md"
    assert manifest["library"] == {
        "env_var": "SPUR_OPEN_DESIGN_LIBRARY",
        "root": "library",
        "design_systems": "library/open-design-library",
        "deck_themes": "library/open-design-deck-library",
        "skill_catalog": "library/skill-catalog",
    }


def test_skill_and_references_are_migrated() -> None:
    skill = (APP_DIR / "skill" / "SKILL.md").read_text()

    assert "name: open-design" in skill
    assert "open_design_search" in skill
    assert "notebook_insert_cell" in skill
    assert "text/html" in skill
    for reference in [
        "artifact-tracks.md",
        "critique.md",
        "deck-artifact.md",
        "deck-mode.md",
        "design-systems.md",
        "directions.md",
    ]:
        assert (APP_DIR / "skill" / "references" / reference).read_text().strip()


def test_runtime_libraries_are_migrated() -> None:
    design_index = json.loads(
        (APP_DIR / "library" / "open-design-library" / "index.json").read_text()
    )
    deck_index = json.loads(
        (APP_DIR / "library" / "open-design-deck-library" / "index.json").read_text()
    )

    assert design_index["kind"] == "design-systems"
    assert design_index["count"] > 0
    assert (APP_DIR / "library" / "open-design-library" / "design-systems").is_dir()
    assert deck_index["kind"] == "deck-themes"
    assert deck_index["count"] > 0
    assert (APP_DIR / "library" / "open-design-deck-library" / "deck-themes").is_dir()


def test_upstream_skill_catalog_definitions_are_migrated() -> None:
    skill_paths = sorted(
        (APP_DIR / "library" / "skill-catalog").glob("*/SKILL.md")
    )

    assert len(skill_paths) == 122
    assert (APP_DIR / "library" / "skill-catalog" / "web-prototype" / "SKILL.md").is_file()
    assert (APP_DIR / "library" / "skill-catalog" / "dashboard" / "SKILL.md").is_file()
    assert (APP_DIR / "library" / "skill-catalog" / "replit-deck" / "SKILL.md").is_file()


def test_archive_contains_expected_files(tmp_path: Path) -> None:
    archive_path = tmp_path / "open_design.spurapp"
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
