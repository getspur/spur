from __future__ import annotations

from typing import Any

import library


DEFAULT_TOP = 5


def html_video_search_templates(intent: str, top: int | None = None) -> dict[str, Any]:
    """Search bundled html video templates."""
    resolved_top = DEFAULT_TOP if top is None else top
    if resolved_top < 1:
        raise ValueError("html_video_search_templates top must be >= 1")
    return {"items": library.search(intent, resolved_top)}
