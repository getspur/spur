from __future__ import annotations

import base64
import json
import math
import os
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any


METHOD = "html_video_render"
DEFAULT_FPS = 30
DEFAULT_RESOLUTION = "1280x720"
DEFAULT_FRAME_DURATION = 3.0
HYPERFRAMES_FPS_WARNING_THRESHOLD = 240

ENGINE_DIR = Path(__file__).resolve().parent.parent / "engine"
HYPERFRAMES_SCRIPT = ENGINE_DIR / "render-hf.mjs"
HYPERFRAMES_ENGINE_MODULE = ENGINE_DIR / "node_modules" / "@hyperframes" / "engine" / "package.json"
HYPERFRAMES_PUPPETEER_MODULE = ENGINE_DIR / "node_modules" / "puppeteer" / "package.json"


class RenderError(Exception):
    def __init__(self, message: str, details: dict[str, Any] | None = None) -> None:
        super().__init__(message)
        self.message = message
        self.details = details or {}

    def __str__(self) -> str:
        if not self.details:
            return self.message
        return f"{self.message}: {json.dumps(self.details, sort_keys=True)}"


class InvalidParams(RenderError):
    pass


@dataclass(frozen=True)
class RenderDimensions:
    width: int
    height: int


@dataclass(frozen=True)
class RenderOptions:
    fps: int
    resolution: RenderDimensions
    total_duration: float


def render_html_video(
    *,
    webm_frames: list[str] | None = None,
    port_names: list[str] | None = None,
    output_path: str,
    composition_html: str | None = None,
    duration: float | None = None,
    resolution: str | None = None,
    fps: int | None = None,
    frame_duration: float | None = None,
) -> dict[str, Any]:
    webm_frames = webm_frames or []
    port_names = port_names or []
    resolved_fps = max(int(fps or DEFAULT_FPS), 1)
    dimensions = parse_resolution(resolution or DEFAULT_RESOLUTION)
    normalized_output_path = normalize_output_path(output_path)
    if normalized_output_path.parent:
        normalized_output_path.parent.mkdir(parents=True, exist_ok=True)

    if composition_html is not None:
        composition_html = composition_html.strip()
        if not composition_html:
            raise InvalidParams(
                f"{METHOD} composition_html cannot be empty",
                {"code": "invalid_composition_html"},
            )
        resolved_duration = _resolve_render_duration(
            duration=duration,
            frame_duration=frame_duration,
        )
        ensure_hyperframes_runtime_available()
        return run_node_render(
            composition_html=composition_html,
            output_path=normalized_output_path,
            duration=resolved_duration,
            fps=resolved_fps,
            resolution=dimensions,
        )

    if not webm_frames and not port_names:
        raise InvalidParams(
            f"{METHOD} requires at least one webm_frame or port_name",
            {"code": "missing_frames"},
        )

    explicit_frame_duration: float | None = (
        float(frame_duration)
        if frame_duration is not None
        and math.isfinite(float(frame_duration))
        and float(frame_duration) > 0.0
        else None
    )

    if port_names:
        # read_webm_port_frames returns (bytes, duration_sec_or_None) tuples.
        port_frame_pairs = read_webm_port_frames(port_names)
        frame_bytes = [pair[0] for pair in port_frame_pairs]
        # Per-frame duration: explicit arg wins; fall back to manifest value,
        # then to DEFAULT_FRAME_DURATION as last resort.
        per_frame_durations = [
            explicit_frame_duration
            if explicit_frame_duration is not None
            else (pair[1] if pair[1] is not None else DEFAULT_FRAME_DURATION)
            for pair in port_frame_pairs
        ]
    else:
        frame_bytes = decode_webm_frames(webm_frames)
        per_frame_durations = [
            explicit_frame_duration if explicit_frame_duration is not None else DEFAULT_FRAME_DURATION
            for _ in frame_bytes
        ]

    total_duration = sum(per_frame_durations)
    ensure_ffmpeg_available()

    options = RenderOptions(
        fps=resolved_fps,
        resolution=dimensions,
        total_duration=total_duration,
    )
    with tempfile.TemporaryDirectory(prefix="spur-html-video-render-") as scratch:
        scratch_dir = Path(scratch)
        frame_webm_paths = write_webm_frames(frame_bytes, scratch_dir)
        if len(frame_webm_paths) == 1:
            encode_single_frame(
                frame_webm_paths[0],
                normalized_output_path,
                options,
                per_frame_durations[0],
            )
        else:
            encode_frame_sequence(
                frame_webm_paths,
                normalized_output_path,
                scratch_dir,
                options,
                per_frame_durations,
            )

    return {
        "output_path": str(normalized_output_path),
        "frame_count": len(frame_bytes),
        "fps": options.fps,
        "duration": options.total_duration,
        "resolution": f"{options.resolution.width}x{options.resolution.height}",
    }


