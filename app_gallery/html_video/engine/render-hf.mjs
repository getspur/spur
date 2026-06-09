#!/usr/bin/env bun
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { basename, dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { createRenderJob, executeRenderJob } from "@hyperframes/producer";

const USAGE = `Usage:
  bun render-hf.mjs <composition> <duration_seconds> <fps> <resolution> <output_mp4>

Args:
  composition        Path to a HyperFrames composition directory (with index.html) or an
                     html file in that directory.
  duration_seconds   Render duration in seconds (positive number).
  fps               Integer fps ("30", "60") or NTSC rational ("30000/1001").
  resolution        "<width>x<height>" such as "1280x720".
  output_mp4         Output MP4 path (will append .mp4 if missing).
`;

function parseFps(raw) {
  const input = String(raw).trim();
  if (input.length === 0) {
    throw new Error("fps is required");
  }

  const rationalMatch = input.match(/^(\d+)\s*\/\s*(\d+)$/);
  if (rationalMatch) {
    const num = Number(rationalMatch[1]);
    const den = Number(rationalMatch[2]);
    const fps = num / den;
    if (!Number.isFinite(num) || !Number.isFinite(den) || den <= 0 || num <= 0 || fps < 1 || fps > 240) {
      throw new Error(`Invalid fps "${raw}"`);
    }
    return fps;
  }

  if (!/^[0-9]+$/.test(input)) {
    throw new Error(`Invalid fps "${raw}"`);
  }
  const fps = Number(input);
  if (!Number.isFinite(fps) || !Number.isInteger(fps) || fps <= 0 || fps > 240) {
    throw new Error(`Invalid fps "${raw}"`);
  }
  return fps;
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
    throw new Error(`Composition path not found: ${absoluteCompositionPath}`);
  }

  const info = statSync(absoluteCompositionPath);
  if (!info.isFile() && !info.isDirectory()) {
    throw new Error(`Composition must be an html file or directory: ${absoluteCompositionPath}`);
  }

  const workspace = mkdtempSync(join(tmpdir(), "hf-render-composition-"));

  if (info.isDirectory()) {
    cpSync(absoluteCompositionPath, workspace, { recursive: true, force: true });
    const indexPath = join(workspace, "index.html");
    if (!existsSync(indexPath)) {
      throw new Error(`Directory composition missing index.html: ${absoluteCompositionPath}`);
    }
    return { workspace, compositionDir: workspace };
  }

  cpSync(dirname(absoluteCompositionPath), workspace, { recursive: true, force: true });
  const originalName = basename(absoluteCompositionPath);
  if (originalName !== "index.html") {
    cpSync(absoluteCompositionPath, join(workspace, "index.html"));
  }

  const stagedIndexPath = join(workspace, "index.html");
  if (!existsSync(stagedIndexPath)) {
    throw new Error(`Failed to stage composition html: ${absoluteCompositionPath}`);
  }

  return { workspace, compositionDir: workspace };
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
  const fps = parseFps(fpsRaw);
  const duration = parseDuration(durationRaw);
  const resolution = parseResolution(resolutionRaw);
  return {
    compositionPath,
    fps,
    duration,
    resolution,
    outputPath: normalizeOutputPath(outputRaw),
  };
}

async function main() {
  const bunVersion = process?.versions?.bun;
  if (!bunVersion || typeof bunVersion !== "string") {
    throw new Error(
      "Bun runtime is required for @hyperframes/producer 0.6.84. Run with: bun render-hf.mjs ...",
    );
  }

  const { compositionPath, duration, fps, resolution, outputPath } = parseArgs();
  const outputDir = dirname(outputPath);
  if (outputDir) {
    mkdirSync(outputDir, { recursive: true });
  }

  const { compositionDir } = await stageComposition(compositionPath);
  const chromePath = (process.env.PUPPETEER_EXECUTABLE_PATH || "").trim();
  let workspace = compositionDir;

  try {
    const producerConfig = chromePath ? { chromePath } : {};
    const job = createRenderJob({
      fps,
      quality: "standard",
      format: "mp4",
      workers: 1,
      producerConfig,
    });

    console.log(`Rendering native composition ${compositionDir}`);
    console.log(
      `[render-hf] duration=${duration}s fps=${fps} resolution=${resolution.width}x${resolution.height}`,
    );
    if (chromePath) {
      console.log(`[render-hf] Using PUPPETEER_EXECUTABLE_PATH=${chromePath}`);
    }

    await executeRenderJob(job, compositionDir, outputPath);

    if (!existsSync(outputPath) || statSync(outputPath).size <= 0) {
      throw new Error(`Output file was not created or is empty: ${outputPath}`);
    }

    const stats = statSync(outputPath);
    const frameHash = createHash("sha1").update(readFileSync(outputPath)).digest("hex");
    console.log(`[render-hf] Wrote ${outputPath} (${stats.size} bytes, sha1=${frameHash})`);
  } finally {
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
