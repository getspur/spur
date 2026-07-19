# SPUR Concept Proof Film Series Design

**Status:** User-approved design
**Date:** 2026-07-19
**Surface:** Product Hunt and social launch video series
**Format:** Three 40-second, 1920x1080, 30fps proof films

## Summary

SPUR needs a short video series that explains one product concept at a time and
then proves that concept with real SPUR TUI footage. The series will use a
match-cut structure: a focused concept animation ends in the same geometry as
the first real capture frame, so the explanation transforms into evidence
instead of cutting to an unrelated screen recording.

The first series contains three films:

1. **Keep the control loop** - human direction, Brain planning, worker
   delegation, evidence return, review, and resume.
2. **Work survives the session** - durable plan, lineage, evidence, and review
   state across an interrupted agent session.
3. **Bring any ACP agent** - Claude Code, Grok, Codex, and OpenCode entering the
   same SPUR control system through ACP-compatible routing.

The open html-video Jute notebook remains the motion-design source of truth.
Palmier Pro assembles the rendered concept plates with reviewed real captures,
music, diagnostic labels where required, and the shared end card. Existing
motion plates, Palmier timelines, and exports remain unchanged.

## Goals

- Make each concept understandable before showing the product UI.
- Connect every conceptual claim to visible behavior in existing SPUR material.
- Give each film one narrative job and one memorable takeaway.
- Preserve a coherent visual family across the three films without repeating
  the same animation.
- Use the real `spur` repository, real sessions, and the working agent roster:
  Claude Code, Grok, Codex, and OpenCode.
- Keep approved and diagnostic source status explicit throughout production.

## Non-goals

- Replacing the existing 45-second and 90-second V2 hero cuts.
- Producing one film that explains every SPUR feature.
- Inventing UI, agent events, plan state, or proof that is absent from the
  materialized sessions.
- Removing the `DRAFT - DIAGNOSTIC CAPTURE` watermark from unapproved footage.
- Generating new narration, music, or paid AI media in the initial pass.
- Claiming broad ACP compatibility through logos or agents that are not part of
  the approved working roster.

## Audience and success criterion

The primary viewer is a developer evaluating SPUR on Product Hunt or social
media who has not used a multi-agent coding harness before. Each film succeeds
when the viewer can answer both questions after one viewing:

1. What problem does this SPUR mechanism solve?
2. Where did I see that mechanism operating in the real product?

Copy must remain legible at a 640-pixel-wide preview. The concept-to-product
transition must remain understandable with audio muted.

## Shared 40-second structure

Every film uses the same exact timeline envelope:

| Time | Chapter | Purpose |
|---|---|---|
| 0-3s | Problem hook | Show one concrete failure state. |
| 3-13s | Concept model | Explain the relevant SPUR mechanism. |
| 13-16s | Match cut | Transform the concept geometry into real TUI geometry. |
| 16-35s | Real proof | Show uninterrupted materialized SPUR behavior with restrained labels. |
| 35-40s | Takeaway | State one claim and show the shared end card. |

All final film outputs are exactly 40.000 seconds / 1200 frames at 30fps.

## Shared visual language

### Control spine

A cyan line acts as the persistent SPUR control spine. Requests, plan tasks,
worker returns, session state, and agent ports attach to this line. The spine
survives every transition and provides the visual continuity across the series.

### Palette and typography

The series reuses the existing SPUR motion system:

- Ink: `#0B0E14`
- Surface: `#111620`
- Ivory: `#E6E1CF`
- Cyan: `#7FB4CA`
- Violet: `#957FB8`
- Line: `#2A2E38`
- Muted: `#AEB6C5`

Interface labels use a monospaced stack; conceptual headlines use the existing
sans-serif stack. No new decorative palette is introduced.

### Motion rules

- Motion expresses state change, routing, persistence, or evidence return.
- Each object has one semantic role; decorative particles and generic robot
  imagery are prohibited.
- Layout interpolation, line drawing, opacity, restrained scale, and short
  masked reveals form the primary motion vocabulary.
