# Explainer Video Editor Skill Design

## Purpose

Create a reusable `explainer-video-editor` skill under `assets/skills/` for producing new explainer videos and enhancing existing demos or rough cuts. The skill coordinates notebook-driven design, deterministic HTML motion, Higgsfield generation, and PalmierPro finishing without allowing any tool to become an accidental second editor.

## Core principle

Use the notebook to decide the story and PalmierPro to decide the final frame. Treat Open Design, HTML Video, Higgsfield, real captures, and Palmier-native titles as bounded asset producers with explicit handoffs.

## Supported routes

### Create route

Start from a brief, source material, or product documentation. Establish factual claims and visual direction in the notebook, render deterministic concept plates through HTML Video, generate approved narration and selected creative footage through Higgsfield, then assemble and export in PalmierPro.

### Enhance route

Start by importing and inspecting the existing video in PalmierPro. Record useful source ranges and missing story beats in the notebook. Create only the narration, concept plates, generative inserts, or native titles needed to complete the rough cut, then finish in a separate Palmier timeline or project.

## Ownership boundaries

| Surface | Owns | Must not own |
|---|---|---|
| Open Design notebook | Brief, source register, factual claims, visual direction, layouts, approvals | Final edit, mastering, paid generation |
| HTML Video notebook | Timecoded content graph and deterministic motion plates | Factual product UI, narration, final assembly |
| Higgsfield | Dry narration and approved metaphorical or atmospheric shots | Product UI, readable text, logos, factual claims, final edit |
| PalmierPro | Media inspection, real captures, editorial timeline, native text, captions, mix, color, export | Unapproved paid generation or story invention |
| Delivery manifest | Exact approvals plus claims, eligible source assets, owner-specific provenance, scenes, jobs, and delivery verification | Editorial decisions not represented in the notebook or Palmier timeline |

The notebook is the source of truth for intent, facts, storyboard, approvals, and the claims/assets/scenes manifest. The active Palmier timeline is the source of truth for the delivered audiovisual sequence.

## Three approval gates

### Gate 1 — concept and layout

Lock audience, purpose, route, duration, aspect, CTA, source material, rights constraints, and output requirements. Use Open Design to present two or three directions. Approve one palette, typography system, composition language, and pacing envelope.

### Gate 2 — script and storyboard

Ground factual claims in real sources. Approve the narration, source-video selections, timecoded content graph, HTML motion plates, creative inserts, Palmier-native titles, and final CTA. Assign every scene exactly one primary owner; Palmier owns scenes that composite differently owned input assets.

### Gate 3 — paid generation

Before Higgsfield generation, present the exact voice, model contracts, prompts, durations, aspect ratio, shot count, and retry ceiling. Generate all approved voice assets before creative footage. Rejoin timed-out jobs rather than duplicating them. After two equivalent failures, revise the prompt or parameters instead of repeating the same request.

After Gate 3, reversible Palmier edits proceed without per-edit approval. The final export is delivered for review rather than introducing another mandatory production gate.

## Production flow

1. Detect create or enhance route.
2. Collect and verify real source material.
3. Build the notebook brief and claim register.
4. Approve a visual direction through Open Design.
5. Produce the timecoded script, content graph, and scene ownership map.
6. Approve the script and storyboard.
7. Render approved deterministic concept plates through HTML Video.
8. Inspect live Higgsfield contracts and obtain paid-generation approval.
9. Generate dry narration, then selected creative shots only.
10. Import and inspect all assets in PalmierPro.
11. Assemble real captures, HTML plates, generated footage, voice, ambience, native product text, and optional requested captions or music.
12. Sample the timeline visually, export, verify, and record the final artifact in the notebook and manifest.

## Manifest handoff contract

Use one closed traceability graph:

```text
eligible source asset <- claim.source_asset_ids <- claim <- scene.claim_ids <- scene -> scene.asset_ids -> timeline asset
```

