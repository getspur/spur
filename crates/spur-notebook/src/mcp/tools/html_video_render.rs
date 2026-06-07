use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use uuid::Uuid;

use crate::mcp::ServerDeps;

const METHOD: &str = "html_video_render";
const DEFAULT_FPS: u32 = 30;
const DEFAULT_RESOLUTION: &str = "1280x720";

#[derive(Debug, Deserialize)]
struct HtmlVideoRenderParams {
    frame_html_paths: Vec<String>,
    output_path: String,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    fps: Option<u32>,
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct RenderDimensions {
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct RenderOptions {
    fps: u32,
    resolution: RenderDimensions,
    total_duration: f64,
}

#[derive(Serialize)]
struct PlaywrightCaptureConfig {
    frame_html_paths: Vec<String>,
    output_dir: String,
    output_json: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct PlaywrightCaptureOutput {
    frame_png_paths: Vec<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Render html frames into an mp4 file using Playwright + ffmpeg.",
        rmcp_object(json!({
            "type": "object",
            "required": ["frame_html_paths", "output_path"],
            "properties": {
                "frame_html_paths": {
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
                "duration": { "type": "number", "minimum": 0.01 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: HtmlVideoRenderParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ frame_html_paths, output_path, resolution?, fps?, duration? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    if params.frame_html_paths.is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} requires at least one frame_html_path"),
            Some(json!({ "code": "missing_frame_paths" })),
        ));
    }

    let fps = params.fps.unwrap_or(DEFAULT_FPS).max(1);
    let resolution = parse_resolution(
        params
            .resolution
            .unwrap_or_else(|| DEFAULT_RESOLUTION.to_string()),
    )?;
    let total_duration = crate::html_video::default_render_duration(
        params.frame_html_paths.len(),
        fps,
        params.duration,
    );
    let frame_duration = if params.frame_html_paths.len() <= 1 {
        total_duration
    } else {
        total_duration / params.frame_html_paths.len() as f64
    };

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

    for frame_html_path in &params.frame_html_paths {
        let path = Path::new(frame_html_path);
        if !path.is_file() {
            return Err(McpError::invalid_params(
                format!("{METHOD} received a missing frame html path"),
                Some(json!({ "path": frame_html_path })),
            ));
        }
    }

    match capture_frame_pngs(&params.frame_html_paths, &scratch_dir, options.resolution).await {
        Ok(frame_png_paths) => {
            if frame_png_paths.is_empty() {
                return Err(McpError::internal_error(
                    format!("{METHOD} produced no frames from Playwright"),
                    Some(json!({ "code": "render_capture_frames_empty" })),
                ));
            }

            if frame_png_paths.len() == 1 {
                encode_single_frame(&frame_png_paths[0], &output_path, options, frame_duration)
                    .await?;
            } else {
                encode_frame_sequence(
                    &frame_png_paths,
                    &output_path,
                    &scratch_dir,
                    options,
                    frame_duration,
                )
                .await?;
            }

            Ok(CallToolResult::structured(json!({
                "output_path": output_path.to_string_lossy(),
                "frame_count": frame_png_paths.len(),
                "fps": options.fps,
                "duration": options.total_duration,
                "resolution": format!("{}x{}", options.resolution.width, options.resolution.height),
            })))
        }
        Err(error) => Err(error),
    }
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

    if !status.success() {
        return Err(McpError::internal_error(
            "html_video_render requires ffmpeg in PATH",
            Some(json!({ "code": "ffmpeg_unavailable" })),
        ));
    }
    Ok(())
}

async fn ensure_node_available() -> Result<(), McpError> {
    let status = Command::new("node")
        .arg("-v")
        .output()
        .await
        .map_err(|error| {
            McpError::internal_error(
                "html_video_render requires node for Playwright capture",
                Some(json!({ "code": "node_unavailable", "error": error.to_string() })),
            )
        })?;
    if !status.success() {
        return Err(McpError::internal_error(
            "html_video_render requires node for Playwright capture",
            Some(json!({ "code": "node_unavailable" })),
        ));
    }
    Ok(())
}

async fn capture_frame_pngs(
    frame_html_paths: &[String],
    scratch_dir: &Path,
    resolution: RenderDimensions,
) -> Result<Vec<PathBuf>, McpError> {
    ensure_node_available().await?;

    let output_json = scratch_dir.join("captures.json");
    let script_path = scratch_dir.join("capture_frames.js");
    let config_path = scratch_dir.join("capture_config.json");

    let config = PlaywrightCaptureConfig {
        frame_html_paths: frame_html_paths
            .iter()
            .map(PathBuf::from)
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        output_dir: scratch_dir.to_string_lossy().to_string(),
        output_json: output_json.to_string_lossy().to_string(),
        width: resolution.width,
        height: resolution.height,
    };
    let config_json = serde_json::to_string_pretty(&config).map_err(|error| {
        McpError::internal_error(
            format!("{METHOD} failed to serialize capture configuration"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let script = capture_script();

    fs::write(&config_path, config_json).map_err(|error| {
        McpError::internal_error(
            format!("{METHOD} failed to write capture configuration"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    fs::write(&script_path, script).map_err(|error| {
        McpError::internal_error(
            format!("{METHOD} failed to write capture script"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let status = Command::new("node")
        .arg(&script_path)
        .arg(&config_path)
        .output()
        .await
        .map_err(|error| {
            McpError::internal_error(
                "html_video_render failed to launch node capture process",
                Some(json!({ "code": "playwright_capture_launch_failed", "error": error.to_string() })),
            )
        })?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(McpError::internal_error(
            "html_video_render could not capture html frames (Playwright/Node)",
            Some(json!({
                "code": "playwright_capture_failed",
                "stderr": stderr.trim(),
            })),
        ));
    }

    let raw = fs::read_to_string(&output_json).map_err(|error| {
        McpError::internal_error(
            "html_video_render could not read capture output",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let result: PlaywrightCaptureOutput = serde_json::from_str(&raw).map_err(|error| {
        McpError::internal_error(
            "html_video_render produced invalid capture output",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let paths = result
        .frame_png_paths
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let mut validated = Vec::with_capacity(paths.len());
    for path in paths {
        if !path.is_file() {
            return Err(McpError::invalid_params(
                "html_video_render received a missing frame screenshot",
                Some(json!({ "path": path })),
            ));
        }
        validated.push(path);
    }
    Ok(validated)
}

async fn encode_single_frame(
    frame_png_path: &Path,
    output_path: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    run_ffmpeg_frame_encode(frame_png_path, output_path, options, duration_seconds).await
}

async fn encode_frame_sequence(
    frame_png_paths: &[PathBuf],
    output_path: &Path,
    scratch_dir: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    let segments_dir = scratch_dir;
    let mut segment_paths = Vec::with_capacity(frame_png_paths.len());

    for (index, frame_png_path) in frame_png_paths.iter().enumerate() {
        let segment_path = segments_dir.join(format!("spur-html-video-segment-{}.mp4", index));
        run_ffmpeg_frame_encode(frame_png_path, &segment_path, options, duration_seconds).await?;
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
    if !status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(McpError::internal_error(
            "html_video_render failed to concat rendered frame segments",
            Some(json!({ "code": "ffmpeg_concat_failed", "stderr": stderr.trim() })),
        ));
    }
    Ok(())
}

async fn run_ffmpeg_frame_encode(
    frame_png_path: &Path,
    output_path: &Path,
    options: RenderOptions,
    duration_seconds: f64,
) -> Result<(), McpError> {
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-loop")
        .arg("1")
        .arg("-i")
        .arg(frame_png_path)
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
    if !status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(McpError::internal_error(
            format!("{METHOD} failed to encode frame"),
            Some(json!({ "code": "ffmpeg_encode_failed", "stderr": stderr.trim() })),
        ));
    }
    Ok(())
}

fn capture_script() -> &'static str {
    r#"const fs = require('node:fs/promises');
const path = require('node:path');
const { chromium } = require('playwright');
const { pathToFileURL } = require('node:url');

async function main() {
  const configPath = process.argv[2];
  if (!configPath) {
    console.error('missing capture config path');
    process.exit(1);
  }

  const config = JSON.parse(await fs.readFile(configPath, 'utf8'));
  const browser = await chromium.launch({ headless: true });
  const framePngPaths = [];

  try {
    for (let index = 0; index < config.frame_html_paths.length; index += 1) {
      const htmlPath = path.resolve(config.frame_html_paths[index]);
      const outputPath = path.join(
        config.output_dir,
        `frame_${String(index).padStart(4, '0')}.png`,
      );

      const page = await browser.newPage({
        viewport: { width: config.width, height: config.height },
      });
      const fileUrl = pathToFileURL(htmlPath).toString();
      await page.goto(fileUrl, { waitUntil: 'networkidle' });
      await page.screenshot({ path: outputPath, fullPage: true });
      await page.close();
      framePngPaths.push(outputPath);
    }
  } finally {
    await browser.close();
  }

  await fs.writeFile(config.output_json, JSON.stringify({ frame_png_paths: framePngPaths }));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
"#
}
