"""pytest configuration for the html_video app tests.

Adds the app root to sys.path so that ``import server.render``,
``import server.library`` etc. work from tests/.
"""
from __future__ import annotations

import sys
from pathlib import Path

# Make the app root importable so tests can do ``import server.render``.
_APP_ROOT = Path(__file__).resolve().parent
if str(_APP_ROOT) not in sys.path:
    sys.path.insert(0, str(_APP_ROOT))