- Root approvals use exactly `concept_layout`, `paid_generation`, and `script_storyboard`, all set to `approved`.
- Claims use unique `claim_id`, nonempty `text`, and unique `source_asset_ids` that resolve only to `real-capture` or `open-design` assets.
- Assets use unique `asset_id`, one owner, `type`, `source_or_job_id`, approval, and rights. Real captures require numeric source start/end seconds; Higgsfield assets require `prompt_or_script_revision`.
- Scenes use unique `scene_id`, one primary owner, numeric `timeline_slot`, unique known `asset_ids`, and unique known `claim_ids`. Non-Palmier scene inputs match the scene owner; Palmier scenes may composite mixed-owner inputs.
- Delivery uses `path`, `duration_seconds`, `width`, `height`, `fps`, and `checksum_sha256`. Every non-Open-Design asset appears in a scene, and every claim is used by a scene.

HTML plates must be self-contained, deterministic, and free of baked narration. Higgsfield narration should be dry and use one approved voice. Creative generated footage must not contain dialogue, captions, product claims, readable UI, logos, or watermarks. Product names, claims, CTA copy, and captions belong in Palmier-native text.

## Recovery rules

- Block unsupported or contradictory claims until an authoritative source is available.
- Replace weak real-demo selections with better source ranges; never synthesize product evidence.
- For missing HTML capture, verify notebook trust, `text/html` output, capture canvas, and port binding, then rerender only the failed plate.
- Tighten and regenerate only narration segments that exceed their scene duration.
- Reject and regenerate only generated shots that drift in style or invent text.
- Treat Higgsfield `explainer_video` unavailability as non-blocking because PalmierPro is the designated assembler.
- Rejoin timed-out Higgsfield jobs and change prompt or parameters after two equivalent failures.
- After a Palmier failure or out-of-band change, reread the active timeline, preserve successful edits, and retry the smallest operation.
- Inspect the export queue and its warnings before retrying a failed export.

## Delivery and verification

Default delivery consists of:

- an H.264/AAC MP4 matching the approved `duration_seconds`, `width`, `height`, and `fps`, with the declared `checksum_sha256`;
- an editable Palmier project;
- the notebook containing the brief, claim register, storyboard, HTML artifacts, approvals, and final preview; and
- a manifest connecting claims to eligible source assets and scenes, and scenes to their owned timeline assets.

Do not assume captions, ProRes, music, alternate ratios, or extra exports unless requested. Verify duration, width, height, frame rate, `checksum_sha256`, H.264/AAC streams, full decode, representative visual frames, CTA spelling, narration intelligibility, and complete claim/scene coverage before completion.

## Skill packaging

Create the skill with the system `skill-creator` initializer and keep each file focused:

- `assets/skills/explainer-video-editor/SKILL.md` contains concise orchestration rules and approval gates.
- `assets/skills/explainer-video-editor/references/handoff-contract.md` contains the claims/assets/scenes manifest schema, scene-owner rules, recovery matrix, and delivery checklist.
- `assets/skills/explainer-video-editor/scripts/validate-delivery.sh` deterministically validates a manifest and exported MP4 using `jq`, `ffprobe`, `ffmpeg`, and `shasum`.
- `assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh` exercises valid and invalid manifests plus a generated test video.
- `assets/skills/explainer-video-editor/agents/openai.yaml` exposes generated display metadata and a default `$explainer-video-editor` prompt.

Cross-reference the existing `open-design` and `html-video` skills by name instead of duplicating their notebook procedures. Use live tool help and model contracts for Higgsfield and PalmierPro details. Do not add a README, creation log, copied API documentation, or unused assets.

## Validation strategy

Use the baseline scenario captured before implementation to identify overproduction and ownership drift. Forward-test the finished skill against:

1. a new product explainer grounded in documentation and a real demo;
2. an enhancement request starting from an existing rough cut; and
3. a pressure case asking the agent to skip approvals, invent UI in generated footage, or use Higgsfield as the final assembler.

Success requires the agent to select the correct route, preserve the three gates, assign every scene to one owner, keep factual product evidence real, use PalmierPro as the final timeline authority, and verify the delivered export.
