---
name: frame-glitch-title
description: Native DOM/GSAP glitch title card with chromatic layering and burst jitter.
---
# Frame: Glitch Title

Use this template when a video needs an immediate title-card hit: a large
headline with chromatic aberration, scanlines, and short glitch bursts.

## Capture Contract

The template is a full-viewport native HyperFrames composition:
- Root element uses `data-composition-id="main"` with `data-start`, `data-duration`,
  `data-width`, and `data-height`.
- `./gsap.min.js` is vendored and loaded directly.
- A paused GSAP timeline is registered on `window.__timelines["main"]`.
- No `<canvas>`, no `window.__hf`, and no `requestAnimationFrame` render loops.

## Inputs

- `copy.title`: the main title text.
- `copy.kicker`: the small uppercase label above the title.
- `copy.footer`: the supporting line below the title.

Customize colors, slice offsets, glow positions, and scanline density inside the
inline `frame()` function.
