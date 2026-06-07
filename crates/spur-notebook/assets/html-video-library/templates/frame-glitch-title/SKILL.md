---
name: frame-glitch-title
description: CSS-only chromatic glitch title card for short video openers, chapter breaks, and high-energy text reveals.
---
# Frame: Glitch Title

Use this template when a video needs an immediate title-card hit: a large
headline with chromatic aberration, scanlines, and short CSS glitch bursts.

## Inputs

- `title`: the main title text. Replace every `SYSTEM SHOCK` occurrence in
  `template.html`, including the `data-text` attribute and pseudo-layer text.
- Optional kicker and footer copy can be edited in the `.kicker` and `.footer`
  elements.

## Usage

Keep the template self-contained. Do not add external fonts, scripts, images,
or CDN links. The animation starts on page load through CSS keyframes, so the
frame can be captured directly by a browser-based renderer.