def _resolve_render_duration(duration: float | None, frame_duration: float | None) -> float:
    if duration is None:
        if frame_duration is None:
            raise InvalidParams(
                f"{METHOD} requires duration for engine-backed render mode",
                {"code": "missing_duration"},
            )
        return float(frame_duration)
    return float(duration)


def parse_resolution(raw: str) -> RenderDimensions:
    parts = raw.split("x", 1)
    if len(parts) != 2:
        raise InvalidParams(
            f"{METHOD} resolution must be WIDTHxHEIGHT",
            {"resolution": raw},
        )

    width_raw, height_raw = parts
    try:
        width = int(width_raw)
    except ValueError as error:
        raise InvalidParams(
            f"{METHOD} resolution width must be a positive integer",
            {"resolution": raw},
        ) from error

    try:
        height = int(height_raw)
    except ValueError as error:
        raise InvalidParams(
            f"{METHOD} resolution height must be a positive integer",
            {"resolution": raw},
        ) from error

    if width <= 0 or height <= 0:
        raise InvalidParams(
            f"{METHOD} resolution width and height must be > 0",
            {"resolution": raw},
        )

    return RenderDimensions(width=width, height=height)


def normalize_output_path(path: str) -> Path:
    output_path = Path(path)
    if output_path.suffix.lower() != ".mp4":
        output_path = output_path.with_suffix(".mp4")
    return output_path


def ensure_bun_available() -> str:
    bun_path = shutil.which("bun")
    if bun_path is None:
        raise RenderError(
            "html_video_render requires bun in PATH for HyperFrames render mode",
            {"code": "bun_unavailable"},
        )
    return bun_path


def ensure_hyperframes_runtime_available() -> None:
    if not HYPERFRAMES_SCRIPT.is_file():
        raise RenderError(
            "html_video_render requires app_gallery/html_video/engine/render-hf.mjs",
            {"code": "hyperframes_script_missing"},
        )
    if not HYPERFRAMES_ENGINE_MODULE.is_file():
        raise RenderError(
            "html_video_render requires @hyperframes/engine to be installed in app_gallery/html_video/engine/node_modules",
            {"code": "hyperframes_engine_missing"},
        )
    if not HYPERFRAMES_PUPPETEER_MODULE.is_file():
        raise RenderError(
            "html_video_render requires puppeteer to be installed in app_gallery/html_video/engine/node_modules",
            {"code": "puppeteer_missing"},
        )


