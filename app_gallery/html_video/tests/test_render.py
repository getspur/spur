from __future__ import annotations

import base64
import shutil
import subprocess
from pathlib import Path

import pytest

from spur_app.testing import FakePortStore

render = pytest.importorskip("server.render")

FAKE_WEBM_BYTES = b"fake webm bytes"


def test_render_error_str_with_details_uses_json() -> None:
    """RenderError.__str__ must not raise NameError when details are present.

    This guards against accidentally removing `import json` from render.py:
    the __str__ method calls json.dumps() on the details dict.
    """
    err = render.RenderError("something went wrong", {"code": "oops", "count": 3})

    text = str(err)

    assert "something went wrong" in text
    assert '"code"' in text
    assert "oops" in text
    assert '"count"' in text
    assert "3" in text


def test_render_error_str_without_details_is_plain_message() -> None:
    """RenderError.__str__ returns just the message when details are empty."""
    err = render.RenderError("plain message")

    assert str(err) == "plain message"


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


# ── T6a / U8: read_webm_port_frames tests via FakePortStore ──────────────────


def test_read_webm_port_frames_reads_entry_path_not_bare_port_name() -> None:
    """Port bytes must come from entry['path'] (basename-joined under root),
    not from root/<port-name>."""
    with FakePortStore().add_media(
        "spur-ad-capture",
        FAKE_WEBM_BYTES,
        mime="video/webm",
    ):
        frames = render.read_webm_port_frames(["spur-ad-capture"])

    assert len(frames) == 1
    frame_bytes, frame_duration = frames[0]
    assert frame_bytes == FAKE_WEBM_BYTES
    assert frame_duration is None  # no duration_sec in this fixture


def test_read_webm_port_frames_surfaces_duration_sec_from_manifest() -> None:
    """When the manifest entry has duration_sec, it is returned alongside the bytes."""
    with FakePortStore().add_media(
        "spur-ad-capture",
        FAKE_WEBM_BYTES,
        mime="video/webm",
        duration_sec=60.0,
    ):
        frames = render.read_webm_port_frames(["spur-ad-capture"])

    assert len(frames) == 1
    _, frame_duration = frames[0]
    assert frame_duration == 60.0


def test_render_html_video_port_names_uses_manifest_duration_when_frame_duration_absent(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """When frame_duration is not passed and the manifest carries duration_sec=60,
    the rendered total_duration must be 60 (not the 3.0 default)."""
    calls: list[tuple] = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render.shutil, "which", lambda _name: "/usr/bin/ffmpeg")
    monkeypatch.setattr(render.subprocess, "run", fake_run)

    with FakePortStore().add_media(
        "spur-ad-capture",
        FAKE_WEBM_BYTES,
        mime="video/webm",
        duration_sec=60.0,
    ):
        result = render.render_html_video(
            port_names=["spur-ad-capture"],
            output_path=str(tmp_path / "out"),
            resolution="320x180",
            fps=30,
            # frame_duration intentionally omitted — should come from manifest
        )

    assert result["duration"] == 60.0
    assert result["frame_count"] == 1


def test_render_html_video_port_names_explicit_frame_duration_overrides_manifest(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """When frame_duration is explicitly passed, it overrides the manifest duration_sec."""
    calls: list[tuple] = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render.shutil, "which", lambda _name: "/usr/bin/ffmpeg")
    monkeypatch.setattr(render.subprocess, "run", fake_run)

    with FakePortStore().add_media(
        "spur-ad-capture",
        FAKE_WEBM_BYTES,
        mime="video/webm",
        duration_sec=60.0,
    ):
        result = render.render_html_video(
            port_names=["spur-ad-capture"],
            output_path=str(tmp_path / "out"),
            resolution="320x180",
            fps=30,
            frame_duration=5.0,  # explicit: should win over manifest's 60.0
        )

    assert result["duration"] == 5.0
