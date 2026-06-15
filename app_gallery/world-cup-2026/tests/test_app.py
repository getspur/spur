"""Tests for the world-cup-2026 app — manifest, tool surface, SDK port store."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))

from spur_app.testing import FakePortStore

APP_ROOT = Path(__file__).resolve().parents[1]


def test_manifest_is_valid_and_declares_server():
    manifest = json.loads((APP_ROOT / "spur-app.json").read_text())
    assert manifest["schema"] == "spur.app/v1"
    assert manifest["name"] == "world-cup-2026"
    assert manifest["entry_notebook"] == "app.ipynb"
    assert manifest["mcp_server"]["entry"] == "server/main.py"
    # Perspective needs active scripts in the output iframe.
    assert manifest["capabilities"]["active_output_scripts"] is True


def test_server_registers_wc_tools():
    import main

    for name in ("wc_markets", "wc_news", "wc_snapshot", "wc_report"):
        assert callable(getattr(main, name)), f"{name} should be a registered tool"


def test_skill_hard_gate_lists_live_tools():
    skill = (APP_ROOT / "skill" / "SKILL.md").read_text()
    gate = skill.split("<HARD-GATE>", 1)[1].split("</HARD-GATE>", 1)[0]
    for tool in ("wc_snapshot", "wc_markets", "wc_news", "wc_report"):
        assert f"`{tool}`" in gate


def test_port_store_round_trip():
    store = FakePortStore().add_media("clip", b"fake-bytes", mime="video/mp4")
    with store:
        assert store.port_store.read("clip").mime == "video/mp4"
