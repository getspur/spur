"""spur_app — Python server SDK for Spur app plugins.

Minimal public surface::

    from spur_app import App

    app = App("my-app")

    @app.tool()
    def my_tool(x: int) -> str:
        return str(x)

    if __name__ == "__main__":
        app.run()

See :mod:`spur_app.testing` for test helpers.
"""
from __future__ import annotations

from .app import App
from .errors import (
    ArtifactPathError,
    EnvVarRequiredError,
    MissingCapabilityError,
    PortFileNotFoundError,
    PortManifestError,
    PortNotFoundError,
    SpurAppError,
)

__all__ = [
    "App",
    "SpurAppError",
    "MissingCapabilityError",
    "PortNotFoundError",
    "PortFileNotFoundError",
    "PortManifestError",
    "EnvVarRequiredError",
    "ArtifactPathError",
]
