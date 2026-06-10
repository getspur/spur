"""Core :class:`App` class for spur_app.

``mcp`` is imported lazily inside :class:`App` so that :mod:`spur_app.ports`
and :mod:`spur_app.testing` work in environments where ``mcp`` is not
installed (e.g. during pure-library testing of port / artifact logic).
"""
from __future__ import annotations

import logging
import sys
from typing import Any, Callable, Optional, TypeVar

from .artifacts import ArtifactStore
from .env import EnvAccessor
from .ports import PortStore

F = TypeVar("F", bound=Callable[..., Any])

_log = logging.getLogger(__name__)


def _configure_stderr_logging() -> None:
    """Route all spur_app log output to stderr.

    stdio transport owns stdout, so we must never write structured log lines
    there.  This configures a root handler that writes to ``sys.stderr`` if
    no handlers are already attached to the root logger.
    """
    root = logging.getLogger()
    if not root.handlers:
        handler = logging.StreamHandler(sys.stderr)
        handler.setFormatter(
            logging.Formatter("%(asctime)s %(name)s %(levelname)s %(message)s")
        )
        root.addHandler(handler)
        root.setLevel(logging.INFO)


class App:
    """Bootstrap class for a Spur app plugin.

    Wraps ``mcp.server.fastmcp.FastMCP`` (imported lazily), reads the
    provisioned env contract *once on first access*, and fails fast with
    named-contract errors when a required capability is absent.

    Parameters
    ----------
    name:
        The app name, used as the MCP server name (should match the
        ``name`` field in ``spur-app.json``).

    Example
    -------
    ::

        from spur_app import App

        app = App("html-video")

        @app.tool()
        def render(port_names: list[str], output_path: str, fps: int = 30):
            frames = [app.ports.read(p) for p in port_names]
            out = app.artifacts.path(output_path)
            ...

        if __name__ == "__main__":
            app.run()
    """

    def __init__(self, name: str) -> None:
        self._name = name
        _configure_stderr_logging()
        _log.info("spur_app.App(%r) created", name)

        # Lazily initialised on first property access
        self._ports: Optional[PortStore] = None
        self._artifacts: Optional[ArtifactStore] = None
        self._env: Optional[EnvAccessor] = None
        self._mcp: Any = None  # FastMCP instance, created on demand

    # ------------------------------------------------------------------
    # Capability properties — lazy + cached
    # ------------------------------------------------------------------

    @property
    def ports(self) -> PortStore:
        """The port-store accessor.

        **Lazy + cached:** Accessing this property for the first time
        validates ``SPUR_PORTS_ROOT`` and caches the :class:`PortStore`
        instance.  Subsequent accesses return the cached instance.

        Raises
        ------
        MissingCapabilityError
            When ``SPUR_PORTS_ROOT`` is not set.
        """
        if self._ports is None:
            self._ports = PortStore()
        return self._ports

    @property
    def artifacts(self) -> ArtifactStore:
        """The artifact-store accessor.

        **Lazy + cached:** Accessing this property for the first time
        validates ``SPUR_ARTIFACTS_DIR`` and caches the
        :class:`ArtifactStore` instance.  Subsequent accesses return the
        cached instance.

        Raises
        ------
        MissingCapabilityError
            When ``SPUR_ARTIFACTS_DIR`` is not set.
        """
        if self._artifacts is None:
            self._artifacts = ArtifactStore()
        return self._artifacts

    @property
    def env(self) -> EnvAccessor:
        """Typed accessors for manifest-declared env vars.

        Cached on first access; always returns the same instance.
        """
        if self._env is None:
            self._env = EnvAccessor()
        return self._env

    # ------------------------------------------------------------------
    # MCP / tool surface
    # ------------------------------------------------------------------

    def _get_mcp(self) -> Any:
        """Return the FastMCP instance, creating it lazily."""
        if self._mcp is None:
            from mcp.server.fastmcp import FastMCP  # type: ignore[import]

            self._mcp = FastMCP(self._name)
        return self._mcp

    def tool(self) -> Callable[[F], F]:
        """Decorator that registers a function as an MCP tool.

        Delegates directly to ``FastMCP.tool()``.

        Example
        -------
        ::

            @app.tool()
            def my_tool(x: int) -> str:
                return str(x)
        """
        return self._get_mcp().tool()  # type: ignore[no-any-return]

    def run(self) -> None:
        """Start the MCP server with the stdio transport.

        This is the main entry point; call it from ``__main__``.  The server
        reads JSON-RPC messages from stdin and writes responses to stdout;
        all log output goes to stderr.
        """
        _log.info("spur_app.App(%r) starting stdio transport", self._name)
        self._get_mcp().run(transport="stdio")
