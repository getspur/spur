"""Tests for spur_app.testing — FakePortStore and pytest fixtures."""
from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from spur_app.errors import PortNotFoundError
from spur_app.testing import FakePortStore

FIXTURES_DIR = Path(__file__).resolve().parents[3] / "sdk" / "fixtures" / "port-store"


# ---------------------------------------------------------------------------
# FakePortStore.from_fixtures
# ---------------------------------------------------------------------------


def test_from_fixtures_env_var_set_inside_context():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        assert os.environ.get("SPUR_PORTS_ROOT") == str(fake.root)


def test_from_fixtures_env_var_restored_after_context(monkeypatch):
    monkeypatch.delenv("SPUR_PORTS_ROOT", raising=False)
    with FakePortStore.from_fixtures(FIXTURES_DIR):
        pass
    assert "SPUR_PORTS_ROOT" not in os.environ


def test_from_fixtures_manifest_is_written(tmp_path):
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        manifest_path = fake.root / "manifest.json"
        assert manifest_path.exists()
        data = json.loads(manifest_path.read_text())
        assert "ports" in data
        assert "spur-ad-capture" in data["ports"]


def test_from_fixtures_media_file_copied():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        media_file = fake.root / "spur-ad-capture@v1.media"
        assert media_file.exists()
        assert media_file.read_bytes() == b"fake webm"


def test_from_fixtures_arrow_file_absent():
    """sales@v1.arrow does not exist in fixtures — no data file should be copied."""
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        arrow_file = fake.root / "sales@v1.arrow"
        assert not arrow_file.exists()


def test_from_fixtures_read_media_port():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.bytes == b"fake webm"
    assert result.version == 1
    assert result.kind == "media"
    assert result.mime == "video/webm"
    assert result.duration_sec == 60.0


def test_from_fixtures_list_ports():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        ports = fake.port_store.list()
    assert set(ports) == {"sales", "spur-ad-capture"}


# ---------------------------------------------------------------------------
# Programmatic FakePortStore
# ---------------------------------------------------------------------------


def test_programmatic_add_media_and_arrow():
    fs = FakePortStore()
    fs.add_media("vid", b"video-bytes", mime="video/mp4", duration_sec=10.0)
    fs.add_arrow("tbl", b"arrow-ipc")
    with fs as fake:
        vid = fake.port_store.read("vid")
        tbl = fake.port_store.read("tbl")
    assert vid.bytes == b"video-bytes"
    assert vid.mime == "video/mp4"
    assert vid.duration_sec == 10.0
    assert tbl.bytes == b"arrow-ipc"
    assert tbl.kind == "arrow"
    assert tbl.mime is None


def test_programmatic_port_not_found():
    with FakePortStore().add_media("x", b"d") as fake:
        with pytest.raises(PortNotFoundError):
            fake.port_store.read("nonexistent")


def test_add_arrow_with_schema():
    schema = {"fields": [{"name": "id", "data_type": "Int64"}]}
    with FakePortStore().add_arrow("data", b"ipc", schema=schema) as fake:
        manifest_path = fake.root / "manifest.json"
        m = json.loads(manifest_path.read_text())
        assert m["ports"]["data"]["schema"] == schema


def test_add_media_no_duration():
    with FakePortStore().add_media("clip", b"d", mime="video/mp4") as fake:
        result = fake.port_store.read("clip")
    assert result.duration_sec is None


def test_fake_port_store_builder_chaining():
    """Builder methods return self for chaining."""
    fs = FakePortStore()
    result = fs.add_media("a", b"1").add_arrow("b", b"2")
    assert result is fs


# ---------------------------------------------------------------------------
# Env var isolation
# ---------------------------------------------------------------------------


def test_env_var_not_leaked_after_context():
    original = os.environ.get("SPUR_PORTS_ROOT")
    with FakePortStore().add_media("x", b"d"):
        pass
    assert os.environ.get("SPUR_PORTS_ROOT") == original


def test_env_dict_method():
    with FakePortStore().add_media("x", b"d") as fake:
        env = fake.env()
        assert env["SPUR_PORTS_ROOT"] == str(fake.root)


# ---------------------------------------------------------------------------
# pytest fixture injection — exercises the @pytest.fixture path end-to-end
# ---------------------------------------------------------------------------


def test_fake_port_store_fixture_injects_active_store(fake_port_store):
    """Verify fake_port_store is a real pytest fixture that injects a live store.

    The fixture yields an empty (already-entered) FakePortStore; we write a
    port file directly into the tmp root to confirm it is readable.
    """
    # SPUR_PORTS_ROOT must be patched for the duration of this test
    assert os.environ.get("SPUR_PORTS_ROOT") == str(fake_port_store.root)
    # Write a port file + manifest directly into the live tmp root
    import json as _json
    (fake_port_store.root / "clip@v1.media").write_bytes(b"fixture-bytes")
    manifest = {"ports": {"clip": {"path": "clip@v1.media", "version": 1, "kind": "media", "mime": "video/mp4"}}}
    (fake_port_store.root / "manifest.json").write_text(_json.dumps(manifest))
    result = fake_port_store.port_store.read("clip")
    assert result.bytes == b"fixture-bytes"
    assert result.mime == "video/mp4"


def test_fake_port_store_fixture_env_is_set(fake_port_store):
    """A second injected-fixture test confirms the env is set on each test run."""
    assert os.environ.get("SPUR_PORTS_ROOT") is not None


# ---------------------------------------------------------------------------
# root property outside context raises
# ---------------------------------------------------------------------------


def test_root_property_outside_context_raises():
    fs = FakePortStore()
    with pytest.raises(RuntimeError, match="context manager"):
        _ = fs.root
