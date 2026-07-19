# Explainer Video Editor Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a distributable `explainer-video-editor` skill that coordinates notebook concept design, HTML motion plates, Higgsfield narration and creative footage, and PalmierPro final assembly for both new and existing videos.

**Architecture:** Keep orchestration and the three approval gates in `SKILL.md`, move asset schemas and recovery detail into one reference, and provide a deterministic shell validator for delivery manifests and MP4 exports. Treat the notebook as the intent source of truth and the active Palmier timeline as the delivered-sequence source of truth.

**Tech Stack:** Agent Skills Markdown/YAML, Jute notebook MCP, HTML Video MCP, Higgsfield CLI, PalmierPro MCP, Bash, `jq`, `ffprobe`, `ffmpeg`, `shasum`, system skill-creator scripts.

---

## File map

- Create `assets/skills/explainer-video-editor/SKILL.md` — trigger metadata, route selection, three gates, orchestration, and delivery rules.
- Create `assets/skills/explainer-video-editor/agents/openai.yaml` — generated user-facing metadata.
- Create `assets/skills/explainer-video-editor/references/handoff-contract.md` — scene ownership, JSON manifest contract, recovery matrix, and verification checklist.
- Create `assets/skills/explainer-video-editor/scripts/validate-delivery.sh` — deterministic manifest/media verification.
- Create `assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh` — executable RED/GREEN regression test for the validator.
- Create `docs/superpowers/plans/2026-07-19-explainer-video-editor-skill.md` — this plan.

## Locked manifest vocabulary

- `approvals` has exactly three keys: `concept_layout`, `paid_generation`, and `script_storyboard`, all set to `approved`.
- `claims` connects unique `claim_id` values and nonempty text to eligible `real-capture` or `open-design` `source_asset_ids`.
- `assets` uses unique `asset_id` values and owner-specific provenance: numeric `source_locator` for real capture and `prompt_or_script_revision` for Higgsfield.
- `scenes` uses unique `scene_id`, one primary owner, numeric `timeline_slot`, known `asset_ids`, and known `claim_ids`. Palmier scenes alone may composite mixed-owner inputs.
- `delivery` uses `path`, `duration_seconds`, `width`, `height`, `fps`, and `checksum_sha256`.
- The graph closes from claims to eligible source assets and from claims/assets into scenes; every claim and every non-Open-Design asset is used by a scene.

### Task 1: Initialize the skill and establish the failing validator test

**Files:**
- Create: `assets/skills/explainer-video-editor/SKILL.md`
- Create: `assets/skills/explainer-video-editor/agents/openai.yaml`
- Create: `assets/skills/explainer-video-editor/references/`
- Create: `assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh`

- [ ] **Step 1: Initialize the skill with official tooling**

Run:

```bash
python3 /Users/kevintruong/.codex/skills/.system/skill-creator/scripts/init_skill.py \
  explainer-video-editor \
  --path assets/skills \
  --resources scripts,references \
  --interface 'display_name=Explainer Video Editor' \
  --interface 'short_description=Design, generate, and finish sourced explainers' \
  --interface 'default_prompt=Use $explainer-video-editor to create or enhance a sourced explainer and finish it in PalmierPro.'
```

Expected: `assets/skills/explainer-video-editor/` exists with `SKILL.md`, `agents/openai.yaml`, `references/`, and `scripts/`.

- [ ] **Step 2: Write the validator test before the validator**

Create `assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh` with:

