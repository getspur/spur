#!/usr/bin/env bun
import {
  cpSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
} from "node:fs";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, basename, resolve, join } from "node:path";
import { tmpdir } from "node:os";
import {
  createCaptureSession,
  captureFrame,
  closeCaptureSession,
  createFileServer,
  encodeFramesFromDir,
  getCompositionDuration,
  initializeSession,
} from "@hyperframes/engine";

const USAGE = `Usage:
  bun render-hf.mjs <composition_html> <duration_seconds> <fps> <resolution> <output_mp4>

Args:
  composition_html   Path to an HTML composition exposing window.__hf.
  duration_seconds   Render duration in seconds (positive number).
  fps               Integer fps ("30", "60") or NTSC rational ("30000/1001").
  resolution        "<width>x<height>" such as "1280x720".
  output_mp4        Output MP4 path (will append .mp4 if missing).
`;

/**
 * Deterministically parse frame-count parameters.
 */
function parseFps(raw) {
  const input = String(raw).trim();
  if (input.length === 0) {
    throw new Error("fps is required");
  }

  const rationalMatch = input.match(/^([0-9]+)\s*\/\s*([0-9]+)$/);
  if (rationalMatch) {
    const num = Number(rationalMatch[1]);
    const den = Number(rationalMatch[2]);
    const fps = num / den;
    if (
      !Number.isFinite(num) ||
      !Number.isFinite(den) ||
      den <= 0 ||
      num <= 0 ||
      fps < 1 ||
      fps > 240
    ) {
      throw new Error(`Invalid fps "${raw}"`);
    }
    return { num, den };
  }

  if (!/^[0-9]+$/.test(input)) {
    throw new Error(`Invalid fps "${raw}"`);
  }
  const num = Number(input);
  if (!Number.isFinite(num) || !Number.isInteger(num) || num <= 0 || num > 240) {
    throw new Error(`Invalid fps "${raw}"`);
  }
  return { num, den: 1 };
}

function parseDuration(raw) {
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`Invalid duration "${raw}"`);
  }
  return value;
}

function parseResolution(raw) {
  const input = String(raw).trim();
  const match = input.match(/^(\d+)\s*x\s*(\d+)$/i);
  if (!match) {
    throw new Error(`Invalid resolution "${raw}", expected <width>x<height>`);
  }
  const width = Number(match[1]);
  const height = Number(match[2]);
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    throw new Error(`Invalid resolution "${raw}"`);
  }
  return { width, height };
}

async function stageComposition(compositionPath) {
  const absoluteCompositionPath = resolve(compositionPath);
  if (!existsSync(absoluteCompositionPath)) {
    throw new Error(`Composition file not found: ${absoluteCompositionPath}`);
  }

  const info = statSync(absoluteCompositionPath);
  if (!info.isFile() && !info.isDirectory()) {
    throw new Error(`Composition must be an html file or directory: ${absoluteCompositionPath}`);
  }

  const workspace = mkdtempSync(join(tmpdir(), "hf-render-workspace-"));

  if (info.isDirectory()) {
    cpSync(absoluteCompositionPath, workspace, { recursive: true, force: true });
    const indexPath = join(workspace, "index.html");
    if (!existsSync(indexPath)) {
      throw new Error(`Directory composition missing index.html: ${absoluteCompositionPath}`);
    }
    return { workspace };
  }

  cpSync(dirname(absoluteCompositionPath), workspace, { recursive: true, force: true });
  const originalName = basename(absoluteCompositionPath);
  if (originalName !== "index.html") {
    copyFileSync(absoluteCompositionPath, join(workspace, "index.html"));
  }

  return { workspace };
}

function normalizeOutputPath(raw) {
  const output = resolve(raw);
  return output.toLowerCase().endsWith(".mp4") ? output : `${output}.mp4`;
}

function parseArgs() {
  const args = process.argv.slice(2);
  if (args.length >= 1 && (args[0] === "-h" || args[0] === "--help")) {
    console.log(USAGE);
    process.exit(0);
  }
  if (args.length < 5) {
    console.error("Missing required positional args.\n");
    console.log(USAGE);
    process.exit(1);
  }
  const [compositionPath, durationRaw, fpsRaw, resolutionRaw, outputRaw] = args;
  return {
    compositionPath,
    duration: parseDuration(durationRaw),
    fps: parseFps(fpsRaw),
    resolution: parseResolution(resolutionRaw),
    outputPath: normalizeOutputPath(outputRaw),
  };
}

