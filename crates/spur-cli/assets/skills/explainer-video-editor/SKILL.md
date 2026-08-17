---
name: explainer-video-editor
description: Use when creating or enhancing a sourced explainer, product story, demo-led launch video, narrated concept film, or rough cut that combines notebook design, HTML motion graphics, generated media, real captures, and final video editing.
---

# Explainer Video Editor

## Operating rule

**Core principle:** Let the notebook decide the story and PalmierPro decide the final frame. Give every asset one owner and every scene one primary owner, and clear three approval gates before spending credits.

**REQUIRED SUB-SKILLS:**

- Use `open-design` for the interactive brief, claim register, visual direction, layout, and storyboard.
- Use `html-video` for deterministic notebook-canvas motion plates.

Read `references/handoff-contract.md` before creating the scene ownership map or importing anything into PalmierPro.

## Choose the route

- **Create:** Start from a brief, source documents, and unedited media. Build the claim register before scripting.
- **Enhance:** Use this route when an existing demo or rough cut already carries the story. Begin by inspecting the media in PalmierPro, record exact source ranges, then use the notebook to design only the missing beats.

Inspect content rather than inferring it from filenames. Ground every factual statement, product behavior, and proof beat in an approved source.

## Keep ownership explicit

| Tool | Owns |
|---|---|
| Open Design | Intent, sources, claim register, direction, layouts, approvals, storyboard, and reference manifest |
| HTML Video | Exact-duration, deterministic, self-contained motion plates without narration |
| Higgsfield | Dry narration and approved metaphorical or atmospheric footage |
| PalmierPro | Source inspection, real captures, native text, timeline assembly, mix, color, and export |

Assign every asset one owner. Every scene has one primary owner. If PalmierPro composites differently owned input assets, Palmier is the scene owner while inputs retain their own asset owners. Never generate factual UI, product copy, logos, title cards, or proof. Never use Higgsfield `explainer_video` or any generated assembler. Use PalmierPro as the only final editor.

## Gate 1: Concept and layout

Lock the audience, purpose, route, duration, aspect ratio, CTA, sources, rights, and deliverables. Build the brief and claim register, then use Open Design to present two or three visual directions. Wait for approval of the palette, typography system, composition language, and pacing envelope. Treat existing approvals as locked decisions and do not ask for them again.

## Gate 2: Script and storyboard

Write narration only from approved claims. Build a timecoded scene graph with one primary owner per scene: `real-capture`, `html-video`, `higgsfield`, or `palmier`. Record exact source start and end seconds. Specify every plate, generated insert, native title, CTA, and any captions or music the user explicitly requested. Wait for approval of the narration/script, storyboard, source selects, and scene ownership map.

After approval, use HTML Video to render exact-duration, self-contained plates with no baked narration.

## Gate 3: Paid generation

Inspect the live account, available voices, and current model contracts. Show the exact voice, models, prompts, durations, aspect ratio, shot count, and retry ceiling. Wait for approval unless those exact values are already approved.

Generate all narration first with one voice, then check each segment against its scene duration. Generate only approved creative shots afterward. Keep shots silent or ambient-only; prohibit dialogue, captions, readable UI, claims, logos, title cards, and product proof.

Rejoin timed-out jobs instead of duplicating them. Retry only failed jobs. After two equivalent failures, change the prompt or parameters; renew approval if cost or scope changes.

## Assemble in PalmierPro

Create a new project or a separate timeline. Call `get_timeline` once and apply delta patches. After any failure or out-of-band edit, reread the active timeline, then preserve successful edits and retry the smallest failed mutation. Import and inspect every asset before placement. Organize clips, then place them with PalmierPro frame semantics and recorded source seconds.

Keep narration on its own track and lower ambience beneath it. Create all product text natively in PalmierPro. Use restrained transitions, color, and audio treatment. Inspect representative frames and every important cut. Export, monitor the queue through its terminal result, and accept only a successful export. After Gate 3, make reversible edits without per-edit approval gates.

## Deliver and verify

Create the reference manifest defined in the handoff contract. Run exactly:

```sh
assets/skills/explainer-video-editor/scripts/validate-delivery.sh MANIFEST.json VIDEO.mp4
```

Deliver the verified H264/AAC MP4, editable PalmierPro project, notebook, and manifest. Report duration, width, height, frame rate, audio codec, `checksum_sha256`, voice, models, subtitle status, and sources. Do not assume captions, ProRes, music, alternate ratios, or extra exports.

## Stop conditions

Stop on an unsupported claim, missing Gate 3 approval before paid generation, unavailable required source, or missing account login. Ask for the missing evidence, approval, source, or access. A missing Higgsfield assembler is not a blocker because PalmierPro performs final assembly.
