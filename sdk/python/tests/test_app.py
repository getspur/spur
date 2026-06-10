"""Tests for spur_app.App.

mcp is imported lazily, so these tests exercise the lazy-init paths without
requiring mcp to be installed.  The mcp-specific surface (tool(), run()) is
only tested when mcp is available.
"""
from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from spur_app import App
from spur_app.artifacts import ArtifactStore
from spur_app.errors import MissingCapabilityError
from spur_app.env import EnvAccessor
from spur_app.ports import PortStore
from spur_app.testing import FakePortStore


# ---------------------------------------------------------------------------
# Lazy construction — App() must not require any env
# ---------------------------------------------------------------------------


def test_app_constructs_without_env(monkeypatch):
    monkeypatch.delenv("SPUR_PORTS_ROOT", raising=False)
    monkeypatch.delenv("SPUR_ARTIFACTS_DIR", raising=False)
    # Must not raise
    app = App("test-app")
    assert app is not None


# ---------------------------------------------------------------------------
# app.ports — lazy, raises when env absent
# ---------------------------------------------------------------------------


def test_app_ports_raises_when_env_absent(monkeypatch):
    monkeypatch.delenv("SPUR_PORTS_ROOT", raising=False)
    app = App("test-app")
    with pytest.raises(MissingCapabilityError) as exc_info:
        _ = app.ports
    assert exc_info.value.capability == "ports"


def test_app_ports_returns_port_store(monkeypatch, tmp_path):
    import json
    (tmp_path / "manifest.json").write_text(json.dumps({"ports": {}}))
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(tmp_path))
    app = App("test-app")
    store = app.ports
    assert isinstance(store, PortStore)


def test_app_ports_is_cached(monkeypatch, tmp_path):
    import json
    (tmp_path / "manifest.json").write_text(json.dumps({"ports": {}}))
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(tmp_path))
    app = App("test-app")
    assert app.ports is app.ports


# ---------------------------------------------------------------------------
# app.artifacts — lazy, raises when env absent
# ---------------------------------------------------------------------------


def test_app_artifacts_raises_when_env_absent(monkeypatch):
    monkeypatch.delenv("SPUR_ARTIFACTS_DIR", raising=False)
    app = App("test-app")
    with pytest.raises(MissingCapabilityError) as exc_info:
        _ = app.artifacts
    assert exc_info.value.capability == "artifacts_dir"


def test_app_artifacts_returns_artifact_store(monkeypatch, tmp_path):
    monkeypatch.setenv("SPUR_ARTIFACTS_DIR", str(tmp_path))
    app = App("test-app")
    store = app.artifacts
    assert isinstance(store, ArtifactStore)


def test_app_artifacts_is_cached(monkeypatch, tmp_path):
    monkeypatch.setenv("SPUR_ARTIFACTS_DIR", str(tmp_path))
    app = App("test-app")
    assert app.artifacts is app.artifacts


# ---------------------------------------------------------------------------
# app.env
# ---------------------------------------------------------------------------


def test_app_env_returns_env_accessor():
    app = App("test-app")
    assert isinstance(app.env, EnvAccessor)


def test_app_env_is_cached():
    app = App("test-app")
    assert app.env is app.env


# ---------------------------------------------------------------------------
# End-to-end: app.ports.read via FakePortStore
# ---------------------------------------------------------------------------

FIXTURES_DIR = Path(__file__).resolve().parents[3] / "sdk" / "fixtures" / "port-store"


def test_app_ports_read_media_via_fixture():
    with FakePortStore.from_fixtures(FIXTURES_DIR):
        app = App("test-app")
        result = app.ports.read("spur-ad-capture")
    assert result.bytes == b"fake webm"
    assert result.mime == "video/webm"
    assert result.duration_sec == 60.0


def test_app_artifacts_path_via_env(monkeypatch, tmp_path):
    monkeypatch.setenv("SPUR_ARTIFACTS_DIR", str(tmp_path))
    app = App("test-app")
    p = app.artifacts.path("output/render.mp4")
    assert p.parent.exists()
    assert str(tmp_path) in str(p)


# ---------------------------------------------------------------------------
# tool() and run() — tested only when mcp is importable
# ---------------------------------------------------------------------------


def test_app_tool_decorator_delegates_to_fastmcp():
    """App.tool() must delegate to FastMCP.tool()."""
    try:
        import mcp  # noqa: F401
    except ImportError:
        pytest.skip("mcp not installed")

    app = App("test-app")
    mock_mcp = MagicMock()
    mock_mcp.tool.return_value = lambda f: f
    app._mcp = mock_mcp

    @app.tool()
    def my_tool(x: int) -> str:
        return str(x)

    mock_mcp.tool.assert_called_once()


def test_app_run_calls_mcp_run():
    """App.run() must call FastMCP.run(transport='stdio')."""
    try:
        import mcp  # noqa: F401
    except ImportError:
        pytest.skip("mcp not installed")

    app = App("test-app")
    mock_mcp = MagicMock()
    app._mcp = mock_mcp

    app.run()

    mock_mcp.run.assert_called_once_with(transport="stdio")
