from __future__ import annotations

import base64
import shutil
import subprocess
from pathlib import Path

import pytest


render = pytest.importorskip("server.render")


def test_parse_resolution_valid() -> None:
    dimensions = render.parse_resolution("1920x1080")

    assert (dimensions.width, dimensions.height) == (1920, 1080)


def test_parse_resolution_invalid_raises() -> None:
    with pytest.raises(render.InvalidParams):
        render.parse_resolution("abc")


def test_normalize_output_path_adds_mp4() -> None:
    assert str(render.normalize_output_path("/tmp/out")) == "/tmp/out.mp4"


def test_ensure_ffmpeg_available_raises_when_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda _name: None)

    with pytest.raises(render.RenderError):
        render.ensure_ffmpeg_available()


def test_render_html_video_single_frame_mocks_ffmpeg(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path,
) -> None:
    calls = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render.shutil, "which", lambda _name: "/usr/bin/ffmpeg")
    monkeypatch.setattr(render.subprocess, "run", fake_run)

    result = render.render_html_video(
        webm_frames=[base64.b64encode(b"fake webm").decode("ascii")],
        output_path=str(tmp_path / "out"),
        resolution="320x180",
        fps=12,
        frame_duration=0.5,
    )

    assert result == {
        "output_path": str(tmp_path / "out.mp4"),
        "frame_count": 1,
        "fps": 12,
        "duration": 0.5,
        "resolution": "320x180",
    }
    assert len(calls) == 1
    assert calls[0][0][0] == "ffmpeg"


def test_render_html_video_composition_html_invokes_bun_harness(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    composition_html = tmp_path / "template.html"
    composition_html.write_text("<html></html>")
    calls: list[list[str]] = []

    def fake_run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        Path(command[-1]).write_bytes(b"fake mp4")
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render, "ensure_bun_available", lambda: "/usr/bin/bun")
    monkeypatch.setattr(render, "ensure_hyperframes_runtime_available", lambda: None)
    monkeypatch.setattr(render.subprocess, "run", fake_run)

    result = render.render_html_video(
        composition_html=str(composition_html),
        output_path=str(tmp_path / "out"),
        duration=2.0,
        fps=24,
        resolution="640x360",
    )

    assert result == {
        "output_path": str(tmp_path / "out.mp4"),
        "frame_count": 48,
        "fps": 24,
        "duration": 2.0,
        "resolution": "640x360",
        "render_mode": "hyperframes-bun",
    }
    assert calls
    assert calls[0][0] == "/usr/bin/bun"
    assert calls[0][1].endswith("render-hf.mjs")
    assert calls[0][2] == str(composition_html)
    assert calls[0][3] == "2.0"
    assert calls[0][4] == "24"
    assert calls[0][5] == "640x360"
    assert calls[0][6] == str(tmp_path / "out.mp4")
