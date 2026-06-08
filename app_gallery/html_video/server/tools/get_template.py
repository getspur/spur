from __future__ import annotations

from typing import Any

import library


def html_video_get_template(id: str) -> dict[str, Any]:
    """Fetch one bundled html video template with template HTML and metadata."""
    template = library.get_template(id)
    return {
        "id": template["metadata"]["id"],
        "metadata": template["metadata"],
        "html": template["html"],
        "skill_md": template["skill_md"],
    }