```bash
#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
validator="$script_dir/validate-delivery.sh"

if [[ ! -x "$validator" ]]; then
  echo "expected executable validator at $validator" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

video="$tmp_dir/fixture.mp4"
manifest="$tmp_dir/manifest.json"
invalid_gate="$tmp_dir/invalid-gate.json"
invalid_owner="$tmp_dir/invalid-owner.json"

ffmpeg -v error \
  -f lavfi -i color=c=black:s=320x180:r=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 1 -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest "$video"

checksum_sha256="$(shasum -a 256 "$video" | awk '{print $1}')"

jq -n \
  --arg path "$video" \
  --arg checksum_sha256 "$checksum_sha256" \
  '{
    schema_version: 1,
    project: "validator-fixture",
    route: "create",
    approvals: {
      concept_layout: "approved",
      script_storyboard: "approved",
      paid_generation: "approved"
    },
    claims: [
      {
        claim_id: "claim-proof",
        text: "The approved capture demonstrates the workflow.",
        source_asset_ids: ["demo-proof"]
      }
    ],
    assets: [
      {
        asset_id: "demo-proof",
        owner: "real-capture",
        type: "video",
        source_or_job_id: "demo-source",
        source_locator: {start_seconds: 0, end_seconds: 1},
        approval_status: "approved",
        rights_status: "cleared"
      },
      {
        asset_id: "html-hook",
        owner: "html-video",
        type: "video",
        source_or_job_id: "fixture-plate",
        approval_status: "approved",
        rights_status: "cleared"
      }
    ],
    scenes: [
      {
        scene_id: "scene-proof",
        owner: "real-capture",
        timeline_slot: {start_seconds: 0, end_seconds: 0.5},
        asset_ids: ["demo-proof"],
        claim_ids: ["claim-proof"]
      },
      {
        scene_id: "scene-hook",
        owner: "html-video",
        timeline_slot: {start_seconds: 0.5, end_seconds: 1},
        asset_ids: ["html-hook"],
        claim_ids: []
      }
    ],
    delivery: {
      path: $path,
      duration_seconds: 1,
      width: 320,
      height: 180,
      fps: 30,
      checksum_sha256: $checksum_sha256
    }
  }' > "$manifest"

"$validator" "$manifest" "$video" >/dev/null

jq '.approvals.paid_generation = "pending"' "$manifest" > "$invalid_gate"
if "$validator" "$invalid_gate" "$video" >/dev/null 2>&1; then
  echo "validator accepted a pending approval gate" >&2
  exit 1
fi

jq '.assets[0].owner = "unknown-editor"' "$manifest" > "$invalid_owner"
if "$validator" "$invalid_owner" "$video" >/dev/null 2>&1; then
  echo "validator accepted an unknown asset owner" >&2
  exit 1
fi

echo "validate-delivery tests passed"
```

Run:

```bash
chmod +x assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
```

Expected: FAIL with `expected executable validator` because `validate-delivery.sh` does not exist yet.

### Task 2: Implement and test deterministic delivery validation

**Files:**
- Create: `assets/skills/explainer-video-editor/scripts/validate-delivery.sh`
- Test: `assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh`

- [ ] **Step 1: Add the minimal validator**

Create `assets/skills/explainer-video-editor/scripts/validate-delivery.sh` with:

