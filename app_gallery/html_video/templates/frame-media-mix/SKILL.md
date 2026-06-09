---
name: frame-media-mix
description: Native DOM/GSAP mix template with local video and local audio tracks.
---
# Frame: Media Mix

Use this template when a scene needs a synchronized visual and audio mix using
DOM media elements inside a native HyperFrames composition.

## Capture Contract

The template is a full-viewport native composition:
- Root element uses `data-composition-id="main"` with `data-start`,
  `data-duration`, `data-width`, and `data-height`.
- `./gsap.min.js` is vendored and loaded directly.
- A paused GSAP timeline is registered on `window.__timelines["main"]`.
- A local `<video>` element is declared with `data-track-index="0"`.
- A local `<audio>` element is declared with `data-track-index="2"` and
  `data-volume`.
- No `<canvas>`, no `window.__hf`, and no `requestAnimationFrame` render loops.

## Inputs

- `media.video`: replace `./media/clip.mp4` as needed.
- `media.audio`: replace `./media/tone.wav` as needed.
- `copy.kicker`: the small uppercase label above the headline.
- `copy.headline`: the main overlay headline.
- `copy.subtext`: the supporting sentence below the headline.

Customize the GSAP timeline and overlay typography inside the inline script
for pacing and visual rhythm.
