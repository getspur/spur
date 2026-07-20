/**
 * Exact-duration HTML canvas plate renderer.
 * Streams JPEG frames into ffmpeg (no intermediate frame dump) to avoid ENOSPC.
 */
import fs from 'fs';
import path from 'path';
import { createRequire } from 'module';
import { fileURLToPath, pathToFileURL } from 'url';
import { spawn } from 'child_process';

const require = createRequire(import.meta.url);
const puppeteer = require('../../../demo_render/scripts/node_modules/puppeteer-core');

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const chromePath = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const FPS = 24;

const PLATES = [
  { id: 'plate-edges-dark', duration: 11 },
  { id: 'plate-two-planes', duration: 13 },
  { id: 'plate-selector', duration: 8 },
  { id: 'plate-tool-loop', duration: 14 },
];

function runFfmpeg(outMp4) {
  return spawn(
    'ffmpeg',
    [
      '-nostdin', '-y', '-v', 'error',
      '-f', 'image2pipe',
      '-framerate', String(FPS),
      '-i', 'pipe:0',
      '-c:v', 'libx264',
      '-pix_fmt', 'yuv420p',
      '-movflags', '+faststart',
      outMp4,
    ],
    { stdio: ['pipe', 'inherit', 'inherit'] },
  );
}

async function renderPlate(browser, plate) {
  const htmlPath = path.join(__dirname, 'html', `${plate.id}.html`);
  const outMp4 = path.join(__dirname, 'out', `${plate.id}.mp4`);
  fs.mkdirSync(path.dirname(outMp4), { recursive: true });

  const page = await browser.newPage();
  await page.setViewport({ width: 1920, height: 1080, deviceScaleFactor: 1 });
  await page.goto(pathToFileURL(htmlPath).href, { waitUntil: 'networkidle0' });
  await page.waitForFunction(() => typeof window.__draw === 'function');

  const totalFrames = Math.round(plate.duration * FPS);
  const ff = runFfmpeg(outMp4);
  const writeError = new Promise((_, reject) => {
    ff.on('error', reject);
    ff.stdin.on('error', reject);
  });

  try {
    for (let i = 0; i < totalFrames; i++) {
      const t = i / FPS;
      await page.evaluate((sec) => window.__draw(sec), t);
      const buf = await page.screenshot({ type: 'jpeg', quality: 88 });
      await Promise.race([
        new Promise((resolve, reject) => {
          ff.stdin.write(buf, (err) => (err ? reject(err) : resolve()));
        }),
        writeError,
      ]);
      if (i % 48 === 0) console.log(`  ${plate.id} frame ${i}/${totalFrames}`);
    }
    await new Promise((resolve) => ff.stdin.end(resolve));
    const code = await new Promise((resolve) => ff.on('close', resolve));
    if (code !== 0) throw new Error(`ffmpeg exit ${code} for ${plate.id}`);
  } finally {
    await page.close();
  }
  console.log('wrote', outMp4);
}

const only = process.argv[2];
const list = only ? PLATES.filter((p) => p.id === only) : PLATES;
if (!list.length) throw new Error(`unknown plate: ${only}`);

const browser = await puppeteer.launch({
  headless: true,
  executablePath: chromePath,
  args: ['--no-sandbox', '--disable-gpu'],
  defaultViewport: { width: 1920, height: 1080, deviceScaleFactor: 1 },
});

try {
  for (const plate of list) {
    console.log('render', plate.id, plate.duration + 's');
    await renderPlate(browser, plate);
  }
} finally {
  await browser.close();
}
console.log('done');