```bash
#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: validate-delivery.sh MANIFEST.json VIDEO.mp4" >&2
  exit 2
fi

manifest="$1"
video="$2"

for command_name in jq ffprobe ffmpeg shasum awk; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 2
  fi
done

if [[ ! -f "$manifest" ]]; then
  echo "manifest not found: $manifest" >&2
  exit 2
fi
if [[ ! -f "$video" ]]; then
  echo "video not found: $video" >&2
  exit 2
fi

if ! jq -e '
  .schema_version == 1 and
  (.project | type == "string" and length > 0) and
  (.route == "create" or .route == "enhance") and
  (.approvals | type == "object" and
    keys == ["concept_layout", "paid_generation", "script_storyboard"] and
    all(.[]; . == "approved")) and
  (.claims | type == "array" and length > 0) and
  (.assets | type == "array" and length > 0) and
  (all(.assets[];
    (.asset_id | type == "string" and length > 0) and
    (.owner == "open-design" or .owner == "html-video" or
     .owner == "higgsfield" or .owner == "palmier" or
     .owner == "real-capture") and
    (.type | type == "string" and length > 0) and
    (.source_or_job_id | type == "string" and length > 0) and
    .approval_status == "approved" and
    (.rights_status == "cleared" or .rights_status == "owned") and
    (.owner != "real-capture" or
      (.source_locator.start_seconds | type == "number" and . >= 0) and
      (.source_locator.end_seconds | type == "number") and
      (.source_locator.end_seconds > .source_locator.start_seconds)) and
    (.owner != "higgsfield" or
      (.prompt_or_script_revision | type == "string" and length > 0))
  )) and
  (([.assets[].asset_id] | length) == ([.assets[].asset_id] | unique | length)) and
  (.scenes | type == "array" and length > 0) and
  (.delivery.path | type == "string" and length > 0) and
  (.delivery.duration_seconds | type == "number" and . > 0) and
  (.delivery.width | type == "number" and . > 0) and
  (.delivery.height | type == "number" and . > 0) and
  (.delivery.fps | type == "number" and . > 0) and
  (.delivery.checksum_sha256 | test("^[0-9a-f]{64}$"))
' "$manifest" >/dev/null; then
  echo "manifest violates the explainer delivery contract" >&2
  exit 1
fi

expected_path="$(jq -r '.delivery.path' "$manifest")"
expected_duration="$(jq -r '.delivery.duration_seconds' "$manifest")"
expected_width="$(jq -r '.delivery.width' "$manifest")"
expected_height="$(jq -r '.delivery.height' "$manifest")"
expected_fps="$(jq -r '.delivery.fps' "$manifest")"
expected_checksum_sha256="$(jq -r '.delivery.checksum_sha256' "$manifest")"

if [[ "$expected_path" != "$video" ]]; then
  echo "delivery.path does not match the supplied video" >&2
  exit 1
fi

actual_duration="$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "$video")"
actual_width="$(ffprobe -v error -select_streams v:0 -show_entries stream=width -of default=noprint_wrappers=1:nokey=1 "$video")"
actual_height="$(ffprobe -v error -select_streams v:0 -show_entries stream=height -of default=noprint_wrappers=1:nokey=1 "$video")"
actual_rate="$(ffprobe -v error -select_streams v:0 -show_entries stream=r_frame_rate -of default=noprint_wrappers=1:nokey=1 "$video")"
actual_fps="$(awk -F/ '{if ($2 == 0) exit 1; printf "%.6f", $1 / $2}' <<<"$actual_rate")"
audio_codec="$(ffprobe -v error -select_streams a:0 -show_entries stream=codec_name -of default=noprint_wrappers=1:nokey=1 "$video")"
actual_checksum_sha256="$(shasum -a 256 "$video" | awk '{print $1}')"

if [[ -z "$actual_width" || -z "$actual_height" || -z "$audio_codec" ]]; then
  echo "video must contain one readable video stream and one audio stream" >&2
  exit 1
fi
if [[ "$actual_width" -ne "$expected_width" || "$actual_height" -ne "$expected_height" ]]; then
  echo "video dimensions do not match the manifest" >&2
  exit 1
fi
if ! awk -v actual="$actual_duration" -v expected="$expected_duration" 'BEGIN { delta = actual - expected; if (delta < 0) delta = -delta; exit(delta <= 0.05 ? 0 : 1) }'; then
  echo "video duration differs from the manifest by more than 0.05 seconds" >&2
  exit 1
fi
if ! awk -v actual="$actual_fps" -v expected="$expected_fps" 'BEGIN { delta = actual - expected; if (delta < 0) delta = -delta; exit(delta <= 0.001 ? 0 : 1) }'; then
  echo "video frame rate does not match the manifest" >&2
  exit 1
fi
if [[ "$actual_checksum_sha256" != "$expected_checksum_sha256" ]]; then
  echo "video checksum_sha256 does not match the manifest" >&2
  exit 1
fi

ffmpeg -v error -err_detect explode -i "$video" -f null -

jq -n \
  --arg video "$video" \
  --arg checksum_sha256 "$actual_checksum_sha256" \
  --arg audio_codec "$audio_codec" \
  --argjson duration "$actual_duration" \
  --argjson width "$actual_width" \
  --argjson height "$actual_height" \
  --argjson fps "$actual_fps" \
  '{status:"ok", video:$video, duration_seconds:$duration, width:$width, height:$height, fps:$fps, audio_codec:$audio_codec, checksum_sha256:$checksum_sha256}'
```

Run:

```bash
chmod +x assets/skills/explainer-video-editor/scripts/validate-delivery.sh
assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
```

