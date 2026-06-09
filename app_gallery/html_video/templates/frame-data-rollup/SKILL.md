---
name: frame-data-rollup
description: Native DOM/GSAP horizontal bar rollup visual for labels and KPI values.
---
# Frame: Data Rollup

Use this template when a video needs a concise data visualization: rankings,
metrics comparisons, progress summaries, or KPI rollups.

## Capture Contract

The template is a full-viewport native HyperFrames composition:
- Root element uses `data-composition-id="main"` with `data-start`, `data-duration`,
  `data-width`, and `data-height`.
- `./gsap.min.js` is vendored and loaded directly.
- A paused GSAP timeline is registered on `window.__timelines["main"]`.
- No `<canvas>`, no `window.__hf`, and no `requestAnimationFrame` render loops.

## Inputs

- `title.eyebrow`: the small uppercase label above the chart.
- `title.headline`: the main chart title.
- `title.note`: the footer note.
- `data`: edit each object `label`, numeric `value`, and optional `color`.

Use DOM rows with values in `data-value` attributes and animate widths on the `.bar-fill`
elements with the timeline for per-channel rollup pacing.