- The final concept frame reproduces the major panel proportions and focal
  positions of the first real capture frame.
- If the geometry cannot align honestly, the cyan control spine performs a
  visible 8-12-frame wipe. The edit must not imply a seamless UI state that did
  not exist.

## Film 1: Keep the control loop

### Story

The problem hook shows one user request expanding into hidden parallel work.
The concept model then makes the control loop explicit:

1. The user request enters the Brain.
2. `submit_plan` creates four bounded tasks.
3. The Brain delegates to Claude Code, Grok, Codex, and OpenCode.
4. Evidence returns to one review gate.
5. The user chooses whether to approve, redirect, or resume.

The final task lanes match-cut into Session Detail, worker visibility, and the
real four-worker result layout. The proof chapter must show actual task/worker
state and returned evidence, not a generic terminal montage.

### Material mapping

- Approved Session Detail proof:
  `live_demos/13-problem-plan-loop-drive.mp4`
- Approved worker-state proof:
  `live_demos/10-problem-ops-visibility.mp4`
- Approved plan-state proof:
  `live_demos/11-problem-plan-progress.mp4`
- Four-agent evidence when required:
  Palmier media `F2C142AD`, source SHA256
  `b5c407a3753bae990b0cdf95fd5dac2c747934e15f8a314aaff42e52bf83ecb5`

Any segment from `F2C142AD` remains visibly diagnostic.

### Takeaway

`Delegate deeply. Keep the decision.`

## Film 2: Work survives the session

### Story

The problem hook removes the active agent connection mid-task. The concept
model separates process state from durable SPUR state:

1. The active agent process fades.
2. The plan, delegation lineage, evidence, and review state remain attached to
   the control spine.
3. A new session reconnects.
4. The preserved state rehydrates and the loop continues.

The durable-state stack match-cuts into the real session timeline and resume
surface. The proof chapter must visibly include the prior session identity and
`Resumed from prior conversation`.

### Material mapping

- Approved resume proof: `live_demos/04-session-resume.mp4`
- Approved source SHA256:
  `cb110d2cfa9149cb9d8344987f03f11852a181926ee85a572bebf8dbdff0660c`
- Supporting Session Detail proof:
  `live_demos/13-problem-plan-loop-drive.mp4`

### Takeaway

`The agent can stop. The work remains.`

## Film 3: Bring any ACP agent

### Story

The problem hook shows four agent-specific workflows diverging. The concept
model reconnects them through identical ACP ports:

1. Claude Code, Grok, Codex, and OpenCode appear as distinct endpoints.
2. Each endpoint attaches to the same control spine.
3. Agent, model, and effort remain explicit at dispatch.
4. Their work enters the same plan, worker, evidence, and review model.

The four ports match-cut into the real routing controls and worker rows. The
proof chapter must show `agent=`, `model=`, and `effort=` plus visible working
agent identities. The animation may explain the shared harness, but the demo
must not imply unsupported event parity beyond the recorded behavior.

### Material mapping

- Approved specialist-routing proof: `live_demos/09-product-e2e-flow.mp4`
- Approved source SHA256:
  `7fd8473a7870afff7b5085c6a00ef306ac257b0021d8f150884886caa84d47ec`
- Four-agent identity proof when required: Palmier media `F2C142AD`

Any four-agent diagnostic segment retains its watermark.

### Takeaway

`Choose the agent. Keep one control system.`

## Notebook architecture

The open notebook at
`/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb` remains the
single motion source of truth.

### Components

1. **Evidence manifest cell** - records source file/media ID, approved SHA256,
   proof terms, source window, crop geometry, and promotability for every real
   clip used by the series.
2. **Shared motion engine** - owns palette, typography, grid, control-spine
   primitives, easing, deterministic Anime.js state, and canvas rendering.
3. **Story configurations** - three declarative scene graphs containing copy,
   timings, nodes, transitions, and target match-cut geometry.
4. **Interactive storyboard** - one self-contained `text/html` filmstrip that
   lets the user switch films, scrub conceptual scenes, and compare the target
   capture frame beside the final animation frame.
