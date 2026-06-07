const fs = require('node:fs/promises');
const path = require('node:path');
const { chromium } = require('playwright');
const { pathToFileURL } = require('node:url');

async function findNewestWebm(dir) {
  const entries = await fs.readdir(dir, { withFileTypes: true });
  const webms = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.toLowerCase().endsWith('.webm')) {
      continue;
    }
    const filePath = path.join(dir, entry.name);
    const stat = await fs.stat(filePath);
    webms.push({ filePath, mtimeMs: stat.mtimeMs });
  }
  webms.sort((a, b) => b.mtimeMs - a.mtimeMs);
  return webms.length > 0 ? webms[0].filePath : null;
}

async function captureScreenshot(browser, htmlPath, outputPath, width, height) {
  const page = await browser.newPage({
    viewport: { width, height },
  });
  try {
    const fileUrl = pathToFileURL(htmlPath).toString();
    await page.goto(fileUrl, { waitUntil: 'networkidle' });
    await page.screenshot({ path: outputPath, fullPage: true });
    return outputPath;
  } finally {
    await page.close();
  }
}

async function captureFrame(browser, config, htmlPath, index) {
  const frameDir = path.join(config.output_dir, `frame_${String(index).padStart(4, '0')}`);
  await fs.mkdir(frameDir, { recursive: true });

  let context;
  let video;
  try {
    context = await browser.newContext({
      viewport: { width: config.width, height: config.height },
      recordVideo: {
        dir: frameDir,
        size: { width: config.width, height: config.height },
      },
    });
    const page = await context.newPage();
    video = page.video();
    const fileUrl = pathToFileURL(htmlPath).toString();
    await page.goto(fileUrl, { waitUntil: 'networkidle' });
    await page.waitForTimeout(Math.max(1, config.duration_sec * 1000));
    await context.close();
    context = null;

    if (video) {
      try {
        const videoPath = await video.path();
        if (videoPath) {
          return videoPath;
        }
      } catch (_) {
        // Fall through to directory lookup.
      }
    }

    const discoveredPath = await findNewestWebm(frameDir);
    if (discoveredPath) {
      return discoveredPath;
    }

    throw new Error(`recordVideo produced no webm for frame ${index}`);
  } catch (error) {
    if (context) {
      await context.close().catch(() => {});
    }
    const screenshotPath = path.join(
      config.output_dir,
      `frame_${String(index).padStart(4, '0')}.png`,
    );
    return captureScreenshot(browser, htmlPath, screenshotPath, config.width, config.height);
  }
}

async function main() {
  const configPath = process.argv[2];
  if (!configPath) {
    console.error('missing capture config path');
    process.exit(1);
  }

  const config = JSON.parse(await fs.readFile(configPath, 'utf8'));
  const browser = await chromium.launch({ headless: true });
  const frameWebmPaths = [];

  try {
    for (let index = 0; index < config.frame_html_paths.length; index += 1) {
      const htmlPath = path.resolve(config.frame_html_paths[index]);
      const outputPath = await captureFrame(browser, config, htmlPath, index);
      frameWebmPaths.push(outputPath);
    }
  } finally {
    await browser.close();
  }

  await fs.writeFile(config.output_json, JSON.stringify({ frame_webm_paths: frameWebmPaths }));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
