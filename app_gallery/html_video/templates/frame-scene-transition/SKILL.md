---
name: frame-scene-transition
description: Native HyperFrames multi-scene composition with a domain-warp shader transition between two DOM scenes.
---
# Frame: Scene Transition

Use this template when a short title scene needs a direct, stylized transition into a second
closing scene with no canvas fallback.

## Capture Contract

- Composition root is native HyperFrames: `<div data-composition-id="main"...>`.
- Two nested scene layers (`scene-1` and `scene-2`) are both nested under the root and use
  `data-composition-id`, `data-start`, and `data-duration`.
- Vendor scripts are local: `./gsap.min.js` and `./shader-transitions.global.js`.
- A `window.HyperShader.init(...)` instance is created with `scenes: ["scene-1","scene-2"]` and a
  single `domain-warp` transition at the boundary.
- The HyperShader return value is assigned to `window.__timelines["main"]` for deterministic engine seeking.

## Inputs

- Scene visuals are authored directly with DOM/CSS in each `.scene`.
- Edit headline/metrics text and scene duration timings directly in the scene markup.
