from __future__ import annotations

from typing import Any

import render as render_pipeline


def html_video_render(
    output_path: str,
    webm_frames: list[str] | None = None,
    port_names: list[str] | None = None,
    resolution: str | None = None,
    fps: int | None = None,
    frame_duration: float | None = None,
) -> dict[str, Any]:
    """Render webm media ports or base64-encoded webm frames into an mp4 file."""
    return render_pipeline.render_html_video(
        webm_frames=webm_frames,
        port_names=port_names,
        output_path=output_path,
        resolution=resolution,
        fps=fps,
        frame_duration=frame_duration,
    )
