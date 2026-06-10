"""Port-store reader for spur_app.

The host injects ``SPUR_PORTS_ROOT`` into the app-plugin process when the
manifest declares ``capabilities.ports``.  ``SPUR_PORTS_ROOT`` points at the
directory that contains ``manifest.json`` and the versioned port files.

Wire shape of ``manifest.json``::

    {
      "ports": {
        "<name>": {
          "path": "<basename-or-abs-path>",   # MUST be basename-joined
          "version": <int>,
          "kind": "arrow" | "media",
          "mime": "<str>",          # optional
          "size": <int>,            # optional
          "schema": {...},          # optional, arrow only
          "duration_sec": <float>   # optional
        }
      }
    }

**Important:** ``entry["path"]`` may be an absolute path in older manifests.
Consumers MUST always ``basename``-join under ``root`` — never use the raw
path verbatim.  This eliminates the ``@vN`` path bug (render.py:369).

**Re-parse per read:** :meth:`PortStore.read` re-parses ``manifest.json`` on
every call.  The host atomically rewrites the manifest (new versioned file +
new manifest) so a cached manifest would cause a stale read for the lifetime
of the process.  The cost is a single small JSON file read, which is
negligible compared to any real port payload.
"""
from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

from .errors import (
    MissingCapabilityError,
    PortFileNotFoundError,
    PortManifestError,
    PortNotFoundError,
)

ENV_VAR = "SPUR_PORTS_ROOT"


@dataclass(frozen=True)
class PortRead:
    """The result of reading a port from the port store.

    Attributes
    ----------
    bytes:
        Raw file contents.
    mime:
        MIME type declared in the manifest, or ``None`` for arrow ports.
    version:
        Version counter from the manifest.
    kind:
        ``"arrow"`` or ``"media"``.
    duration_sec:
        Duration in seconds for media ports, or ``None`` when not declared.
    path:
        The resolved filesystem :class:`~pathlib.Path` that was read.
    """

    bytes: bytes
    mime: str | None
    version: int
    kind: str
    duration_sec: float | None
    path: Path


class PortStore:
    """Reads port data from the host-provisioned port store.

    Instantiated lazily by :attr:`App.ports`; the constructor validates
    ``SPUR_PORTS_ROOT`` immediately so failures are surfaced on first access.

    Parameters
    ----------
    root:
        Override the root directory.  When ``None`` the value of
        ``SPUR_PORTS_ROOT`` is used.
    """

    def __init__(self, root: str | Path | None = None) -> None:
        if root is None:
            raw = os.environ.get(ENV_VAR)
            if not raw:
                raise MissingCapabilityError(
                    "ports",
                    f"{ENV_VAR} not provisioned — "
                    "declare capabilities.ports in spur-app.json",
                )
            root = raw
        self._root = Path(root)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def read(self, name: str) -> PortRead:
        """Read the named port, re-parsing the manifest on every call.

        Re-parsing is intentional: the host atomically rewrites
        ``manifest.json`` whenever a new port version is written, so a cached
        manifest would silently return stale data for the lifetime of the
        process.

        Parameters
        ----------
        name:
            Port name as declared in the manifest (e.g. ``"sales"``).

        Raises
        ------
        PortManifestError
            When ``manifest.json`` is missing or contains invalid JSON.
        PortNotFoundError
            When *name* is absent from the manifest.
        PortFileNotFoundError
            When the manifest entry exists but the data file is missing on
            disk.
        """
        manifest = self._parse_manifest()
        ports: dict = manifest.get("ports", {})
        if name not in ports:
            raise PortNotFoundError(name, list(ports.keys()))
        entry = ports[name]
        # basename-join: eliminates the @vN absolute-path bug
        file_path = self._root / Path(entry["path"]).name
        if not file_path.exists():
            raise PortFileNotFoundError(name, file_path)
        data = file_path.read_bytes()
        return PortRead(
            bytes=data,
            mime=entry.get("mime"),
            version=entry["version"],
            kind=entry["kind"],
            duration_sec=entry.get("duration_sec"),
            path=file_path,
        )

    def list(self) -> list[str]:
        """Return the names of all ports declared in the current manifest."""
        manifest = self._parse_manifest()
        return list(manifest.get("ports", {}).keys())

    # ------------------------------------------------------------------
    # Private helpers
    # ------------------------------------------------------------------

    def _parse_manifest(self) -> dict:
        manifest_path = self._root / "manifest.json"
        try:
            with manifest_path.open() as fh:
                return json.load(fh)
        except FileNotFoundError:
            raise PortManifestError(manifest_path, "file not found")
        except json.JSONDecodeError as exc:
            raise PortManifestError(manifest_path, f"invalid JSON: {exc}") from exc
