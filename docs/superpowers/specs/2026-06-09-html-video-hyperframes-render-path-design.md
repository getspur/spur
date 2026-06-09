# html_video → HyperFrames render path + Tiers 1–3

- **Date:** 2026-06-09
- **Status:** Approved (Bun-subprocess engine foundation + tiered sequencing)
- **Revision history:** Deno-first → Node subprocess (Option A) → **Bun subprocess**.
  Empirical F0 verification (2026-06-09) proved `@hyperframes/engine@0.6.84` is NOT
  consumable by stock Node ESM (same packaging defects that blocked Deno); it renders
  cleanly under **Bun**, the engine's native bundler-style runtime.
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

### Runtime decision: Bun subprocess (verified)

**Empirical finding (F0, 2026-06-09):** `@hyperframes/engine@0.6.84` cannot be imported by
stock **Node** ESM. Two runtime-agnostic packaging defects:
1. the package `exports` map resolves only to `./src/index.ts` (TypeScript source); the
   compiled `dist/` is shipped but unexported, so bare and deep-subpath imports fail
   (`ERR_UNSUPPORTED_NODE_MODULES_TYPE_STRIPPING` / `ERR_PACKAGE_PATH_NOT_EXPORTED`).
2. the compiled `dist` uses **extensionless relative imports** that Node ESM rejects
   (`Cannot find module '.../@hyperframes/core/dist/core.types'`).

These are exactly why the earlier Deno spike failed too — the engine is built for
**bundler-style runtimes (Bun)**, not Node/Deno ESM.

**Verified working path:** under **Bun**, the F0 harness renders unmodified — it launched
Chrome, ran the engine's BeginFrame capture, and encoded a correct MP4 (1280×720 h264,
60 frames, 2.000s) from the real `frame-liquid-hero` template. Bun is the engine's native
runtime and resolves both defects with zero workarounds.

**Approach:** bundle the pinned `@hyperframes/engine@0.6.84` and invoke it from a small
entry script run under **Bun**; the Python render tool (`server/render.py` /
`html_video_render`) **shells out to a Bun subprocess** (`bun render-hf.mjs …`) over an
`__hf` composition. `html_video_render` becomes a thin invoker; the
seek→BeginFrame→ffmpeg determinism replaces the real-time capture gates.

**Runtime provisioning** is part of the foundation: ensure a **Bun** binary is available
to the app (alongside the existing Python runtime), resolve a Chromium for Puppeteer
(prefer `PUPPETEER_EXECUTABLE_PATH`), and ensure `bun` is discoverable from the Python
process. Node-preserving alternatives (run via `tsx`/esbuild loader, or pre-bundle with
esbuild) were considered and rejected in favor of Bun, which is proven and workaround-free.

## Goal

Replace html_video's real-time capture render with HyperFrames' deterministic engine,
then incrementally light up the engine's higher-tier capabilities (audio/video,
transitions, declarative authoring).

## Decomposition (DAG)

### F0 — Bun engine render harness *(no deps)*
Stand up `@hyperframes/engine@0.6.84` under **Bun** and prove the invocation contract F1
will build on. **Status: harness written + brain-verified to render under Bun; being
re-pointed from `node` to `bun` (the only working JS runtime).**
- **Goal:** an entry script `app_gallery/html_video/engine/render-hf.mjs` (new dir) run
  under Bun that loads a trivial seekable page (`window.__hf={duration:2, seek(t){…}}`),
  drives the engine's BeginFrame capture for `duration·fps` frames, and muxes to MP4 with
  ffmpeg. Pinned `package.json` (engine + puppeteer); README documents **Bun** as the
  runtime + how Chromium is resolved (`PUPPETEER_EXECUTABLE_PATH` preferred).
- **Acceptance:** `bun render-hf.mjs …` produces a non-empty, playable MP4 of the right
  duration/fps. *(Brain confirmed: 1280×720 h264, 60 frames, 2.000s from `frame-liquid-hero`.)*
  Document the exact CLI contract (args in, MP4 out) — runtime is **`bun`, not `node`** — so
  F1 shells to it. README must state the Node-incompatibility rationale.
- **Note:** Node is NOT viable (engine packaging is bundler-only). Pixel-determinism is
  deferred to F1 (needs the engine's SwiftShader headless shell, not system Chrome).

### F1 — render path swap *(depends: F0)*
Route `html_video_render` through the engine.
- **Goal:** add an engine render mode to the app: given a composition (an html_video
  template/`app.ipynb` cell exposing `window.__hf`) + duration/fps/resolution, produce MP4
  by shelling to the F0 harness via **`bun render-hf.mjs …`**. Keep the existing webm/port
  path intact as a deprecated fallback during transition.
- **Determinism requirement:** F0 showed that pointing Puppeteer at system Chrome yields
  non-deterministic pixels (GPU raster). F1 MUST render through the engine's expected
  **software-raster headless shell (SwiftShader)** — see the engine's `assertSwiftShader`
  / `buildChromeArgs` / `BROWSER_GPU_NOT_SOFTWARE` — so output is reproducible.
- **Acceptance:** rendering a Tier 0 template (e.g. `frame-liquid-hero`) through the new
  path yields a deterministic MP4 (**frame-identical decoded pixels across two runs** at
  fixed fps) with no reliance on the real-time capture gates. Existing Python tests stay green.

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

- Run the engine under **Bun** (verified; Node ESM cannot import the engine). Pin the
  `@hyperframes/*` versions; resolve Chromium via `PUPPETEER_EXECUTABLE_PATH` where possible.
- Keep determinism: render output must be reproducible at a fixed fps.
- Each task reads its predecessor's merged branch before starting; downstream tasks adapt
  to the integration shape F0/F1 establish rather than assuming it.
- No secrets/network beyond fetching the pinned `@hyperframes/*` npm packages.
- Scope engine/runtime code under `app_gallery/html_video/engine/`; keep `server/` Python
  changes minimal (invocation glue only).

## Sequencing rationale

F0 establishes the Bun engine harness + subprocess contract before any rewrite. F1 is the
load-bearing foundation that unblocks T1/T2/T3. T1→T2 are strictly ordered (transitions
build on the media model). T3 is a parallel track off F1.
