# HyperFrames Bun harness for `html_video`

This folder is the F0 harness used by the engine-backed render path.

## Runtime stack

- Bun runtime: **>=1.1.0**
- `@hyperframes/engine`: **0.6.84**
- `puppeteer`: **24.0.0**

## Why Bun (not Node)

`@hyperframes/engine@0.6.84` is currently Bun-native and not reliably consumable via stock Node ESM
in this repository context (broken exports/type path behavior), so the subprocess contract is Bun.

## Install + provisioning

From `app_gallery/html_video/engine`:

```bash
cd app_gallery/html_video/engine
npm install
```

- Install Bun if not already available (single binary):

```bash
curl -fsSL https://bun.sh/install | bash
```

- If Chromium is already available locally:

```bash
PUPPETEER_SKIP_DOWNLOAD=true npm install
```

- If no Chromium is present:

```bash
npm install
npx puppeteer browsers install chrome
```

## `render-hf.mjs` CLI contract

`render-hf.mjs` is intentionally positional so F1 can shell to it directly.

```bash
bun render-hf.mjs <composition_html> <duration_seconds> <fps> <resolution> <output_mp4>
```

- `<composition_html>`: HTML file path exposing `window.__hf = { duration, seek }`.
  - If `<composition_html>` is a file, relative assets are served from that file’s parent directory.
  - If `<composition_html>` is a directory, `index.html` must exist.
- `<duration_seconds>`: render duration in seconds, positive numeric.
- `<fps>`: integer fps (`30`, `60`, …) or rational fps (`30000/1001`).
- `<resolution>`: `<width>x<height>`, e.g. `1280x720`.
- `<output_mp4>`: output path (`.mp4` auto-appended if omitted).

Example:

```bash
bun render-hf.mjs ../templates/frame-liquid-hero/template.html 2.0 30 1280x720 /tmp/liquid-hero.mp4
```

## Chromium/Bun process contract

The harness prefers system Chromium via:

1. `PUPPETEER_EXECUTABLE_PATH` (exported env var), passed as `chromePath`.
2. Puppeteer-managed browser fallback when no env path is provided.

For environments where Chromium is unavailable, install one explicitly:

```bash
npx puppeteer browsers install chrome
```

or set `PUPPETEER_EXECUTABLE_PATH` to an existing browser binary.

## Deterministic output behavior

Frames are rendered with fixed frame times at exactly:

`frame_index / fps`.

The script captures `floor(duration * fps)` frames from `t=0` in order and encodes one MP4 via
`encodeFramesFromDir`. For fixed inputs this produces deterministic frame-accurate output.