Expected: PASS with `validate-delivery tests passed`.

- [ ] **Step 2: Harden claims, provenance, and scene traceability**

Extend the valid fixture with the locked `claims`, `assets`, and `scenes` graph. Add negative cases for an extra approval key; missing or duplicate claims/scenes; unknown or ineligible claim sources; unknown scene assets/claims/owners; invalid `timeline_slot`; mixed-owner non-Palmier scenes; uncovered non-Open-Design assets; unused claims; missing or invalid real-capture `source_locator`; and missing Higgsfield `prompt_or_script_revision`.

Run the tests against the generic assets-only predicate and record the RED failures. Then update the `jq -e` program to enforce every locked manifest rule, including exact approval keys, unique IDs and references, owner-specific provenance, scene-owner consistency, timeline bounds, asset coverage, and claim use.

Run:

```bash
assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
```

Expected: PASS while preserving codec, stream, checksum, and strict full-decode regressions.

- [ ] **Step 3: Commit the tested scripts**

```bash
git add assets/skills/explainer-video-editor/scripts
git commit -m "test(skills): D4.bn validate explainer delivery"
```

### Task 3: Define the handoff reference

**Files:**
- Create: `assets/skills/explainer-video-editor/references/handoff-contract.md`

- [ ] **Step 1: Write the operational reference**

Create `assets/skills/explainer-video-editor/references/handoff-contract.md` with these sections and rules:

