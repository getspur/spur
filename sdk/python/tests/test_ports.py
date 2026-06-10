"""Tests for spur_app.ports — pinned against the golden fixture files."""
from __future__ import annotations

import os
from pathlib import Path

import pytest

from spur_app.errors import MissingCapabilityError, PortFileNotFoundError, PortNotFoundError
from spur_app.ports import PortRead, PortStore
from spur_app.testing import FakePortStore

# Resolve fixture dir relative to this test file
FIXTURES_DIR = Path(__file__).resolve().parents[3] / "sdk" / "fixtures" / "port-store"


def test_fixtures_dir_exists():
    """Sanity check: fixture dir must be present."""
    assert FIXTURES_DIR.exists(), f"Fixture dir not found: {FIXTURES_DIR}"
    assert (FIXTURES_DIR / "manifest.json").exists()


# ---------------------------------------------------------------------------
# MissingCapabilityError when env var absent
# ---------------------------------------------------------------------------


def test_port_store_raises_missing_capability_when_env_absent(monkeypatch):
    monkeypatch.delenv("SPUR_PORTS_ROOT", raising=False)
    with pytest.raises(MissingCapabilityError) as exc_info:
        PortStore()
    err = exc_info.value
    assert err.capability == "ports"
    assert "SPUR_PORTS_ROOT" in str(err)
    assert "spur-app.json" in str(err)


# ---------------------------------------------------------------------------
# Fixture-based: manifest parsing (sales — no data file)
# ---------------------------------------------------------------------------


def test_fixture_manifest_lists_sales_and_media():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        store = fake.port_store
        ports = store.list()
    assert "sales" in ports
    assert "spur-ad-capture" in ports


def test_fixture_sales_entry_missing_file_raises_clear_error():
    """sales@v1.arrow does not exist in fixtures — must raise PortFileNotFoundError."""
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        store = fake.port_store
        with pytest.raises(PortFileNotFoundError) as exc_info:
            store.read("sales")
    err = exc_info.value
    assert err.port == "sales"
    assert "sales" in err.path


# ---------------------------------------------------------------------------
# Fixture-based: read spur-ad-capture (the media file exists)
# ---------------------------------------------------------------------------


def test_fixture_media_port_read_bytes():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.bytes == b"fake webm"


def test_fixture_media_port_mime():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.mime == "video/webm"


def test_fixture_media_port_version():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.version == 1


def test_fixture_media_port_kind():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.kind == "media"


def test_fixture_media_port_duration_sec():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.duration_sec == 60.0


def test_fixture_media_port_path_is_absolute():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert result.path.is_absolute()


def test_fixture_media_port_returns_port_read_dataclass():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
    assert isinstance(result, PortRead)


# ---------------------------------------------------------------------------
# PortNotFoundError
# ---------------------------------------------------------------------------


def test_port_not_found_error_names_available_ports():
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        with pytest.raises(PortNotFoundError) as exc_info:
            fake.port_store.read("nonexistent")
    err = exc_info.value
    assert err.port == "nonexistent"
    assert "sales" in err.available
    assert "spur-ad-capture" in err.available


# ---------------------------------------------------------------------------
# basename-join: @vN path bug cannot happen
# ---------------------------------------------------------------------------


def test_basename_join_used_not_raw_path():
    """Even if manifest contains an absolute path, we basename-join under root."""
    with FakePortStore.from_fixtures(FIXTURES_DIR) as fake:
        result = fake.port_store.read("spur-ad-capture")
        root_str = str(fake.root)
    # The file must be INSIDE the tmp root, not under the original fixtures dir
    assert str(FIXTURES_DIR) not in str(result.path)
    assert root_str in str(result.path)


# ---------------------------------------------------------------------------
# Programmatic FakePortStore
# ---------------------------------------------------------------------------


def test_fake_port_store_add_media():
    with FakePortStore().add_media("clip", b"fake-video", mime="video/mp4", duration_sec=5.0) as fake:
        result = fake.port_store.read("clip")
    assert result.bytes == b"fake-video"
    assert result.mime == "video/mp4"
    assert result.duration_sec == 5.0
    assert result.kind == "media"


def test_fake_port_store_add_arrow():
    with FakePortStore().add_arrow("data", b"ipc-bytes") as fake:
        result = fake.port_store.read("data")
    assert result.bytes == b"ipc-bytes"
    assert result.kind == "arrow"
    assert result.mime is None


def test_fake_port_store_env_var_restored(monkeypatch):
    monkeypatch.delenv("SPUR_PORTS_ROOT", raising=False)
    with FakePortStore().add_media("x", b"d") as fake:
        assert os.environ.get("SPUR_PORTS_ROOT") == str(fake.root)
    assert "SPUR_PORTS_ROOT" not in os.environ


def test_fake_port_store_env_dict():
    with FakePortStore().add_media("x", b"d") as fake:
        env = fake.env()
    assert "SPUR_PORTS_ROOT" in env


def test_fake_port_store_port_store_raises_outside_context():
    store = FakePortStore()
    with pytest.raises(RuntimeError, match="context manager"):
        _ = store.port_store


# ---------------------------------------------------------------------------
# Re-parse per read: manifest can be updated between reads
# ---------------------------------------------------------------------------


def test_reparse_per_read_picks_up_new_version():
    """Overwrite manifest between reads — second read must return new data."""
    import json

    with FakePortStore().add_media("clip", b"v1-data", mime="video/mp4") as fake:
        r1 = fake.port_store.read("clip")
        assert r1.bytes == b"v1-data"
        # Simulate host writing v2
        v2_filename = "clip@v2.media"
        (fake.root / v2_filename).write_bytes(b"v2-data")
        manifest = {"ports": {"clip": {"path": v2_filename, "version": 2, "kind": "media", "mime": "video/mp4"}}}
        (fake.root / "manifest.json").write_text(json.dumps(manifest))
        r2 = fake.port_store.read("clip")
    assert r2.bytes == b"v2-data"
    assert r2.version == 2
