"""Tests for the code-graph-workbench server seed."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))

from main import wb_ping


def test_wb_ping_reports_live_surface():
    result = wb_ping()
    assert result == {"ok": True, "app": "code-graph-workbench"}
