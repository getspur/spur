import path from 'path';
import { fileURLToPath } from 'url';
import fs from 'fs';
import { createRequire } from 'module';
const require = createRequire(import.meta.url);
const puppeteer = require('puppeteer-core');
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const chromePath = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

const frames = [
  { html: 'html/01-title.html', out: 'frames/01-title.png' },
  { html: 'html/03-end.html', out: 'frames/03-end.png' },
  { html: 'html/cap-session.html', out: 'frames/cap-session.png' },
  { html: 'html/cap-workers.html', out: 'frames/cap-workers.png' },
  { html: 'html/cap-plans.html', out: 'frames/cap-plans.png' },
  { html: 'html/cap-specialists.html', out: 'frames/cap-specialists.png' },
  { html: 'html/cap-resume.html', out: 'frames/cap-resume.png' },
];

// caption strip template
function capHtml(text) {
  return `<!DOCTYPE html><html><head><meta charset="utf-8"/><style>
html,body{margin:0;width:1920px;height:1080px;background:transparent;overflow:hidden;
font-family:ui-sans-serif,system-ui,-apple-system,Segoe UI,sans-serif;}
.cap{position:absolute;left:48px;bottom:48px;padding:14px 22px;border-radius:10px;
background:rgba(11,14,20,.82);color:#E6E1CF;font-size:34px;border:1px solid #2a2e38;
max-width:1600px;}
.accent{color:#7FB4CA;margin-right:10px;}
</style></head><body><div class="cap"><span class="accent">▸</span>${text}</div></body></html>`;
}
const caps = [
  ['html/cap-session.html', 'Session Detail — operator home'],
  ['html/cap-workers.html', 'Drive brain ↔ worker loops'],
  ['html/cap-plans.html', 'Plans and progress in one surface'],
  ['html/cap-specialists.html', 'Specialists without losing context'],
  ['html/cap-resume.html', 'Resume after you close the laptop'],
];
for (const [file, text] of caps) {
  fs.writeFileSync(path.join(root, file), capHtml(text));
}

const browser = await puppeteer.launch({
  headless: true,
  executablePath: chromePath,
  args: ['--no-sandbox', '--disable-gpu', '--window-size=1920,1080'],
  defaultViewport: { width: 1920, height: 1080, deviceScaleFactor: 1 },
});
const page = await browser.newPage();
for (const f of frames) {
  const file = path.join(root, f.html);
  await page.goto('file://' + file, { waitUntil: 'networkidle0', timeout: 30000 });
  await page.screenshot({ path: path.join(root, f.out), type: 'png', omitBackground: f.out.includes('cap-') });
  console.log('shot', f.out);
}
await browser.close();
