# HyperFrames Bun harness for `html_video`

This folder now runs HyperFrames-native compositions through the Bun runtime.

## Runtime stack

- Bun runtime: **>=1.1.0**
- `@hyperframes/engine`: **0.6.84**
- `@hyperframes/producer`: **0.6.84**
- `puppeteer`: **24.0.0**

## Why Bun

`@hyperframes/producer@0.6.84` is Bun-native and requires Bun for reliable execution.
The subprocess contract remains direct Bun execution.

## Install + provisioning

From `app_gallery/html_video/engine`:

```bash
cd app_gallery/html_video/engine
bun install
```

- Install Bun if not already available:

```bash
curl -fsSL https://bun.sh/install | bash
```

- If Chromium is already available locally:

```bash
bun install
```

- If no Chromium is present:

```bash
bun install
bunx puppeteer browsers install chrome
```

## `render-hf.mjs` CLI contract

`render-hf.mjs` is intentionally positional so F1 can shell to it directly.

```bash
bun render-hf.mjs <composition> <duration_seconds> <fps> <resolution> <output_mp4>
```

- `<composition>`: a composition directory containing `index.html` or an html file under that root.
  - If a directory, `index.html` must exist in the directory.
  - If an html file path, its parent directory is staged as the composition root and staged to `index.html` when needed.
- `<duration_seconds>`: render duration in seconds, positive numeric (validated in harness, retained for CLI contract).
- `<fps>`: integer fps (`30`, `60`, …) or rational fps (`30000/1001`).
- `<resolution>`: `<width>x<height>`, e.g. `1280x720` (validated in harness, composition attributes drive actual canvas size).
- `<output_mp4>`: output path (`.mp4` auto-appended if omitted).

Example:

```bash
bun render-hf.mjs ../templates/frame-liquid-hero 2 30 1280x720 /tmp/liquid-hero.mp4
```

## Chromium/Bun process contract

`PUPPETEER_EXECUTABLE_PATH` is optional and passed through as a producer chrome path.
When set, it is sent to `createRenderJob` as `producerConfig.chromePath`.

For environments where Chromium is unavailable, install one explicitly:

```bash
bunx puppeteer browsers install chrome
```

or set `PUPPETEER_EXECUTABLE_PATH` to an existing browser binary.

## Determinism and composition metadata

`frame` timing is handled by the producer and the composition’s timeline.

The composition must be HyperFrames-native:

- Root `<div>` carries `data-composition-id`, `data-start`, `data-duration`,
  `data-width`, and `data-height`.
- Animation is driven by a paused GSAP timeline assigned as
  `window.__timelines["main"]`.
- The producer reads composition duration and dimensions from DOM attributes.
