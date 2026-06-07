---
name: frame-glitch-title
description: Canvas chromatic glitch title card for short video openers, chapter breaks, and high-energy text reveals.
---
# Frame: Glitch Title

Use this template when a video needs an immediate title-card hit: a large
headline with chromatic aberration, scanlines, and short glitch bursts.

## Capture Contract

The template renders all visible content to one `<canvas data-capture="true">`.
Do not add DOM-rendered visual layers, CSS animations, external fonts, scripts,
images, or CDN links. The script sizes the canvas with
`canvas.width = window.innerWidth` and `canvas.height = window.innerHeight`,
then starts a `requestAnimationFrame` loop on load so `canvas.captureStream()`
records the same pixels shown in the iframe preview.

## Inputs

- `copy.title`: the main title text.
- `copy.kicker`: the small uppercase label above the title.
- `copy.footer`: the supporting line below the title.

Customize colors, slice offsets, glow positions, and scanline density inside the
inline `frame()` function.
