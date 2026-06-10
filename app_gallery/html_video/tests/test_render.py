from __future__ import annotations

import base64
import json
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


# ── T6a: read_webm_port_frames fixture-based tests ────────────────────────────


def _make_port_store(tmp_path: Path, duration_sec: float | None = None) -> tuple[Path, Path]:
    """Create a minimal port store fixture in tmp_path.

    Returns (ports_root, media_file_path).
    """
    ports_root = tmp_path / "ports"
    ports_root.mkdir()
    media_file = ports_root / "spur-ad-capture@v1.media"
    media_file.write_bytes(b"fake webm bytes")
    entry: dict = {
        "path": str(media_file),
        "version": 1,
        "kind": "media",
        "mime": "video/webm",
        "size": len(b"fake webm bytes"),
    }
    if duration_sec is not None:
        entry["duration_sec"] = duration_sec
    manifest = {"ports": {"spur-ad-capture": entry}}
    (ports_root / "manifest.json").write_text(json.dumps(manifest))
    return ports_root, media_file


def test_read_webm_port_frames_reads_entry_path_not_bare_port_name(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """Port bytes must come from entry['path'] (basename-joined under root),
    not from root/<port-name>."""
    ports_root, media_file = _make_port_store(tmp_path)
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(ports_root))

    frames = render.read_webm_port_frames(["spur-ad-capture"])

    assert len(frames) == 1
    frame_bytes, frame_duration = frames[0]
    assert frame_bytes == b"fake webm bytes"
    assert frame_duration is None  # no duration_sec in this fixture


def test_read_webm_port_frames_surfaces_duration_sec_from_manifest(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """When the manifest entry has duration_sec, it is returned alongside the bytes."""
    ports_root, _ = _make_port_store(tmp_path, duration_sec=60.0)
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(ports_root))

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
    ports_root, _ = _make_port_store(tmp_path, duration_sec=60.0)
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(ports_root))

    calls: list[tuple] = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render.shutil, "which", lambda _name: "/usr/bin/ffmpeg")
    monkeypatch.setattr(render.subprocess, "run", fake_run)

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
    ports_root, _ = _make_port_store(tmp_path, duration_sec=60.0)
    monkeypatch.setenv("SPUR_PORTS_ROOT", str(ports_root))

    calls: list[tuple] = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(render.shutil, "which", lambda _name: "/usr/bin/ffmpeg")
    monkeypatch.setattr(render.subprocess, "run", fake_run)

    result = render.render_html_video(
        port_names=["spur-ad-capture"],
        output_path=str(tmp_path / "out"),
        resolution="320x180",
        fps=30,
        frame_duration=5.0,  # explicit: should win over manifest's 60.0
    )

    assert result["duration"] == 5.0
