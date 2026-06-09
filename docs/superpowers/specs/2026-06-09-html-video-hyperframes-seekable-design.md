# html_video → HyperFrames seekable templates (Tier 0)

- **Date:** 2026-06-09
- **Status:** Approved (Tier 0 scope), Tiers 1–3 deferred
- **Owner:** brain
- **Worker:** codex

## Context

`app_gallery/html_video` authors short videos as animating HTML/canvas documents and
renders them to MP4. Today the render path is a **real-time capture** pipeline: each
template animates with `requestAnimationFrame` driving `frame(now)` off the wall clock,
and the notebook records the canvas with `MediaRecorder` (the `jute-video-capture`
message → media port → `html_video_render` ffmpeg path). That pipeline is fragile: it
depends on three runtime gates (canvas-capture DAG source, the iframe-name == port
contract, and the "active content in outputs" toggle) plus a real-time wait equal to the
clip length.

HeyGen's **HyperFrames** (`/tmp/hyperframes`, inspected) renders any web page to MP4
**deterministically** by seeking a virtual clock. Its engine has exactly one contract
with a page (`packages/engine/src/types.ts:69`):

```ts
interface HfProtocol {           // window.__hf
  duration: number;              // total seconds
  seek(time: number): void;      // must produce deterministic visual output at `time`
  media?: HfMediaElement[];      // optional: audio / <video> the engine mixes out-of-band
  transitions?: HfTransitionMeta[]; // optional: shader transition metadata
}
```

> "The engine does NOT care what animation framework drives the page … anything works as
> long as `seek()` produces deterministic visual output for a given time."

**Key finding:** html_video's templates are *already* pure functions of time —
`frame-liquid-hero/template.html:50` computes `t = now/1000` and every draw call is
`Math.sin(t·…)`/`Math.cos(t·…)` with no retained state. The only structural gap to
HyperFrames compatibility is that the templates read the **real** clock and never expose
`window.__hf`.

## Goal

Make every reusable html_video template a **seekable HyperFrames composition** by
exposing `window.__hf = { duration, seek }`, without changing visual output and without
touching the Python render path. This is the authoring-side foundation; it future-proofs
the templates and is the prerequisite for later routing renders through HyperFrames'
deterministic engine.

## Scope (Tier 0)

In scope:
- The 3 reusable templates:
  - `templates/frame-liquid-hero/template.html`
  - `templates/frame-glitch-title/template.html`
  - `templates/frame-data-rollup/template.html`

Out of scope (deferred, recorded as future tiers):
- **Tier 1** — declare `__hf.media` to gain audio mixing + `<video>` compositing (a new
  capability html_video lacks today).
- **Tier 2** — `__hf.transitions` + `@hyperframes/shader-transitions` for HDR scene
  transitions.
- **Tier 3** — adopt HyperFrames' `core` authoring layer (declarative timelines +
  framework adapters).
- Routing `html_video_render` through the HyperFrames Node engine (Puppeteer/BeginFrame).
  Requires resolving the Node-22 runtime question vs. the current Python+ffmpeg server;
  tracked separately.

## Design

Each template currently ends its draw function with a self-scheduling rAF loop:

```js
function frame(now){ const t = now/1000; /* … pure draws of t … */ requestAnimationFrame(frame); }
requestAnimationFrame(frame);
```

Refactor to separate **drawing** (pure function of time) from **scheduling**:

```js
// 1. Pure draw — identical math, time injected in ms. No rAF inside.
function draw(nowMs){ const t = nowMs/1000; /* … unchanged draw body … */ }

// 2. Canonical duration (seconds). Honor window.__FRAME__.duration_ms when present,
//    else a template default (DURATION_SEC constant).
const DURATION_SEC = (window.__FRAME__ && window.__FRAME__.duration_ms)
  ? window.__FRAME__.duration_ms / 1000 : 6;

// 3. The HyperFrames seek protocol — the only new contract.
window.__hf = { duration: DURATION_SEC, seek: (time) => draw(time * 1000) };

// 4. Live preview keeps a real-time loop, but drives it THROUGH seek so preview and
//    deterministic render share one code path.
const start = performance.now();
function preview(now){
  window.__hf.seek(((now - start) / 1000) % window.__hf.duration);
  requestAnimationFrame(preview);
}
requestAnimationFrame(preview);
```

Notes:
- `seek(time)` must be idempotent and order-independent (any `time` → same pixels). The
  existing math already satisfies this; the refactor must not introduce frame-to-frame
  state (no accumulators, no `+=`).
- `resize()` and canvas sizing stay as-is; they are not time-dependent.
- The `data-capture="true"` canvas attribute stays so the legacy capture path keeps
  working unchanged during the transition.

## Acceptance criteria

1. All three templates expose a `window.__hf` object with a numeric `duration` and a
   `seek(time)` function.
2. `seek(t)` renders identical pixels for the same `t` regardless of call order
   (deterministic; no retained inter-frame state).
3. Live preview still animates (driven via `__hf.seek`).
4. No change to `server/render.py` or the existing capture/media-port path.
5. No external/CDN assets introduced; templates remain self-contained.

## Verification

- Manual: open each template, confirm it animates; in console, call
  `window.__hf.seek(0)` then `window.__hf.seek(2)` then `window.__hf.seek(0)` and confirm
  the canvas returns to the identical first-frame image (determinism).
- Existing `app_gallery/html_video/tests/` (Python) must remain green — Tier 0 does not
  touch the server, so they should be unaffected.

## Future tiers (recorded, not built)

| Tier | Adds | Cost |
|---|---|---|
| 0 (this) | `window.__hf` seek contract on templates | ~authoring refactor, no runtime change |
| 1 | audio mixing + `<video>` compositing via `__hf.media` | engine-resident, needs Node render path |
| 2 | HDR shader transitions via `__hf.transitions` | engine-resident |
| 3 | declarative timelines + GSAP/Lottie/anime adapters | adopt `@hyperframes/core` |
