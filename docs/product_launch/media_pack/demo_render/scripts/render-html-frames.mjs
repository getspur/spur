import fs from 'fs';
import path from 'path';
import { createRequire } from 'module';
import { fileURLToPath, pathToFileURL } from 'url';

const require = createRequire(import.meta.url);
const puppeteer = require('puppeteer-core');
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const renderRoot = path.resolve(__dirname, '..');
const packRoot = path.resolve(renderRoot, '..');
const chromePath = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;');
}

async function launch(viewport) {
  return puppeteer.launch({
    headless: true,
    executablePath: chromePath,
    args: ['--no-sandbox', '--disable-gpu'],
    defaultViewport: { ...viewport, deviceScaleFactor: 1 },
  });
}

async function renderGallery(stage) {
  const manifest = JSON.parse(fs.readFileSync(path.join(packRoot, 'proof-manifest.json'), 'utf8'));
  const browser = await launch({ width: 1270, height: 760 });
  const page = await browser.newPage();

  for (const asset of manifest.assets.filter((entry) => entry.kind === 'still' && entry.status === 'approved')) {
    const captureBytes = fs.readFileSync(path.join(stage, 'gallery_stills', `${asset.id}.png`));
    const capture = `data:image/png;base64,${captureBytes.toString('base64')}`;
    const html = `<!doctype html><html><head><meta charset="utf-8"><style>
      :root{--bg:#0B0E14;--surface:#11141C;--text:#E6E1CF;--muted:#8B8680;--accent:#7FB4CA;--violet:#957FB8;--border:#2A2E38}
      *{box-sizing:border-box}html,body{margin:0;width:1270px;height:760px;overflow:hidden;background:var(--bg);color:var(--text)}
      body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif}
      .rail{height:120px;padding:24px 30px 22px;display:grid;grid-template-columns:minmax(0,1fr) auto;gap:32px;align-items:center;border-bottom:1px solid var(--border);background:var(--surface)}
      .label{margin:0 0 9px;color:var(--accent);font:700 13px/1.1 "SFMono-Regular",Consolas,monospace;letter-spacing:.12em}
      .caption{margin:0;font-size:26px;line-height:1.15;letter-spacing:-.025em;font-weight:650}
      .meta{text-align:right;color:var(--violet);font:12px/1.55 "SFMono-Regular",Consolas,monospace;white-space:nowrap}
      img{display:block;width:1270px;height:640px;object-fit:fill;background:#000}
    </style></head><body><header class="rail"><div><p class="label">${escapeHtml(asset.label)}</p><p class="caption">${escapeHtml(asset.caption)}</p></div><div class="meta">REAL TUI CAPTURE<br>${escapeHtml(asset.journey)} @ ${asset.timestamp_sec}s</div></header><img src="${capture}" alt=""></body></html>`;
    await page.setContent(html, { waitUntil: 'load', timeout: 10000 });
    await page.waitForFunction(() => Array.from(document.images).every((image) => image.complete), { timeout: 10000 });
    await page.screenshot({ path: path.join(stage, 'ph_ready', asset.output), type: 'png' });
    console.log('shot', asset.output);
  }

  await page.setViewport({ width: 512, height: 512, deviceScaleFactor: 1 });
  await page.goto(pathToFileURL(path.join(renderRoot, 'html', 'thumbnail.html')).href, { waitUntil: 'networkidle0' });
  await page.screenshot({ path: path.join(stage, 'ph_ready', 'thumbnail-512.png'), type: 'png' });
  await browser.close();
}

async function renderHeroFrames() {
  const frames = [
    { html: 'html/01-title.html', out: 'frames/01-title.png' },
    { html: 'html/03-end.html', out: 'frames/03-end.png' },
    { html: 'html/cap-session.html', out: 'frames/cap-session.png' },
    { html: 'html/cap-workers.html', out: 'frames/cap-workers.png' },
    { html: 'html/cap-plans.html', out: 'frames/cap-plans.png' },
  ];
  const browser = await launch({ width: 1920, height: 1080 });
  const page = await browser.newPage();
  for (const frame of frames) {
    await page.goto(pathToFileURL(path.join(renderRoot, frame.html)).href, { waitUntil: 'networkidle0' });
    await page.screenshot({
      path: path.join(renderRoot, frame.out),
      type: 'png',
      omitBackground: frame.out.includes('cap-'),
    });
    console.log('shot', frame.out);
  }
  await browser.close();
}

const stageIndex = process.argv.indexOf('--gallery-stage');
if (stageIndex !== -1) {
  const stage = process.argv[stageIndex + 1];
  if (!stage) throw new Error('--gallery-stage requires a directory');
  await renderGallery(path.resolve(stage));
} else {
  await renderHeroFrames();
}
