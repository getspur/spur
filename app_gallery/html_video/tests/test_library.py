from __future__ import annotations

from pathlib import Path

import pytest


library = pytest.importorskip("server.library")


@pytest.fixture()
def real_templates_dir(monkeypatch: pytest.MonkeyPatch) -> Path:
    templates_dir = Path(__file__).resolve().parents[1] / "templates"
    if not (templates_dir / "index.json").is_file():
        pytest.skip("html_video templates are unavailable")
    monkeypatch.setenv("TEMPLATES_DIR", str(templates_dir))
    return templates_dir


def test_search_returns_results_for_data_intent(real_templates_dir: Path) -> None:
    results = library.search("data visualization", top=10)

    assert any(result["id"] == "frame-data-rollup" for result in results)


def test_search_scores_exact_match_highest(real_templates_dir: Path) -> None:
    results = library.search("Glitch Title", top=10)

    assert results
    assert results[0]["id"] == "frame-glitch-title"


def test_get_template_returns_html_and_metadata(real_templates_dir: Path) -> None:
    template = library.get_template("frame-glitch-title")

    assert template["metadata"]["id"] == "frame-glitch-title"
    assert template["metadata"]["title"] == "Glitch Title"
    assert "<html" in template["html"].lower()
    assert template["skill_md"]


def test_get_template_unknown_id_raises(real_templates_dir: Path) -> None:
    with pytest.raises(library.TemplateNotFound):
        library.get_template("nonexistent")


def test_tokenize_splits_and_lowercases() -> None:
    assert library.tokenize("Hello World") == ["hello", "world"]
