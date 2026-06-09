# html_video → HyperFrames media rework (renderOrchestrator)

- **Date:** 2026-06-09
- **Status:** Approved (rework foundation + redo of deferred tiers)
- **Owner:** brain
- **Worker:** codex
- **Builds on:** `2026-06-09-html-video-hyperframes-render-path-design.md` — F0+F1 merged to
  local main (`75f28606`, `ca85e4ec`): a **Bun** engine render path that renders any
  `window.__hf` canvas composition to MP4 via `bun render-hf.mjs <composition> <duration>
  <fps> <WxH> <out>`.

## Why a rework

F0/F1's harness (`engine/render-hf.mjs`) uses the engine's **low-level primitives**
(`createCaptureSession → captureFrame → encodeFramesFromDir`) which encode **video frames
only**. Brain verified that declaring `window.__hf.media` produces an MP4 with **no audio
stream** — audio mixing (`processCompositionAudio`/`muxVideoWithAudio`) and `<video>`
compositing (`injectVideoFramesBatch`) are separate engine services the harness never
calls. The same gap blocks shader transitions.

`@hyperframes/producer`'s **`renderOrchestrator`** is the high-level entry that runs the
full pipeline — duration discovery, **media reconciliation**, an **audio stage**
(`runAudioStage`, `audio.aac` sidecar + mux), and **transition compositing** (reads
`window.__hf.transitions`, dual-scene loop). Rebuilding the harness on it makes media
(T1) and transitions (T2) mostly "declare it in the composition." `@hyperframes/
producer@0.6.84` is on npm (`main: dist/index.js`); consumed under **Bun** (same runtime
constraint as the engine — see prior spec; Node ESM cannot import these packages).

## Runtime + platform (unchanged, carried over)

- **Runtime: Bun.** Node ESM cannot import `@hyperframes/*` (src-only exports +
  extensionless dist imports). This is FINAL — do not reintroduce Node.
- **Determinism is Linux-only.** BeginFrame (deterministic capture) crashes on
  macOS/Windows (crbug.com/40656275) → screenshot mode off-Linux. Audio mixing and video
  injection are **out-of-band ffmpeg** steps and work on macOS regardless. Bit-determinism
  is verified on a Linux render target, not the macOS dev box.

## Decomposition (DAG) — all Bun

### R0 — orchestrator harness *(no deps)*
Swap the harness internals from low-level primitives to `producer.renderOrchestrator`,
**keeping the exact same CLI contract** so F1's Python (`bun render-hf.mjs <composition>
<duration> <fps> <WxH> <out>`) is unchanged.
- Add `@hyperframes/producer@0.6.84` (pinned) to `engine/package.json`.
- First PROVE the orchestrator imports + runs under Bun (it is the same packaging family
  as the engine — expect Bun-only). Then wire `render-hf.mjs` to call `renderOrchestrator`
  for an `__hf` composition → MP4.
- **Acceptance:** parity with F0 — `bun render-hf.mjs ../templates/frame-liquid-hero/
  template.html 2 30 1280x720 out.mp4` produces a valid MP4 (right duration/fps). Keep the
  Bun shebang/guard. Do NOT change F1's Python contract. *(Brain verifies on real machine —
  codex sandbox cannot launch Chromium.)*

### R1 — audio + `<video>` via `__hf.media` *(depends: R0)*
- Re-add the `frame-media-mix` template + media assets (reuse from the deferred T1 branch
  `spur/worker/v2/codex/9b01edfaa15cf583/16c98fd0-0caa-4540-b5a4-23027db5fa40` — the
  `__hf.media` declaration there is contract-correct).
- With R0's orchestrator harness, `__hf.media` is mixed/composited automatically.
- **Acceptance:** rendering `frame-media-mix` yields an MP4 **with an audio stream present**
  and the `<video>` composited. *(Brain verifies the audio stream on the real machine.)*

### R2 — shader/HDR transitions via `__hf.transitions` *(depends: R1)*
- Add `@hyperframes/shader-transitions` (pinned); a multi-scene composition declaring
  `window.__hf.transitions`; orchestrator applies the transition.
- **Acceptance:** a 2-scene composition with one transition renders a clean transition.

### R3 — declarative authoring via `@hyperframes/core` *(depends: R0)*
- Adopt `@hyperframes/core` so a composition can be authored declaratively (timeline +
  adapters) compiling to `window.__hf`, rendered through R0. Keep hand-written templates
  working. Largest scope; deliver ONE declaratively-authored template.

## Constraints (all tasks)

- **Bun only.** The task descriptions in the plan are authoritative on this; do NOT
  reintroduce Node anywhere (engine/, render.py, tests). Node is verified-impossible.
- Pin all `@hyperframes/*` at `0.6.84` (shader-transitions/core at their matching version).
- Keep F1's Python CLI contract (`bun render-hf.mjs <args>`) stable — R0 changes harness
  internals only.
- codex CANNOT verify renders in-sandbox (Chromium blocked). Do NOT claim a render works
  without running it; report "could-not-verify-in-sandbox" and brain verifies on the real
  machine.

## Sequencing rationale

R0 re-establishes the foundation (media/transition-capable) with no Python change. R1→R2
are ordered (transitions build on the media pipeline). R3 is a parallel authoring track off
R0. Brain verifies every render on the real machine at each gate.
