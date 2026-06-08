---
name: frame-data-rollup
description: Canvas animated horizontal bar chart frame for showing a small set of labels and values in video.
---
# Frame: Data Rollup

Use this template when a video needs a concise data visualization: rankings,
metric comparisons, progress summaries, or KPI rollups.

## Capture Contract

The template renders all visible content to one `<canvas data-capture="true">`.
Do not add DOM-rendered chart rows, CSS animations, external fonts, scripts,
images, or CDN links. The script sizes the canvas with
`canvas.width = window.innerWidth` and `canvas.height = window.innerHeight`,
then starts a `requestAnimationFrame` loop on load so `canvas.captureStream()`
records the same pixels shown in the iframe preview.

## Inputs

- `title.eyebrow`: the small uppercase label above the chart.
- `title.headline`: the main chart title.
- `title.note`: the footer note.
- `data`: edit each object `label`, numeric `value`, and optional `color`.

The script scales bar widths against the largest `value`. Bar state lives in the
`bars` array and uses spring easing each frame; adjust the force and damping
constants in `frame()` to change how quickly bars settle.