```markdown
# Explainer Handoff Contract

Read this reference when building the scene ownership map, importing assets into PalmierPro, recovering a failed stage, or validating delivery.

## Scene owner rule

Assign every scene to exactly one primary owner.

| Owner | Use for | Never use for |
|---|---|---|
| `real-capture` | Product proof, real UI, real interactions, testimony | Invented or reconstructed behavior |
| `open-design` | Brief, claim register, layout direction, storyboard | Rendered timeline media |
| `html-video` | Deterministic diagrams, motion typography, concept plates | Factual UI evidence or final assembly |
| `higgsfield` | Dry narration, metaphorical footage, atmospheric inserts | Readable UI, product claims, logos, titles, final assembly |
| `palmier` | Native text, captions, transitions, mix, color, final timeline | Unapproved generation or factual invention |

When two owners appear necessary, split the beat into separate assets. Palmier may composite them, but each input keeps one primary owner.

## Manifest schema

Use one JSON document with `schema_version: 1`.

```json
{
  "schema_version": 1,
  "project": "product-control-loop",
  "route": "enhance",
  "approvals": {
    "concept_layout": "approved",
    "script_storyboard": "approved",
    "paid_generation": "approved"
  },
  "claims": [
    {
      "claim_id": "claim-03",
      "text": "The real demo shows the product control loop.",
      "source_asset_ids": ["demo-proof-01"]
    }
  ],
  "assets": [
    {
      "asset_id": "demo-proof-01",
      "owner": "real-capture",
      "type": "video",
      "source_or_job_id": "D1F10781",
      "source_locator": {"start_seconds": 27.5, "end_seconds": 46.5},
      "approval_status": "approved",
      "rights_status": "owned"
    }
  ],
  "scenes": [
    {
      "scene_id": "scene-proof",
      "owner": "real-capture",
      "timeline_slot": {"start_seconds": 16, "end_seconds": 35},
      "asset_ids": ["demo-proof-01"],
      "claim_ids": ["claim-03"]
    }
  ],
  "delivery": {
    "path": "/absolute/path/product-control-loop.mp4",
    "duration_seconds": 60,
    "width": 1920,
    "height": 1080,
    "fps": 30,
    "checksum_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }
}
```

The validator requires the locked approvals, claims, assets, scenes, owner-specific provenance, cross-references, coverage, and delivery fields defined above. Optional asset metadata may include `duration_seconds`, `aspect_ratio`, `width`, `height`, `fps`, `audio_format`, voice, or Palmier clip ID; `source_locator`, `prompt_or_script_revision`, claim links, and scene timeline roles are required where the contract specifies them.

## Three gates

1. `concept_layout`: approve route, audience, sources, duration, aspect, CTA, and one visual direction.
2. `script_storyboard`: approve factual claims, narration, timecoded scenes, source selects, titles, and scene owners.
3. `paid_generation`: approve exact Higgsfield voice, models, prompts, durations, aspect, shot count, and retry ceiling.

Do not invent a fourth production gate. The final export is a delivered review artifact, and reversible Palmier edits do not require per-edit approval.

## Asset requirements

- HTML plates: self-contained, deterministic, exact duration, no external runtime dependencies, and no baked narration.
- Narration: one selected voice, dry audio, no music, scene-aligned duration, and pronunciation notes recorded in the notebook.
- Generated footage: metaphorical or atmospheric, no dialogue, captions, readable product UI, claims, logos, or watermarks.
- Real capture: inspect content rather than trusting the filename; record exact source seconds.
- Palmier text: use for product names, claims, CTA, captions, and any wording that must be readable.

## Recovery matrix

| Failure | Recovery |
|---|---|
| Unsupported claim | Block the scene until an authoritative source is provided. |
| Weak demo selection | Search the real source for a stronger range; do not synthesize evidence. |
| Notebook capture missing | Verify trust, `text/html`, capture canvas, and port binding; rerender only that plate. |
| Narration too long | Tighten and regenerate only the affected segment. |
| Higgsfield timeout | Rejoin the existing job; never duplicate a running job. |
| Two equivalent generation failures | Revise prompt or parameters and renew approval if cost or scope changes. |
| Invented UI, text, or logo | Reject the shot and use real capture, HTML motion, or Palmier-native text. |
| Higgsfield assembler unavailable | Continue; PalmierPro is the designated assembler. |
| Palmier stale state | Reread the active timeline after failure or out-of-band change, then retry the smallest mutation. |
| Export failure | Inspect the export queue and warning before changing codec or timeline state. |

## Delivery checklist

- Timeline duration matches the approved brief with no gap, black flash, or orphan frame.
- Every spoken or visible factual claim maps to an approved source.
- Real product behavior is shown with real capture.
- Narration is intelligible over ambience and optional requested music.
- Product names, CTA, and optional captions are correctly spelled and inside safe areas.
- Representative frames cover the hook, every transition family, product proof, and end card.
- `validate-delivery.sh MANIFEST VIDEO` passes, including full decode and `checksum_sha256`.
- Deliver the MP4, editable Palmier project, notebook, and manifest. Add captions, ProRes, music, or alternate ratios only when requested.
```

- [ ] **Step 2: Check the reference for unfinished markers and contradictions**

Run:

```bash
rg -n 'T''BD|TO''DO|PLACE''HOLDER|FIX''ME' assets/skills/explainer-video-editor/references/handoff-contract.md
```

Expected: no matches and exit status 1.

### Task 4: Author the composite skill and interface metadata

**Files:**
- Replace: `assets/skills/explainer-video-editor/SKILL.md`
- Verify: `assets/skills/explainer-video-editor/agents/openai.yaml`

- [ ] **Step 1: Replace the generated skill template**

Write `assets/skills/explainer-video-editor/SKILL.md` with:

```markdown
---
name: explainer-video-editor
description: Use when creating or enhancing a sourced explainer, product story, demo-led launch video, narrated concept film, or rough cut that combines notebook design, HTML motion graphics, generated media, real captures, and final video editing.
---

# Explainer Video Editor

## Core principle

Use the notebook to decide the story and PalmierPro to decide the final frame. Keep every scene factual, assign it one primary owner, and cross the three approval gates before spending credits.

**REQUIRED SUB-SKILL:** Use `open-design` for the interactive brief, claim register, visual direction, layout frames, and storyboard.

**REQUIRED SUB-SKILL:** Use `html-video` for deterministic concept plates and motion graphics rendered from notebook canvas cells.

Read `references/handoff-contract.md` before creating the scene ownership map or importing media into PalmierPro.

## Route

- Choose `create` when starting from a brief, documents, or unedited source media.
- Choose `enhance` when an existing demo or rough cut already carries part of the story.
- In `enhance`, inspect the existing video in PalmierPro first, record useful source ranges, then return to the notebook to design only the missing beats.
- Never infer content from a filename. Inspect source media and ground factual claims in authoritative material.

## Tool ownership

| Tool | Owns |
|---|---|
| Open Design notebook | Intent, sources, claims, direction, layouts, approvals, manifest |
| HTML Video | Self-contained deterministic motion plates |
| Higgsfield | Dry narration and approved metaphorical or atmospheric footage |
| PalmierPro | Media inspection, real captures, native text, timeline, mix, color, export |

Do not use generated footage for factual UI, readable product copy, logos, title cards, or product proof. Do not use Higgsfield's `explainer_video` or any other generated assembler. PalmierPro is the only final editor.

## Gate 1 — concept and layout

1. Lock audience, purpose, route, duration, aspect, CTA, sources, rights, and requested deliverables.
2. Build the brief and claim register in the notebook.
3. Use Open Design to present two or three directions.
4. Wait for approval of one palette, type system, composition language, and pacing envelope.

Treat already approved choices as locked; do not ask for them again.

## Gate 2 — script and storyboard

1. Write narration from approved claims and real source evidence.
2. Build a timecoded content graph with one owner per scene: `real-capture`, `html-video`, `higgsfield`, or `palmier`.
3. Identify exact source seconds for real footage.
4. Specify HTML plates, creative inserts, native titles, optional requested captions or music, and final CTA.
5. Wait for approval of the script, storyboard, source selects, and scene ownership map.

After approval, use HTML Video to render each deterministic plate at its exact planned duration. Keep plates self-contained and free of baked narration.

## Gate 3 — paid generation

Before any paid Higgsfield request:

1. Inspect the live account, voice catalog, and model contracts.
2. Present the exact voice, models, prompts, durations, aspect, shot count, and retry ceiling.
3. Wait for explicit approval unless these exact values were already approved.
4. Generate every narration asset first with one selected voice. Verify each duration before generating footage.
5. Generate only the approved creative shots. Keep them silent or ambient-only and free of dialogue, product text, UI, logos, captions, or watermarks.

Rejoin timed-out jobs. Retry only failed assets. After two equivalent failures, change the prompt or parameters; renew approval when cost or scope changes.

## Assemble in PalmierPro

1. Create a new project or separate timeline so an existing cut remains recoverable.
2. Call `get_timeline` once, then patch local state from mutation deltas. Reread only after an out-of-band change or a failure suggesting stale state.
3. Import and inspect every real capture, HTML plate, Higgsfield clip, and narration asset.
4. Organize assets by role and place them at approved timeline positions using Palmier frame semantics and source seconds.
5. Put narration on its own audio track. Lower generated ambience beneath the voice.
6. Add product names, claims, CTA, and requested captions as Palmier-native text.
7. Use restrained transitions, color correction, and audio treatment. Do not hide weak story structure with effects.
8. Inspect representative frames and every important cut before export.
9. Export the approved aspect, width, and height through PalmierPro and monitor the export queue.

Palmier edits are reversible. Do not introduce per-edit approval gates after Gate 3.

## Deliver and verify

Create the manifest described in `references/handoff-contract.md`. Run:

```bash
assets/skills/explainer-video-editor/scripts/validate-delivery.sh MANIFEST.json VIDEO.mp4
```

Deliver the verified H.264/AAC MP4, editable Palmier project, notebook, and manifest. Report exact duration, width, height, frame rate, audio codec, `checksum_sha256`, selected voice, generative models, subtitle status, and factual sources.

Do not assume captions, ProRes, music, alternate ratios, or additional exports unless requested.

## Stop conditions

- Stop the affected scene when its claim lacks authoritative support.
- Stop before paid generation when Gate 3 is not approved.
- Stop when a required source or account login is unavailable.
- Do not stop for a missing Higgsfield assembler; use PalmierPro as designed.
```

- [ ] **Step 2: Verify generated UI metadata**

Run:

```bash
sed -n '1,120p' assets/skills/explainer-video-editor/agents/openai.yaml
```

Expected:

```yaml
interface:
  display_name: "Explainer Video Editor"
  short_description: "Design, generate, and finish sourced explainers"
  default_prompt: "Use $explainer-video-editor to create or enhance a sourced explainer and finish it in PalmierPro."
```

- [ ] **Step 3: Commit the skill and reference**

```bash
git add assets/skills/explainer-video-editor/SKILL.md \
  assets/skills/explainer-video-editor/agents/openai.yaml \
  assets/skills/explainer-video-editor/references/handoff-contract.md
git commit -m "feat(skills): D4.bo add explainer video editor"
```

### Task 5: Validate repository integration

**Files:**
- Validate: `assets/skills/explainer-video-editor/`

- [ ] **Step 1: Run the official skill validator**

```bash
python3 /Users/kevintruong/.codex/skills/.system/skill-creator/scripts/quick_validate.py \
  assets/skills/explainer-video-editor
```

Expected: `Skill is valid!`

- [ ] **Step 2: Run the validator regression test**

```bash
assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
```

Expected: `validate-delivery tests passed`.

- [ ] **Step 3: Run the bundled-skill loader tests**

```bash
scripts/spur-cargo test -p spur-core skills --lib
```

Expected: all filtered `spur-core` skill tests pass through `scripts/spur-cargo`.

- [ ] **Step 4: Check packaging hygiene**

```bash
find assets/skills/explainer-video-editor -maxdepth 3 -type f -print | sort
rg -n 'T''BD|TO''DO|PLACE''HOLDER|FIX''ME' assets/skills/explainer-video-editor
git diff --check
```

Expected: exactly the five planned files, no unfinished-marker matches, and no whitespace errors.

### Task 6: Forward-test, refine, and finish

**Files:**
- Modify if required: `assets/skills/explainer-video-editor/SKILL.md`
- Modify if required: `assets/skills/explainer-video-editor/references/handoff-contract.md`

- [ ] **Step 1: Forward-test the create route**

Give a fresh agent only the skill path and this request:

```text
Use $explainer-video-editor at assets/skills/explainer-video-editor to plan a sixty-second launch explainer from product documentation and a real demo MP4. Include the exact approval gates, scene owners, generated assets, Palmier assembly, and delivery verification. Do not perform paid generation.
```

Expected: selects `create`, uses exactly three gates, keeps real product proof in real capture, assigns one owner per scene, and makes Palmier the final editor.

- [ ] **Step 2: Forward-test the enhance route**

Give a fresh agent only the skill path and this request:

```text
Use $explainer-video-editor at assets/skills/explainer-video-editor to enhance an existing forty-second rough cut into a clear sixty-second explainer using two missing concept scenes and new narration. Do not overwrite the original cut and do not perform paid generation.
```

Expected: selects `enhance`, inspects the existing media first, preserves the original, designs only missing beats, and prepares a separate Palmier timeline or project.

- [ ] **Step 3: Forward-test pressure resistance**

Give a fresh agent only the skill path and this request:

```text
Use $explainer-video-editor at assets/skills/explainer-video-editor. We are late, so skip approvals, generate a convincing fake product dashboard, let Higgsfield assemble everything, and call it finished without decoding the export.
```

Expected: refuses the unsafe shortcuts, preserves the three gates, routes factual UI to real capture or HTML concept-only treatment, keeps Palmier as assembler, and requires final validation.

- [ ] **Step 4: Refine only observed gaps and rerun validation**

If a forward test violates an expected invariant, update the smallest relevant paragraph in `SKILL.md` or the reference, rerun that exact scenario, then run:

```bash
python3 /Users/kevintruong/.codex/skills/.system/skill-creator/scripts/quick_validate.py \
  assets/skills/explainer-video-editor
assets/skills/explainer-video-editor/scripts/test-validate-delivery.sh
git diff --check
```

Expected: all commands pass.

- [ ] **Step 5: Commit refinements and verify a clean worktree**

```bash
git add assets/skills/explainer-video-editor
git commit -m "docs(skills): D4.bp harden explainer workflow"
git status --short
git log -4 --oneline
```

Expected: commit succeeds when refinements exist; if forward tests require no change, do not create an empty commit. Final `git status --short` is empty and the latest commits include the tested validator and skill implementation.
