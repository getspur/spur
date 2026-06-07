---
name: frame-liquid-hero
description: Canvas animated liquid-gradient hero frame for polished openers, transitions, and calm headline moments.
---
# Frame: Liquid Hero

Use this template when a video needs a refined hero or opener with a centered
headline over a moving liquid-gradient field.

## Capture Contract

The template renders all visible content to one `<canvas data-capture="true">`.
Do not add DOM-rendered visual layers, CSS animations, external fonts, scripts,
images, or CDN links. The script sizes the canvas with
`canvas.width = window.innerWidth` and `canvas.height = window.innerHeight`,
then starts a `requestAnimationFrame` loop on load so `canvas.captureStream()`
records the same pixels shown in the iframe preview.

## Inputs

- `copy.headline`: the centered main headline.
- `copy.subtext`: the supporting sentence below the headline.
- `copy.eyebrow`: the small uppercase label above the headline.

Customize the gradient palette and liquid motion by editing the background
stops and `blob(...)` calls in the inline `frame()` function.
