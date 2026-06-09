# html_video → HyperFrames render path + Tiers 1–3

- **Date:** 2026-06-09
- **Status:** Approved (Option A: Node-subprocess engine foundation + tiered sequencing)
- **Revision:** runtime decision changed Deno-first → **Node subprocess (Option A)** per user direction.
- **Owner:** brain
- **Worker:** codex
- **Builds on:** `2026-06-09-html-video-hyperframes-seekable-design.md` (Tier 0, merged `ef3a68d4`)

## Context

Tier 0 made the three `html_video` templates seekable HyperFrames compositions
(`window.__hf = { duration, seek }`). But the render path (`server/render.py`) still
uses real-time `MediaRecorder` capture and **ignores `__hf` entirely**. The valuable
HyperFrames capabilities — audio mixing, `<video>` compositing (Tier 1), shader/HDR
transitions (Tier 2) — are **engine-resident**: they are performed by
`@hyperframes/engine` (`audioMixer`, `videoFrameInjector`, `shaderTransitionWorkerPool`),
driven by the optional `__hf.media`/`__hf.transitions` fields. Declaring those fields on
templates does nothing until an engine consumes them.

**Therefore the gating prerequisite for Tiers 1–2 is swapping the render path to the
HyperFrames engine.** The engine requires a JS/TS runtime + Puppeteer (Chromium) +
ffmpeg.

### Runtime decision: Node subprocess (Option A)

The HyperFrames engine's **native runtime is Node** (`package.json: engines.node >=22`,
`puppeteer ^24`, `worker_threads`, `@hono/node-server`). Running it on its native runtime
removes all node-compat risk (Puppeteer launch, worker pools, hono server all work
unmodified). The cost is that this app — Python-only today (`spur-app.json:
mcp_server.type = "python"`) — gains a **Node 22 + Chromium + ffmpeg** dependency.

**Approach:** bundle the pinned `@hyperframes/engine@0.6.84` and invoke it from a small
Node entry script; the Python render tool (`server/render.py` / `html_video_render`)
**shells out to a Node subprocess** over an `__hf` composition. `html_video_render`
becomes a thin invoker; the seek→BeginFrame→ffmpeg determinism replaces the real-time
capture gates.

**Runtime provisioning** is part of the foundation: declare the Node toolchain + engine
deps in the app (alongside the existing Python `requirements.txt`), resolve a Chromium for
Puppeteer (prefer `PUPPETEER_EXECUTABLE_PATH` over a fresh download), and ensure `node`
is discoverable from the Python process.

(The previously-considered Deno-first path is dropped; Deno node-compat for Puppeteer was
the gating unknown and Option A sidesteps it by using the engine's native runtime.)

## Goal

Replace html_video's real-time capture render with HyperFrames' deterministic engine,
then incrementally light up the engine's higher-tier capabilities (audio/video,
transitions, declarative authoring).

## Decomposition (DAG)

### F0 — Node engine render harness *(no deps)*
Stand up `@hyperframes/engine@0.6.84` on its native Node runtime and prove the invocation
contract F1 will build on.
- **Goal:** a Node entry script `app_gallery/html_video/engine/render-hf.mjs` (new dir)
  that loads a trivial seekable page (`window.__hf={duration:2, seek(t){…canvas draw…}}`),
  drives the engine's BeginFrame capture for `duration·fps` frames, and muxes to MP4 with
  ffmpeg. Add a pinned `package.json` (engine + puppeteer) and document the Node version +
  how Chromium is resolved (`PUPPETEER_EXECUTABLE_PATH` preferred).
- **Acceptance:** `node render-hf.mjs …` produces a non-empty, playable MP4 of the right
  duration/fps. Document the exact CLI contract (args in, MP4 out) so F1 can shell to it.
- **Note:** this is foundation, not a gamble — Node is the engine's native runtime; the
  task is provisioning + establishing the subprocess contract, not de-risking compat.

### F1 — render path swap *(depends: F0)*
Route `html_video_render` through the engine.
- **Goal:** add an engine render mode to the app: given a composition (an html_video
  template/`app.ipynb` cell exposing `window.__hf`) + duration/fps/resolution, produce MP4
  by shelling to the F0 Node render harness. Keep the existing webm/port path intact as a
  deprecated fallback during transition.
- **Acceptance:** rendering a Tier 0 template (e.g. `frame-liquid-hero`) through the new
  path yields a deterministic MP4 (same bytes across two runs at fixed fps) with no
  reliance on the real-time capture gates. Existing Python tests stay green.

### T1 — audio + `<video>` *(depends: F1)*
- **Goal:** extend the content model + templates to declare `__hf.media`
  (`HfMediaElement[]`); the engine mixes audio and composites `<video>` out-of-band.
- **Acceptance:** a composition with one audio track + one `<video>` renders an MP4 whose
  audio is present and in sync, and whose video frames appear composited.

### T2 — shader/HDR transitions *(depends: T1)*
- **Goal:** multi-scene compositions declaring `__hf.transitions`; add
  `@hyperframes/shader-transitions`; engine does HDR-aware transition compositing.
- **Acceptance:** a 2-scene composition with one shader transition renders a clean
  crossfade/transition in the output MP4.

### T3 — declarative authoring *(depends: F1; independent of T1/T2)*
- **Goal:** adopt `@hyperframes/core` so compositions can be authored declaratively
  (timeline + framework adapters) instead of hand-written canvas `seek`. Largest scope;
  reshapes the authoring model. Keep hand-written `__hf` templates working alongside.
- **Acceptance:** at least one template authored declaratively via `core` renders
  identically through the F1 engine path.

## Constraints (all tasks)

- Run the engine on Node (Option A). Pin the Node toolchain + `@hyperframes/*` versions;
  resolve Chromium via `PUPPETEER_EXECUTABLE_PATH` where possible.
- Keep determinism: render output must be reproducible at a fixed fps.
- Each task reads its predecessor's merged branch before starting; downstream tasks adapt
  to the integration shape F0/F1 establish rather than assuming it.
- No secrets/network beyond fetching the pinned `@hyperframes/*` npm packages.
- Scope engine/runtime code under `app_gallery/html_video/engine/`; keep `server/` Python
  changes minimal (invocation glue only).

## Sequencing rationale

F0 establishes the Node engine harness + subprocess contract before any rewrite. F1 is the
load-bearing foundation that unblocks T1/T2/T3. T1→T2 are strictly ordered (transitions
build on the media model). T3 is a parallel track off F1.