def run_node_render(
    composition_html: str,
    output_path: Path,
    duration: float,
    fps: int,
    resolution: RenderDimensions,
) -> dict[str, Any]:
    if not math.isfinite(duration) or duration <= 0.0:
        raise InvalidParams(
            f"{METHOD} duration must be positive",
            {"code": "invalid_duration"},
        )

    if fps > HYPERFRAMES_FPS_WARNING_THRESHOLD:
        raise InvalidParams(
            f"{METHOD} fps must be <= {HYPERFRAMES_FPS_WARNING_THRESHOLD}",
            {"code": "invalid_fps"},
        )

    composition_path = Path(composition_html).expanduser()
    if not composition_path.is_absolute():
        composition_path = composition_path.resolve()
    if not composition_path.exists():
        raise InvalidParams(
            f"{METHOD} composition_html must exist",
            {"code": "composition_not_found", "composition_html": composition_html},
        )

    frame_count = max(1, int(math.floor(duration * fps)))
    command = [
        ensure_bun_available(),
        str(HYPERFRAMES_SCRIPT),
        str(composition_path),
        f"{duration}",
        str(fps),
        f"{resolution.width}x{resolution.height}",
        str(output_path),
    ]
    status = subprocess.run(
        command,
        cwd=str(ENGINE_DIR),
        capture_output=True,
        text=True,
    )
    if status.returncode != 0:
        stderr = status.stderr.strip()
        stdout = status.stdout.strip()
        details = {"code": "hyperframes_render_failed", "command": command[0]}
        if stderr:
            details["stderr"] = stderr
        if stdout:
            details["stdout"] = stdout
        raise RenderError(
            "html_video_render failed while running HyperFrames Bun harness",
            details,
        )
    if not output_path.is_file() or output_path.stat().st_size <= 0:
        raise RenderError(
            "html_video_render produced an empty HyperFrames output",
            {"code": "hyperframes_output_invalid", "path": str(output_path)},
        )

    return {
        "output_path": str(output_path),
        "frame_count": frame_count,
        "fps": fps,
        "duration": duration,
        "resolution": f"{resolution.width}x{resolution.height}",
        "render_mode": "hyperframes-bun",
    }


def ensure_ffmpeg_available() -> None:
    if shutil.which("ffmpeg") is None:
        raise RenderError(
            "html_video_render requires ffmpeg in PATH",
            {"code": "ffmpeg_unavailable"},
        )


def decode_webm_frames(webm_frames: list[str]) -> list[bytes]:
    decoded: list[bytes] = []
    for index, encoded in enumerate(webm_frames):
        try:
            decoded.append(base64.b64decode(encoded, validate=True))
        except Exception as error:
            raise InvalidParams(
                f"{METHOD} received invalid base64 webm data",
                {
                    "code": "invalid_webm_frame_base64",
                    "frame_index": index,
                    "error": str(error),
                },
            ) from error
    return decoded


def read_webm_port_frames(
    port_names: list[str],
) -> list[tuple[bytes, float | None]]:
    """Read WebM bytes (and optional duration) for each named port from the
    port store manifest.

    Returns a list of ``(bytes, duration_sec_or_None)`` tuples — one per port
    in the order they were requested.  ``duration_sec`` is ``None`` when the
    manifest entry does not carry the field (old stores).

    The physical path of each port file is taken from ``entry["path"]`` in the
    manifest, NOT derived as ``root/<port>``.  For safety the path is resolved
    as ``root / Path(entry["path"]).name`` so only the basename is used.
    """
    root_raw = os.environ.get("SPUR_PORTS_ROOT")
    if not root_raw:
        raise RenderError(
            f"{METHOD} port_names require SPUR_PORTS_ROOT",
            {"code": "ports_root_unavailable"},
        )

    root = Path(root_raw)
    manifest_path = root / "manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text())
    except Exception as error:
        raise RenderError(
            f"{METHOD} failed to open notebook port store",
            {"error": str(error)},
        ) from error

    ports = manifest.get("ports", {}) if isinstance(manifest, dict) else {}
    if not isinstance(ports, dict):
        raise RenderError(
            f"{METHOD} failed to open notebook port store",
            {"error": "manifest ports must be an object"},
        )

    frames: list[tuple[bytes, float | None]] = []
    for port in port_names:
        entry = ports.get(port)
        if not isinstance(entry, dict):
            raise InvalidParams(
                f"{METHOD} could not read media port",
                {"port": port, "error": "port not found"},
            )

        mime = entry.get("mime")
        if mime != "video/webm":
            raise InvalidParams(
                f"{METHOD} media port must be video/webm",
                {"port": port, "mime": mime},
            )

        # Read from the path recorded in the manifest entry.  The file is
        # written as ``<port>@v<N>.media`` — never as bare ``<port>``.
        # For safety only the basename is used so a malicious manifest entry
        # cannot escape the root directory.
        entry_path_raw = entry.get("path", "")
        port_file_path = root / Path(entry_path_raw).name if entry_path_raw else None
        if not port_file_path or not port_file_path.name:
            raise InvalidParams(
                f"{METHOD} could not read media port",
                {"port": port, "error": "manifest entry is missing a path"},
            )

        try:
            frame_bytes = port_file_path.read_bytes()
        except Exception as error:
            raise InvalidParams(
                f"{METHOD} could not read media port",
                {"port": port, "error": str(error)},
            ) from error

        raw_duration = entry.get("duration_sec")
        duration_sec: float | None = (
            float(raw_duration)
            if raw_duration is not None and math.isfinite(float(raw_duration)) and float(raw_duration) > 0.0
            else None
        )
        frames.append((frame_bytes, duration_sec))

    return frames


