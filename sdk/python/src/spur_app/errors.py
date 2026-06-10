"""Error types for the spur_app SDK.

All errors derive from :class:`SpurAppError` so callers can catch the whole
hierarchy with a single ``except SpurAppError``.
"""
from __future__ import annotations

from pathlib import Path


class SpurAppError(Exception):
    """Base class for all spur_app errors."""


class MissingCapabilityError(SpurAppError):
    """Raised when the host has not provisioned a required capability.

    The *message* MUST name the missing env var and tell the developer which
    ``spur-app.json`` manifest key to add, e.g.::

        MissingCapabilityError(
            "ports",
            "SPUR_PORTS_ROOT not provisioned — "
            "declare capabilities.ports in spur-app.json",
        )
    """

    def __init__(self, capability: str, message: str) -> None:
        self.capability = capability
        super().__init__(message)


class PortNotFoundError(SpurAppError):
    """Raised when a named port is not present in the manifest.

    Parameters
    ----------
    port:
        The name of the requested port.
    available:
        The list of port names that *are* present in the manifest.
    """

    def __init__(self, port: str, available: list[str]) -> None:
        self.port = port
        self.available = available
        names = ", ".join(sorted(available)) if available else "(none)"
        super().__init__(
            f"Port {port!r} not found in manifest. Available ports: {names}"
        )


class PortFileNotFoundError(SpurAppError):
    """Raised when a port's manifest entry exists but the data file is missing.

    Parameters
    ----------
    port:
        The name of the port whose file was expected.
    path:
        The resolved filesystem :class:`~pathlib.Path` that was expected to
        exist.
    """

    def __init__(self, port: str, path: Path) -> None:
        self.port = port
        self.path = path
        super().__init__(
            f"Port {port!r} is declared in the manifest but its data file is "
            f"missing: {path}"
        )


class PortManifestError(SpurAppError):
    """Raised when ``manifest.json`` is missing or cannot be parsed.

    Parameters
    ----------
    manifest_path:
        The path to ``manifest.json`` that was expected to exist or be valid
        JSON.
    reason:
        A short description of what went wrong (e.g. "file not found" or
        "invalid JSON").
    """

    def __init__(self, manifest_path: Path, reason: str) -> None:
        self.manifest_path = manifest_path
        self.reason = reason
        super().__init__(
            f"Could not read port-store manifest at {manifest_path}: {reason}"
        )


class EnvVarRequiredError(SpurAppError):
    """Raised by :meth:`app.env.require` when a variable is absent.

    Parameters
    ----------
    name:
        The name of the missing env var.
    """

    def __init__(self, name: str) -> None:
        self.name = name
        super().__init__(
            f"Required env var {name!r} is not set. "
            "Declare it in the 'env' block of spur-app.json."
        )


class ArtifactPathError(SpurAppError):
    """Raised when an artifact path is unsafe (absolute or escaping the dir)."""
