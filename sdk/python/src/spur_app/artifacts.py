"""Artifact directory accessor for spur_app.

The host injects ``SPUR_ARTIFACTS_DIR`` and creates the directory when the
app manifest declares ``capabilities.artifacts_dir``.  :class:`ArtifactStore`
validates the env var on first access (via :attr:`App.artifacts`) and then
provides safe path resolution under that directory.
"""
from __future__ import annotations

import os
from pathlib import Path

from .errors import ArtifactPathError, MissingCapabilityError

ENV_VAR = "SPUR_ARTIFACTS_DIR"


class ArtifactStore:
    """Resolves and creates paths inside the host-provisioned artifacts dir.

    Instantiated lazily by :attr:`App.artifacts`; the constructor validates
    ``SPUR_ARTIFACTS_DIR`` immediately so failures are surfaced on first access.

    Parameters
    ----------
    root:
        Override the artifacts root.  When ``None`` the value of
        ``SPUR_ARTIFACTS_DIR`` is used.
    """

    def __init__(self, root: str | Path | None = None) -> None:
        if root is None:
            raw = os.environ.get(ENV_VAR)
            if not raw:
                raise MissingCapabilityError(
                    "artifacts_dir",
                    f"{ENV_VAR} not provisioned — "
                    "declare capabilities.artifacts_dir in spur-app.json",
                )
            root = raw
        self._root = Path(root).resolve()

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def path(self, relative: str) -> Path:
        """Return a :class:`~pathlib.Path` under ``SPUR_ARTIFACTS_DIR``.

        Creates the parent directory (and all ancestors) if they do not yet
        exist.

        Parameters
        ----------
        relative:
            A relative path segment, e.g. ``"renders/output.mp4"``.

        Raises
        ------
        ArtifactPathError
            When *relative* is absolute or would escape the artifacts root via
            ``..`` traversal.
        """
        if Path(relative).is_absolute():
            raise ArtifactPathError(
                f"Artifact path must be relative, got: {relative!r}"
            )
        resolved = (self._root / relative).resolve()
        try:
            resolved.relative_to(self._root)
        except ValueError:
            raise ArtifactPathError(
                f"Artifact path {relative!r} escapes the artifacts root "
                f"{self._root}"
            )
        resolved.parent.mkdir(parents=True, exist_ok=True)
        return resolved

    @property
    def root(self) -> Path:
        """The resolved artifacts root directory."""
        return self._root