def write_webm_frames(webm_frames: list[bytes], scratch_dir: Path) -> list[Path]:
    frame_webm_paths: list[Path] = []
    for index, data in enumerate(webm_frames):
        path = scratch_dir / f"spur-html-video-frame-{index}.webm"
        try:
            path.write_bytes(data)
        except Exception as error:
            raise RenderError(
                f"{METHOD} failed to write temporary webm frame",
                {"frame_index": index, "error": str(error)},
            ) from error
        frame_webm_paths.append(path)
    return frame_webm_paths


def encode_single_frame(
    frame_webm_path: Path,
    output_path: Path,
    options: RenderOptions,
    duration_seconds: float,
) -> None:
    run_ffmpeg_frame_encode(frame_webm_path, output_path, options, duration_seconds)


def encode_frame_sequence(
    frame_webm_paths: list[Path],
    output_path: Path,
    scratch_dir: Path,
    options: RenderOptions,
    per_frame_durations: list[float],
) -> None:
    segment_paths: list[Path] = []
    for index, frame_webm_path in enumerate(frame_webm_paths):
        segment_path = scratch_dir / f"spur-html-video-segment-{index}.mp4"
        frame_duration = (
            per_frame_durations[index]
            if index < len(per_frame_durations)
            else DEFAULT_FRAME_DURATION
        )
        run_ffmpeg_frame_encode(frame_webm_path, segment_path, options, frame_duration)
        segment_paths.append(segment_path)

    list_path = scratch_dir / "html-video-concat-list.txt"
    list_contents = "".join(f"file '{_escape_concat_path(path)}'\n" for path in segment_paths)
    try:
        list_path.write_text(list_contents)
    except Exception as error:
        raise RenderError(
            "html_video_render failed to write ffmpeg concat list",
            {"error": str(error)},
        ) from error

    status = subprocess.run(
        [
            "ffmpeg",
            "-y",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            str(list_path),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            str(output_path),
        ],
        capture_output=True,
        text=True,
    )
    if status.returncode != 0:
        raise RenderError(
            "html_video_render failed to concat rendered frame segments",
            {"code": "ffmpeg_concat_failed", "stderr": status.stderr.strip()},
        )


def run_ffmpeg_frame_encode(
    frame_webm_path: Path,
    output_path: Path,
    options: RenderOptions,
    duration_seconds: float,
) -> None:
    status = subprocess.run(
        [
            "ffmpeg",
            "-y",
            "-loglevel",
            "error",
            "-i",
            str(frame_webm_path),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-r",
            str(options.fps),
            "-t",
            f"{duration_seconds:.6f}",
            "-vf",
            f"scale={options.resolution.width}:{options.resolution.height}",
            str(output_path),
        ],
        capture_output=True,
        text=True,
    )
    if status.returncode != 0:
        raise RenderError(
            f"{METHOD} failed to encode frame",
            {"code": "ffmpeg_encode_failed", "stderr": status.stderr.strip()},
        )


def _escape_concat_path(path: Path) -> str:
    return str(path).replace("'", "'\\''")