async function main() {
  const bunVersion = process?.versions?.bun;
  if (!bunVersion || typeof bunVersion !== "string") {
    throw new Error(
      "Bun runtime is required for @hyperframes/engine 0.6.84 (Node execution path is incompatible). Run with: bun render-hf.mjs ...",
    );
  }

  const { compositionPath, duration, fps, resolution, outputPath } = parseArgs();
  const outputDir = dirname(outputPath);
  if (outputDir) {
    mkdirSync(outputDir, { recursive: true });
  }

  const preferredExecutable = (process.env.PUPPETEER_EXECUTABLE_PATH || "").trim();
  const engineConfig = {};
  if (preferredExecutable) {
    if (existsSync(preferredExecutable)) {
      engineConfig.chromePath = preferredExecutable;
      console.log(`[render-hf] Using PUPPETEER_EXECUTABLE_PATH=${preferredExecutable}`);
    } else {
      console.warn(
        `[render-hf] PUPPETEER_EXECUTABLE_PATH is set but does not exist: ${preferredExecutable}`,
      );
    }
  } else {
    console.log("[render-hf] PUPPETEER_EXECUTABLE_PATH is not set; using Puppeteer default.");
  }

  const tmpFramesDir = mkdtempSync(join(tmpdir(), "hf-render-frames-"));
  let server;
  let session;
  let workspace;
  const framePattern = "frame_%06d.jpg";

  try {
    const staged = await stageComposition(compositionPath);
    workspace = staged.workspace;
    const framesDir = tmpFramesDir;

    server = await createFileServer({ projectDir: workspace });
    console.log(`[render-hf] Serving composition from ${server.url} (workspace ${workspace})`);

    session = await createCaptureSession(
      server.url,
      framesDir,
      {
        width: resolution.width,
        height: resolution.height,
        fps,
        format: "jpeg",
        quality: 90,
        lockWarmupTicks: true,
      },
      null,
      engineConfig,
    );

    await initializeSession(session);
    const pageDuration = await getCompositionDuration(session);
    if (!Number.isFinite(pageDuration) || pageDuration <= 0) {
      throw new Error("Composition __hf.duration is missing or non-positive.");
    }
    if (Math.abs(pageDuration - duration) > 0.001) {
      console.warn(
        `[render-hf] Requested duration (${duration}) differs from composition duration (${pageDuration}). Using requested duration.`,
      );
    }

    const secondsPerFrame = fps.den / fps.num;
    const targetFrameCount = Math.floor(duration * fps.num / fps.den);
    const captureFrames = Math.max(1, targetFrameCount);

    for (let frameIndex = 0; frameIndex < captureFrames; frameIndex += 1) {
      const seekTime = frameIndex * secondsPerFrame;
      await captureFrame(session, frameIndex, seekTime);
    }

    const encodeResult = await encodeFramesFromDir(
      framesDir,
      framePattern,
      outputPath,
      {
        fps,
        width: resolution.width,
        height: resolution.height,
        quality: 18,
      },
    );
    if (!encodeResult.success) {
      throw new Error(`HyperFrames encode failed: ${encodeResult.error || "unknown"}`);
    }

    if (!existsSync(outputPath) || statSync(outputPath).size <= 0) {
      throw new Error(`Output file was not created or is empty: ${outputPath}`);
    }

    const stats = statSync(outputPath);
    const frameHash = createHash("sha1").update(readFileSync(outputPath)).digest("hex");
    console.log(
      `[render-hf] Wrote ${outputPath} (${stats.size} bytes, ${encodeResult.framesEncoded} frames, sha1=${frameHash})`,
    );
    console.log(`[render-hf] Duration arg=${duration}, fps=${fps.num}/${fps.den}`);
  } finally {
    if (session) {
      try {
        await closeCaptureSession(session);
      } catch (error) {
        console.warn(`[render-hf] closeCaptureSession warning: ${error instanceof Error ? error.message : error}`);
      }
    }
    if (server) {
      try {
        server.close();
      } catch (error) {
        console.warn(`[render-hf] file server close warning: ${error instanceof Error ? error.message : error}`);
      }
    }
    if (tmpFramesDir) {
      try {
        rmSync(tmpFramesDir, { recursive: true, force: true });
      } catch (error) {
        console.warn(
          `[render-hf] frames cleanup warning: ${error instanceof Error ? error.message : error}`,
        );
      }
    }
    if (workspace) {
      try {
        rmSync(workspace, { recursive: true, force: true });
      } catch (error) {
        console.warn(
          `[render-hf] workspace cleanup warning: ${error instanceof Error ? error.message : error}`,
        );
      }
    }
  }
}

main().catch((error) => {
  console.error(`[render-hf] ${error instanceof Error ? error.message : String(error)}`);
  process.exit(1);
});