5. **Sequential capture cell** - renders one selected story through the existing
   `spur-ad-capture` port. The single-port limitation is deliberate; films are
   captured and rendered sequentially.
6. **Render cell** - calls `html_video_render` and writes versioned concept-plate
   MP4 files into the worktree.
7. **Final notebook artifact** - one `text/html` panel with the three concept
   plates, exact metadata, and links/records for the assembled outputs.

Anime.js remains pinned to version 4.4.1 and is inlined into outputs. Rendered
HTML contains no external runtime resources.

## Palmier assembly

Palmier Pro will create three new timelines without modifying the existing V2
timelines or exports:

- `Proof Film 1 - Control Loop - 40s`
- `Proof Film 2 - Durable Memory - 40s`
- `Proof Film 3 - ACP Agents - 40s`

Each timeline contains the concept plate, geometry-aligned match cut, reviewed
real proof, restrained proof labels, existing music trimmed to 40 seconds, and
the domain-free `INSTALL SPUR · COMMUNITY FREE` end card. No narration or new
paid generation is required for the initial pass.

Expected worktree outputs:

- `docs/product_launch/media_pack/ph_ready/series/spur-control-loop-proof-40s.mp4`
- `docs/product_launch/media_pack/ph_ready/series/spur-durable-memory-proof-40s.mp4`
- `docs/product_launch/media_pack/ph_ready/series/spur-acp-agents-proof-40s.mp4`

## Data flow

1. The evidence manifest supplies approved source identity and proof terms.
2. A story configuration drives the shared canvas renderer.
3. The storyboard renders the concept and target capture geometry side by side.
4. The selected story captures through `spur-ad-capture`.
5. `html_video_render` writes an exact 16-second hook-plus-concept-plus-match
   plate.
6. Palmier places that plate at frames 0-480, real proof at frames 480-1050, and
   the takeaway/end card at frames 1050-1200.
7. Final exports return to the media pack with hashes and verification records.

The renderer and Palmier must use returned timing boundaries. Implementations
must not infer asset timing from filenames.

## Failure handling

- A missing or mismatched source SHA blocks use of that real clip.
- An unapproved source automatically sets the segment and final film to
  diagnostic/non-promotable and requires a visible watermark.
- A notebook capture that produces no port bytes is rerun only after checking
  the active-output capability and port manifest.
- A `html_video_render` text response beginning with an execution error is
  treated as failure even if the notebook tool call itself returned normally.
- A failed match-cut alignment falls back to the explicit cyan-spine wipe.
- Palmier timeline edits always occur on new duplicates. A failed edit triggers
  a timeline re-read before any further index-based operation.
- Existing source timelines and exports are never overwritten.

## Verification

### Notebook checks

- Every storyboard and capture artifact yields exactly one `text/html` output.
- The interactive storyboard contains all three film configurations.
- Static and full notebook doctor checks pass.
- The notebook DAG contains no failed or stale production cells.
- Rendered HTML has no remote resources or unrelated project domains.

### Media checks

- Every concept plate and final export fully decodes with ffmpeg.
- Final exports are H.264 video plus AAC stereo audio.
- Resolution is 1920x1080 at 30fps.
- Duration is exactly 40.000 seconds / 1200 video frames per film.
- SHA256 values are recorded in the media-pack notebook.

### Visual checks

- Review frames at the problem hook, concept midpoint, match-cut boundary,
  proof midpoint, and end card for every film.
- Compare the final conceptual frame and first real frame side by side.
- Verify proof labels do not obscure source evidence.
- Verify copy remains legible at 640 pixels wide.
- Verify diagnostic watermarks remain visible on every unapproved segment.
- Verify no external domain appears on the end card.

## Acceptance criteria

The series is complete when all three films:

1. Follow the approved 3s/10s/3s/19s/5s narrative structure.
2. Explain exactly one concept and show its matching real proof.
3. Use only evidence-manifest sources with validated identity.
4. Preserve diagnostic labeling wherever required.
5. Pass notebook, media, and visual verification.
6. Exist as new committed outputs without modifying prior V2 deliverables.
