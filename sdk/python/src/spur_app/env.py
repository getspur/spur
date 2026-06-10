"""Typed env-var accessors for spur_app.

:class:`EnvAccessor` wraps ``os.environ`` with accessors for manifest-declared
env vars (the ``env`` block of ``spur-app.json``).  It deliberately does NOT
read ``spur-app.json`` at runtime — that is a host concern.  The accessor
simply provides clear error messages when a var is absent.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

from .errors import EnvVarRequiredError


class EnvAccessor:
    """Typed accessors for manifest-declared environment variables.

    Obtained via :attr:`App.env`.  There is no per-instance state beyond an
    optional namespace prefix; all reads go to ``os.environ`` at call time.
    """

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def get(self, name: str, default: Optional[str] = None) -> Optional[str]:
        """Return the value of env var *name*, or *default* if absent.

        Parameters
        ----------
        name:
            Env var name, e.g. ``"TEMPLATES_DIR"``.
        default:
            Value to return when the var is absent.  Defaults to ``None``.
        """
        return os.environ.get(name, default)

    def require(self, name: str) -> str:
        """Return the value of env var *name*, raising if absent.

        Parameters
        ----------
        name:
            Env var name, e.g. ``"TEMPLATES_DIR"``.

        Raises
        ------
        EnvVarRequiredError
            When *name* is not set in the environment.
        """
        value = os.environ.get(name)
        if value is None:
            raise EnvVarRequiredError(name)
        return value

    def path(self, name: str) -> Path:
        """Return the value of env var *name* as a :class:`~pathlib.Path`.

        Parameters
        ----------
        name:
            Env var name, e.g. ``"TEMPLATES_DIR"``.

        Raises
        ------
        EnvVarRequiredError
            When *name* is not set in the environment.
        """
        return Path(self.require(name))
