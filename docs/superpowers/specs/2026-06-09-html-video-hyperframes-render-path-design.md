# html_video → HyperFrames render path + Tiers 1–3

- **Date:** 2026-06-09
- **Status:** Approved (Deno-first foundation + tiered sequencing)
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

### Runtime decision: Deno-first, Node-subprocess fallback

SPUR already provisions a **Deno kernel** (`crates/spur-notebook/.../kernel_provision.rs`;
`dag/engine.rs` supports the `deno` kernelspec). Reusing Deno avoids introducing a Node 22
toolchain into the Python-only app. Deno 2 can import `npm:@hyperframes/engine@0.6.84` and
provides node-compat for `node:` builtins / `child_process`.

**Risk surface (why F0 is a spike, not an assumption):** the engine uses Puppeteer 24,
`worker_threads`, and `@hono/node-server`. Puppeteer-under-Deno + worker pools are the
real unknowns. The CDP BeginFrame capture itself is WebSocket-level and portable; the
question is whether `npm:puppeteer` launches and connects under Deno node-compat.

- **Primary:** run the engine under `deno run -A` (reuse provisioned Deno).
- **Fallback (only if F0 fails):** bundle Node 22 + the engine and spawn a Node subprocess
  from the Python render tool.

Either way, `html_video_render` becomes a thin invoker of the engine over an `__hf`
composition; the seek→BeginFrame→ffmpeg determinism replaces the real-time capture gates.

## Goal

Replace html_video's real-time capture render with HyperFrames' deterministic engine,
then incrementally light up the engine's higher-tier capabilities (audio/video,
transitions, declarative authoring).

## Decomposition (DAG)

### F0 — Deno engine spike *(no deps)*
Prove `@hyperframes/engine@0.6.84` renders under Deno.
- **Goal:** a `render-hf.ts` that, via `deno run -A`, loads a trivial seekable page
  (`window.__hf={duration:2, seek(t){…canvas draw…}}`), drives the engine's BeginFrame
  capture for `duration·fps` frames, and muxes to MP4 with ffmpeg.
- **Acceptance:** a non-empty, playable MP4 of the right duration/fps, OR a precise
  go/no-go report naming the exact Deno-compat blocker (Puppeteer launch, worker_threads,
  hono server, etc.). Keep the spike script in `app_gallery/html_video/engine/` (new dir).
- **Gate:** green → F1 uses Deno; red → F1 uses the Node-subprocess fallback.

### F1 — render path swap *(depends: F0)*
Route `html_video_render` through the engine.
- **Goal:** add an engine render mode to the app: given a composition (an html_video
  template/`app.ipynb` cell exposing `window.__hf`) + duration/fps/resolution, produce MP4
  via the engine (Deno path from F0, or fallback). Keep the existing webm/port path intact
  as a deprecated fallback during transition.
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

- Reuse the provisioned Deno runtime where possible; do not add a Node toolchain unless
  F0 forces the fallback.
- Keep determinism: render output must be reproducible at a fixed fps.
- Each task reads its predecessor's merged branch before starting; downstream tasks adapt
  to the integration shape F0/F1 establish rather than assuming it.
- No secrets/network beyond fetching the pinned `@hyperframes/*` npm packages.
- Scope engine/runtime code under `app_gallery/html_video/engine/`; keep `server/` Python
  changes minimal (invocation glue only).

## Sequencing rationale

F0 de-risks the entire arc before any rewrite. F1 is the load-bearing foundation that
unblocks T1/T2/T3. T1→T2 are strictly ordered (transitions build on the media model).
T3 is a parallel track off F1. If F0 returns red, brain re-scopes F1 to the Node fallback
at the review gate before F1 dispatches.
