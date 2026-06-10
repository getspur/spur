"""Test helpers for spur_app.

:class:`FakePortStore` materialises a real port-store directory in a
temporary directory (``manifest.json`` + files) from entries you add
programmatically or from the golden fixture directory.  It sets / patches
``SPUR_PORTS_ROOT`` so that any code that constructs a :class:`PortStore`
inside the context picks up the fake root automatically.

**stdlib-only** — no ``mcp`` import, no third-party dependencies.

Typical pytest usage::

    from spur_app.testing import FakePortStore

    def test_my_tool():
        with FakePortStore.from_fixtures(FIXTURES_DIR) as store:
            result = store.port_store.read("spur-ad-capture")
            assert result.mime == "video/webm"

Or use the helper fixtures::

    # conftest.py
    from spur_app.testing import fake_port_store   # noqa: F401

    def test_tool(fake_port_store):
        result = fake_port_store.port_store.read("spur-ad-capture")
        ...
"""
from __future__ import annotations

import json
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any, Iterator, Optional

from .ports import PortStore


class FakePortStore:
    """Builder / context-manager that creates a temporary port store.

    Usage::

        with FakePortStore() as store:
            store.add_media("clip", b"fake-bytes", mime="video/mp4", duration_sec=5.0)
            store.add_arrow("data", b"ipc-bytes")
            # SPUR_PORTS_ROOT is set inside the context
            port_read = store.port_store.read("clip")

    Or load from the golden fixtures::

        with FakePortStore.from_fixtures(Path(...) / "fixtures" / "port-store") as store:
            ...
    """

    def __init__(self) -> None:
        self._entries: list[dict[str, Any]] = []
        self._tmp_dir: Optional[Path] = None
        self._old_env: Optional[str] = None

    # ------------------------------------------------------------------
    # Builder methods — call before entering the context
    # ------------------------------------------------------------------

    def add_media(
        self,
        name: str,
        data: bytes,
        *,
        mime: str = "application/octet-stream",
        duration_sec: Optional[float] = None,
        version: int = 1,
    ) -> "FakePortStore":
        """Add a media port entry.

        Parameters
        ----------
        name:
            Port name (the manifest key).
        data:
            Raw bytes written to the port file.
        mime:
            MIME type string.
        duration_sec:
            Optional duration in seconds.
        version:
            Version counter.
        """
        filename = f"{name}@v{version}.media"
        entry: dict[str, Any] = {
            "name": name,
            "filename": filename,
            "data": data,
            "manifest": {
                "path": filename,
                "version": version,
                "kind": "media",
                "mime": mime,
                "size": len(data),
            },
        }
        if duration_sec is not None:
            entry["manifest"]["duration_sec"] = duration_sec
        self._entries.append(entry)
        return self

    def add_arrow(
        self,
        name: str,
        ipc_bytes: bytes,
        *,
        schema: Optional[dict] = None,
        version: int = 1,
    ) -> "FakePortStore":
        """Add an arrow port entry.

        Parameters
        ----------
        name:
            Port name (the manifest key).
        ipc_bytes:
            Raw Arrow IPC bytes written to the port file.
        schema:
            Optional Arrow schema dict to embed in the manifest.
        version:
            Version counter.
        """
        filename = f"{name}@v{version}.arrow"
        entry: dict[str, Any] = {
            "name": name,
            "filename": filename,
            "data": ipc_bytes,
            "manifest": {
                "path": filename,
                "version": version,
                "kind": "arrow",
            },
        }
        if schema is not None:
            entry["manifest"]["schema"] = schema
        self._entries.append(entry)
        return self

    # ------------------------------------------------------------------
    # Factory: load from golden fixtures
    # ------------------------------------------------------------------

    @classmethod
    def from_fixtures(cls, fixtures_dir: Path) -> "FakePortStore":
        """Create a :class:`FakePortStore` that mirrors a fixture directory.

        Reads the manifest from *fixtures_dir* and copies every file that
        exists on disk (some entries — like ``sales@v1.arrow`` — may be
        absent from the fixtures; those entries are still written to the
        manifest so manifest-parse tests work, but no data file is created).

        Parameters
        ----------
        fixtures_dir:
            Path to a port-store fixture directory containing
            ``manifest.json``.
        """
        instance = cls()
        instance._fixture_dir = fixtures_dir
        return instance

    # ------------------------------------------------------------------
    # Context manager
    # ------------------------------------------------------------------

    def __enter__(self) -> "FakePortStore":
        self._tmp_dir = Path(tempfile.mkdtemp(prefix="spur_fake_port_store_"))

        if hasattr(self, "_fixture_dir"):
            # Mirror from fixture dir: copy manifest + any files that exist
            fixture_manifest_path = self._fixture_dir / "manifest.json"
            with fixture_manifest_path.open() as fh:
                manifest = json.load(fh)
            # Copy data files that exist
            for _port_name, entry in manifest.get("ports", {}).items():
                basename = Path(entry["path"]).name
                src = self._fixture_dir / basename
                if src.exists():
                    shutil.copy2(src, self._tmp_dir / basename)
            # Write manifest verbatim (basename-join is done at read time)
            (self._tmp_dir / "manifest.json").write_text(
                json.dumps(manifest, indent=2)
            )
        else:
            # Build from programmatic entries
            ports_manifest: dict[str, Any] = {}
            for entry in self._entries:
                (self._tmp_dir / entry["filename"]).write_bytes(entry["data"])
                ports_manifest[entry["name"]] = entry["manifest"]
            manifest = {"ports": ports_manifest}
            (self._tmp_dir / "manifest.json").write_text(
                json.dumps(manifest, indent=2)
            )

        # Patch env var
        self._old_env = os.environ.get("SPUR_PORTS_ROOT")
        os.environ["SPUR_PORTS_ROOT"] = str(self._tmp_dir)
        return self

    def __exit__(self, *_: object) -> None:
        # Restore env var
        if self._old_env is None:
            os.environ.pop("SPUR_PORTS_ROOT", None)
        else:
            os.environ["SPUR_PORTS_ROOT"] = self._old_env
        # Clean up tmp dir
        if self._tmp_dir and self._tmp_dir.exists():
            shutil.rmtree(self._tmp_dir)
        self._tmp_dir = None

    # ------------------------------------------------------------------
    # Convenience accessor
    # ------------------------------------------------------------------

    @property
    def port_store(self) -> PortStore:
        """A :class:`PortStore` rooted at this fake store's tmp directory.

        Only valid inside the context manager.
        """
        if self._tmp_dir is None:
            raise RuntimeError("FakePortStore must be used as a context manager")
        return PortStore(root=self._tmp_dir)

    @property
    def root(self) -> Path:
        """The tmp directory for this fake port store.

        Only valid inside the context manager.
        """
        if self._tmp_dir is None:
            raise RuntimeError("FakePortStore must be used as a context manager")
        return self._tmp_dir

    def env(self) -> dict[str, str]:
        """Return an env dict with ``SPUR_PORTS_ROOT`` set.

        Useful when you want to pass env vars to a subprocess rather than
        patch ``os.environ`` directly.  Only valid inside the context manager.
        """
        return {"SPUR_PORTS_ROOT": str(self.root)}


# ---------------------------------------------------------------------------
# pytest fixture helpers
# ---------------------------------------------------------------------------


def fake_port_store() -> Iterator["FakePortStore"]:
    """Pytest fixture that yields a :class:`FakePortStore` context.

    Import directly in ``conftest.py``::

        from spur_app.testing import fake_port_store  # noqa: F401

    Then use it in tests::

        def test_something(fake_port_store):
            fake_port_store.add_media("clip", b"data", mime="video/mp4")
            # SPUR_PORTS_ROOT is patched for the test
            store = fake_port_store.port_store
            result = store.read("clip")
            assert result.mime == "video/mp4"

    Note: since the fixture is yielded, you can add entries BEFORE the
    context activates by building the store and then using it.  The fixture
    enters the context before yielding.
    """
    with FakePortStore() as store:
        yield store
