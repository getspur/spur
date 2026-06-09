# html_video → HyperFrames-native compositions (GSAP/DOM)

- **Date:** 2026-06-09
- **Status:** Approved design; P0 spike PROVEN. Spec under user review.
- **Design epic:** `bd-2pwb`
- **Owner:** brain · **Worker:** codex
- **Supersedes (for media/transitions):** the renderOrchestrator drop-in rework
  (`2026-06-09-html-video-hyperframes-media-rework-design.md`), which was rejected — the
  producer cannot render raw-canvas `__hf` templates (clones the root; canvas → blank).

## Context & proven foundation

F0+F1 (on `main`) give a Bun engine render path for **canvas + `window.__hf.seek`**
templates, video-only. HyperFrames' `producer.renderOrchestrator` (audio/video/transitions)
renders **HyperFrames-native compositions** — a `<div data-composition-id>` root of DOM/CSS
clips driven by a **GSAP timeline** (`window.__timelines`) — NOT raw canvas.

**P0 spike (brain-verified on real machine, 2026-06-09):** a native GSAP/DOM composition
with **vendored local `gsap.min.js`** + a local `<audio>` element rendered through
`createRenderJob`/`executeRenderJob` to a valid MP4 — **video (h264) + audio (aac)**,
frame30 PNG 182 KB (non-blank), `audioCount:1`, `tweenCount:2`, no CDN. The native model
works and audio mixes automatically. This grounds the whole rework.

## Native composition format (authoritative, from the proven PoC)

```html
<head>
  <meta name="viewport" content="width=<W>, height=<H>" />
  <script src="./gsap.min.js"></script>   <!-- VENDORED locally, never CDN -->
  <style>/* root + clips sized to W×H */</style>
</head>
<body>
  <div id="root" data-composition-id="main" data-start="0"
       data-duration="<sec>" data-width="<W>" data-height="<H>">
    <!-- DOM/CSS clips animated by the timeline -->
    <div id="title">…</div>
    <div id="box"></div>
    <!-- media as DOM elements, mixed out-of-band by the engine -->
    <audio src="./audio.wav" data-start="0" data-duration="<sec>" data-track-index="2" data-volume="1"></audio>
    <video src="./clip.mp4" data-start="0" data-duration="<sec>" data-track-index="0" …></video>
  </div>
  <script>
    window.__timelines = window.__timelines || {};
    const tl = gsap.timeline({ paused: true });
    tl.fromTo("#box", {x:0}, {x:900, duration:2}, 0);   // animation as GSAP tweens
    window.__timelines["main"] = tl;
  </script>
</body>
```

- Root carries `data-composition-id` + `data-width`/`data-height`/`data-duration`.
- Animation = a **paused GSAP timeline** registered under the composition id; the producer
  seeks it per frame (deterministic on the BeginFrame path).
- Audio/video are **DOM `<audio>`/`<video>`** with `data-start`/`data-duration`/
  `data-track-index`/`data-volume`; the engine extracts + mixes/composites out-of-band.
- **GSAP is vendored** (`gsap.min.js` copied beside the composition) — satisfies html_video's
  no-external-assets rule.

## Render path

Replace the (rejected) canvas-fed R0 harness with a **native-fed** `render-hf.mjs` that calls
`createRenderJob({fps, quality, format:"mp4", workers:1, producerConfig:{chromePath}})` +
`executeRenderJob(job, compositionDir, outputPath)` under **Bun**. The composition is a
directory (index.html + gsap.min.js + media assets). F1's Python `html_video_render` keeps
shelling `bun render-hf.mjs <compositionDir> <duration> <fps> <WxH> <out>` — the
composition arg becomes a DIRECTORY (it already supports dir compositions). Server contract
stays stable.

## Determinism (revisit)

Earlier finding: headless-shell on macOS forces screenshot mode (non-deterministic). BUT the
P0 run used **`captureMode: "beginframe"` with full Chrome on macOS and succeeded** — so
BeginFrame determinism may be achievable on macOS with full Chrome (the Linux-only caveat
appears tied to chrome-headless-shell). To be re-verified per template; not a blocker.

## Phasing (DAG)

- **P0 — native-format spike — DONE (brain-verified).**
- **P1 — native render harness + first template.** Rewrite `render-hf.mjs` to feed native
  compositions; re-author ONE template (liquid-hero) as GSAP/DOM with vendored gsap.
  *Acceptance:* renders non-blank MP4 (brain-verified).
- **P2 — migrate remaining templates** (glitch-title, data-rollup) to native GSAP/DOM.
- **P3 — media** (audio + `<video>`) via DOM elements; re-add a media-mix template.
  *Acceptance:* MP4 has an audio stream + composited video (brain-verified).
- **P4 — shader transitions** via `__hf.transitions` + `@hyperframes/shader-transitions`,
  multi-scene.
- **P5 — declarative authoring helpers** via `@hyperframes/core` (optional convenience layer).

## Constraints (all phases)

- **Bun only** (Node cannot import `@hyperframes/*` — verified). Pin `@hyperframes/*` 0.6.84.
- **Vendor GSAP locally**; no CDN/external assets in any composition.
- Keep F1's Python CLI contract (`bun render-hf.mjs <dir> <dur> <fps> <WxH> <out>`) stable.
- codex CANNOT launch Chromium in-sandbox → it authors; **brain verifies every render** on
  the real machine at each gate.
- Keep the existing canvas templates working via the F0 low-level path during migration
  (don't break Tier 0) — OR explicitly retire them once the native equivalents land.

## Risks

- Visual parity: native GSAP/DOM renders will look different from the canvas originals — this
  is a re-author, not a port. Accepted (option C).
- GSAP licence: GSAP 3 standard is free for most uses; vendoring the core `gsap.min.js` is
  fine. Note if any premium plugins are needed (avoid).
