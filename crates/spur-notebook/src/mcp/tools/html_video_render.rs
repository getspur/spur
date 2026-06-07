use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;
use uuid::Uuid;

use crate::mcp::ServerDeps;

const METHOD: &str = "html_video_render";
const DEFAULT_FPS: u32 = 30;
const DEFAULT_RESOLUTION: &str = "1280x720";
const DEFAULT_FRAME_DURATION: f64 = 3.0;

#[derive(Debug, Deserialize)]
struct HtmlVideoRenderParams {
    webm_frames: Vec<String>,
    output_path: String,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    fps: Option<u32>,
    #[serde(default)]
    frame_duration: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct RenderDimensions {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy)]
struct RenderOptions {
    fps: u32,
    resolution: RenderDimensions,
    total_duration: f64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Render base64-encoded webm frames into an mp4 file using ffmpeg.",
        rmcp_object(json!({
            "type": "object",
            "required": ["webm_frames", "output_path"],
            "properties": {
                "webm_frames": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "string",
                        "minLength": 1
                    }
                },
                "output_path": { "type": "string", "minLength": 1 },
                "resolution": { "type": "string", "pattern": "^\\d+x\\d+$" },
                "fps": { "type": "integer", "minimum": 1 },
                "frame_duration": { "type": "number", "minimum": 0.01 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: HtmlVideoRenderParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ webm_frames, output_path, resolution?, fps?, frame_duration? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    if params.webm_frames.is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} requires at least one webm_frame"),
            Some(json!({ "code": "missing_webm_frames" })),
        ));
    }

    let fps = params.fps.unwrap_or(DEFAULT_FPS).max(1);
    let resolution = parse_resolution(
        params
            .resolution
            .unwrap_or_else(|| DEFAULT_RESOLUTION.to_string()),
    )?;
    let frame_duration = params
        .frame_duration
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_FRAME_DURATION);
    let total_duration = frame_duration * params.webm_frames.len() as f64;

    let output_path = normalize_output_path(&params.output_path);
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            McpError::internal_error(
                format!("{METHOD} could not prepare output directory"),
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    }
    ensure_ffmpeg_available().await?;

    let scratch_dir =
        std::env::temp_dir().join(format!("spur-html-video-render-{}", Uuid::new_v4()));
    fs::create_dir_all(&scratch_dir).map_err(|error| {
        McpError::internal_error(
            format!("{METHOD} failed to create temporary render directory"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let options = RenderOptions {
        fps,
        resolution,
        total_duration,
    };

    let frame_webm_paths = write_webm_frames(&params.webm_frames, &scratch_dir)?;

    if frame_webm_paths.len() == 1 {
        encode_single_frame(&frame_webm_paths[0], &output_path, options, frame_duration).await?;
    } else {
        encode_frame_sequence(
            &frame_webm_paths,
            &output_path,
            &scratch_dir,
            options,
            frame_duration,
        )
        .await?;
    }

    Ok(CallToolResult::structured(json!({
        "output_path": output_path.to_string_lossy(),
        "frame_count": frame_webm_paths.len(),
        "fps": options.fps,
        "duration": options.total_duration,
        "resolution": format!("{}x{}", options.resolution.width, options.resolution.height),
    })))
}

fn parse_resolution(raw: String) -> Result<RenderDimensions, McpError> {
    let (width, height) = raw.split_once('x').ok_or_else(|| {
        McpError::invalid_params(
            format!("{METHOD} resolution must be WIDTHxHEIGHT"),
            Some(json!({ "resolution": raw })),
        )
    })?;
    let width = width.parse::<u32>().map_err(|_| {
        McpError::invalid_params(
            format!("{METHOD} resolution width must be a positive integer"),
            Some(json!({ "resolution": raw })),
        )
    })?;
    let height = height.parse::<u32>().map_err(|_| {
        McpError::invalid_params(
            format!("{METHOD} resolution height must be a positive integer"),
            Some(json!({ "resolution": raw })),
        )
    })?;
    if width == 0 || height == 0 {
        return Err(McpError::invalid_params(
            format!("{METHOD} resolution width and height must be > 0"),
            Some(json!({ "resolution": raw })),
        ));
    }
    Ok(RenderDimensions { width, height })
}

fn normalize_output_path(path: &str) -> PathBuf {
    let mut path = PathBuf::from(path);
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
    {
        path.set_extension("mp4");
    }
    path
}

async fn ensure_ffmpeg_available() -> Result<(), McpError> {
    let status = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await
        .map_err(|error| {
            McpError::internal_error(
                "html_video_render requires ffmpeg in PATH, but it could not be launched",
                Some(json!({ "code": "ffmpeg_unavailable", "error": error.to_string() })),
            )
        })?;

    if !status.status.success() {
        return Err(McpError::internal_error(
            "html_video_render requires ffmpeg in PATH",
            Some(json!({ "code": "ffmpeg_unavailable" })),
        ));
    }
    Ok(())
}

fn write_webm_frames(webm_frames: &[String], scratch_dir: &Path) -> Result<Vec<PathBuf>, McpError> {
    let mut frame_webm_paths = Vec::with_capacity(webm_frames.len());
    for (index, encoded) in webm_frames.iter().enumerate() {
        let bytes = STANDARD.decode(encoded).map_err(|error| {
            McpError::invalid_params(
                format!("{METHOD} received invalid base64 webm data"),
                Some(json!({
                    "code": "invalid_webm_frame_base64",
                    "frame_index": index,
                    "error": error.to_string(),
                })),
            )
        })?;
        let path = scratch_dir.join(format!("spur-html-video-frame-{index}.webm"));
        fs::write(&path, bytes).map_err(|error| {
            McpError::internal_error(
                format!("{METHOD} failed to write temporary webm frame"),
                Some(json!({ "frame_index": index, "error": error.to_string() })),
            )
        })?;
        frame_webm_paths.push(path);
    }
    Ok(frame_webm_paths)
}

async fn encode_single_frame(
    frame_webm_path: &Path,
    output_path: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    run_ffmpeg_frame_encode(frame_webm_path, output_path, options, duration_seconds).await
}

async fn encode_frame_sequence(
    frame_webm_paths: &[PathBuf],
    output_path: &Path,
    scratch_dir: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    let segments_dir = scratch_dir;
    let mut segment_paths = Vec::with_capacity(frame_webm_paths.len());

    for (index, frame_webm_path) in frame_webm_paths.iter().enumerate() {
        let segment_path = segments_dir.join(format!("spur-html-video-segment-{}.mp4", index));
        run_ffmpeg_frame_encode(frame_webm_path, &segment_path, options, duration_seconds).await?;
        segment_paths.push(segment_path);
    }

    let list_path = segments_dir.join("html-video-concat-list.txt");
    let mut list_contents = String::new();
    for segment_path in &segment_paths {
        writeln!(
            &mut list_contents,
            "file '{}'",
            segment_path.to_string_lossy().replace('\'', "'\\''")
        )
        .map_err(|error| {
            McpError::internal_error(
                "html_video_render failed to build ffmpeg concat list",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    }
    fs::write(&list_path, list_contents).map_err(|error| {
        McpError::internal_error(
            "html_video_render failed to write ffmpeg concat list",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&list_path)
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output_path)
        .output()
        .await
        .map_err(|error| {
            McpError::internal_error(
                "html_video_render failed to launch ffmpeg concat",
                Some(json!({ "error": error.to_string(), "code": "ffmpeg_concat_launch_failed" })),
            )
        })?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(McpError::internal_error(
            "html_video_render failed to concat rendered frame segments",
            Some(json!({ "code": "ffmpeg_concat_failed", "stderr": stderr.trim() })),
        ));
    }
    Ok(())
}

async fn run_ffmpeg_frame_encode(
    frame_webm_path: &Path,
    output_path: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(frame_webm_path)
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-r")
        .arg(options.fps.to_string())
        .arg("-t")
        .arg(format!("{:.6}", duration_seconds))
        .arg("-vf")
        .arg(format!(
            "scale={}:{}",
            options.resolution.width, options.resolution.height
        ))
        .arg(output_path)
        .output()
        .await
        .map_err(|error| {
            McpError::internal_error(
                format!("{METHOD} failed to launch ffmpeg"),
                Some(json!({ "error": error.to_string(), "code": "ffmpeg_launch_failed" })),
            )
        })?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(McpError::internal_error(
            format!("{METHOD} failed to encode frame"),
            Some(json!({ "code": "ffmpeg_encode_failed", "stderr": stderr.trim() })),
        ));
    }
    Ok(())
}
