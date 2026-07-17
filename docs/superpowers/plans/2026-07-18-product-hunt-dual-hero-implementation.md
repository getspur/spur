# Product Hunt Dual Hero Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce truthful 45-second and 90-second SPUR Product Hunt hero videos from the five checksum-reviewed TUI recordings, with Higgsfield narration/music, notebook-authored review overlays, and Palmier Pro editing/rendering.

**Architecture:** Keep the approved TUI recordings as the only product-picture evidence. Generate audio only through Higgsfield, generate transparent progress-rail plates and their interactive review surface in the existing notebook, then assemble two new frame-exact Palmier timelines. Preserve the 23.2-second Palmier baseline and the proof manifest; the two exports remain review candidates.

**Tech Stack:** Jute Notebook MCP, Python/Pillow/HTML, Higgsfield CLI (`inworld_text_to_speech`, `sonilo_music`), Palmier Pro MCP, FFmpeg/FFprobe, jq, SHA-256.

---

## Locked inputs and truth boundary

- Design spec: `docs/superpowers/specs/2026-07-17-product-hunt-kinetic-hero-design.md`
- Review notebook: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Palmier project: `/Users/kevintruong/Documents/Palmier Pro/SPUR Product Hunt Hero - Real TUI.palmier`
- Existing baseline: `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro.mp4`
- New outputs:
  - `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-45s.mp4`
  - `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-90s.mp4`
- Use only these reviewed Palmier media assets for moving product picture:

| Media ref | Reviewed asset | Duration | Approved meaning |
|---|---|---:|---|
| `791B452C` | Session Detail operator home | 52.48 s | One durable operator context |
| `14D82963` | Worker state visibility | 44.64 s | Running/cancelled state remains visible |
| `82D9D60A` | Explicit plan state | 22.72 s | Empty plan inventory is explicit |
| `63605F31` | Specialist routing | 59.16 s | Agent/model/effort are explicit |
| `4B29113A` | Session resume | 9.20 s | Saved context returns to the operator surface |
| `4387A028` | SPUR ink matte | still | Existing authored background |

- The approved still `docs/product_launch/media_pack/ph_ready/gallery-05-session-resume-1270x760.png` may be used only for the final 0.8-second hold after the complete resume recording.
- Do not use `scripts/e2e/demos/tui-live/out/15-live-hitl-agent-loop*`; that D4 attempt is diagnostic and non-promotable.
- Do not generate UI, title cards, logos, product footage, or motion graphics with image/video models.
- The 90-second truth card must say `PROVEN NOW` and `NOT PROVEN YET`; it must not imply that continuous Reject → Retry → Approve ownership is proven.
- Do not edit `docs/product_launch/media_pack/proof-manifest.json`, replace the baseline file, or replace its Palmier timeline.

## Locked visual and audio settings

- Canvas: 1920×1080, 30 fps.
- Palette: ink `#0B0E14`, ivory `#E6E1CF`, cyan `#7FB4CA`, violet `#957FB8`, border `#2A2E38`.
- Typography: SF Mono, uppercase cues, direct cuts, no ornamental transitions.
- Captions: ivory SF Mono, 34 px, dark 82%-opaque background, centered at `centerY=0.84`, maximum seven words, no animation.
- Proof cues: upper-left at `centerX=0.25`, `centerY=0.14`, `width=0.42`, `height=0.16`; 42 px bold ivory, with the cyan progress rail carrying the signal color.
- Progress rail: x=200…1720, y=982…988, six pixels high; border track with cyan fill at 20/40/60/80/100%.
- Product-picture grade: `reset=true`, `contrast=1.04`, `saturation=0.92`, `shadows=0.03`, `highlights=-0.03`, `temperature=6400`.
- Motion: establish the frame for 30 frames, then push from normalized scale 1.00×1.00 to at most 1.08×1.08. Use smooth interpolation and paired position keyframes so evidence stays visible.
- Narration target: -16 LUFS integrated, -1.5 dBTP ceiling.
- Music target before Palmier mixing: -24 LUFS integrated, -2 dBTP ceiling. Palmier music volume stays at or below 0.14 and ducks further under the opening, truth boundary, and CTA.

### Exact narration copy — 45 seconds

> Running more coding agents is easy. Keeping their work visible, isolated, and recoverable is the hard part. SPUR gives Claude Code, Codex, Kiro, and Gemini one durable outer harness. Start from Session Detail. See every worker and its current state. Inspect what is planned without pretending an empty state is progress. Route each task to the right agent, model, and effort. Then resume the same conversation without losing context. SPUR keeps the operator in view while the agents do the work. Install Community free.

### Exact narration copy — 90 seconds

> Running multiple coding agents can make more work disappear into more terminals. The difficult part is not asking an agent to write code. It is knowing what is running, where the result lives, and whether you can recover the context when something stops. SPUR is a common outer harness for Claude Code, Codex, Kiro, and Gemini. Session Detail gives the operator one durable place to ask, steer, and inspect. Worker state remains visible while tasks run in isolated worktrees. Plans show what exists and what does not; an empty state stays labeled as an empty state. Routing is explicit, so the operator chooses the agent, model, and effort instead of hiding those decisions behind automation. Saved conversations can resume without discarding the working context. The current product proves session, worker, routing, and resume visibility. Continuous human ownership of Reject, Retry, and Approve is still a product direction, not approved launch proof. SPUR keeps agent work visible, isolated, recoverable, and reviewable from one session. Install Community free.

### Exact music prompt

> Instrumental minimal electronic score for a precise developer tool demo; calm forward pulse, warm analog texture, restrained percussion, no vocals, no dramatic drops, clear space for narration, confident quiet ending.

## Task 1: Hydrate reviewed evidence, protect generated outputs, and prove the input state

**Files:**

- Modify: `docs/product_launch/media_pack/.gitignore`

- [ ] **Step 1: hydrate the worktree's ignored real-evidence files without overwriting any existing copy**

The approved source MP4s and deterministic hero intermediates are ignored, so git did not copy them into the isolated worktree. Copy only the checksum-bound files from the primary checkout:

```bash
SOURCE_PACK="/Volumes/Projects/spur/docs/product_launch/media_pack"
WORKTREE_PACK="$PWD/docs/product_launch/media_pack"
mkdir -p "$WORKTREE_PACK/live_demos" "$WORKTREE_PACK/demo_render/out"
cp -n -p "$SOURCE_PACK/live_demos/04-session-resume.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/live_demos/09-product-e2e-flow.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/live_demos/10-problem-ops-visibility.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/live_demos/11-problem-plan-progress.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/live_demos/12-problem-backlog-triage.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/live_demos/13-problem-plan-loop-drive.mp4" "$WORKTREE_PACK/live_demos/"
cp -n -p "$SOURCE_PACK/demo_render/out/seg-session.mp4" "$WORKTREE_PACK/demo_render/out/"
cp -n -p "$SOURCE_PACK/demo_render/out/seg-workers.mp4" "$WORKTREE_PACK/demo_render/out/"
cp -n -p "$SOURCE_PACK/demo_render/out/seg-plans.mp4" "$WORKTREE_PACK/demo_render/out/"
cp -n -p "$SOURCE_PACK/demo_render/out/seg-specialist.mp4" "$WORKTREE_PACK/demo_render/out/"
cp -n -p "$SOURCE_PACK/demo_render/out/seg-resume.mp4" "$WORKTREE_PACK/demo_render/out/"
cp -n -p "$SOURCE_PACK/ph_ready/hero-video-ph-ready.mp4" "$WORKTREE_PACK/ph_ready/"
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

The media contract must pass before generation. A checksum failure is a hard stop; never refresh the manifest to accept drift.

- [ ] **Step 2: verify the worktree and Palmier baseline before editing**

```bash
git status --short
ffprobe -v error \
  -show_entries format=duration,size:stream=codec_name,codec_type,width,height,pix_fmt,r_frame_rate \
  -of json docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro.mp4
shasum -a 256 docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro.mp4
test ! -e docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-45s.mp4
test ! -e docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-90s.mp4
```

Expected baseline SHA-256: `06ff6d2e772d36ac7fd54209d24456415cc3af8a071501da418f00ef58e12a36`. Stop if it differs. If either new output path already exists, stop and ask whether to preserve or replace it; do not infer overwrite permission.

- [ ] **Step 3: add these exact ignore entries with `apply_patch`**

```gitignore
ph_ready/audio/
ph_ready/overlays/
ph_ready/hero-video-palmier-pro-45s.mp4
ph_ready/hero-video-palmier-pro-90s.mp4
```

- [ ] **Step 4: verify only the ignore file changed**

```bash
git diff --check
git diff -- docs/product_launch/media_pack/.gitignore
```

- [ ] **Step 5: commit**

```bash
git add docs/product_launch/media_pack/.gitignore
git commit -m "chore(product-launch): D4.m ignore dual hero outputs"
```

## Task 2: Generate and normalize the approved Higgsfield audio

**Generated files (ignored):**

- `docs/product_launch/media_pack/ph_ready/audio/narration-45s.wav`
- `docs/product_launch/media_pack/ph_ready/audio/narration-90s.wav`
- `docs/product_launch/media_pack/ph_ready/audio/music-90s.wav`
- `docs/product_launch/media_pack/ph_ready/audio/music-45s.wav`

- [ ] **Step 1: confirm authentication and live schemas without starting jobs**

```bash
higgsfield auth status
higgsfield model get inworld_text_to_speech --json
higgsfield model get sonilo_music --json
```

Expected: `Simon (en)` is present; Inworld requires `prompt` and `voice`; Sonilo requires `duration` and `prompt`.

- [ ] **Step 2: declare the exact approved strings and output directory**

```bash
PACK_ROOT="$PWD/docs/product_launch/media_pack"
AUDIO_ROOT="$PACK_ROOT/ph_ready/audio"
HERO_45_SCRIPT='Running more coding agents is easy. Keeping their work visible, isolated, and recoverable is the hard part. SPUR gives Claude Code, Codex, Kiro, and Gemini one durable outer harness. Start from Session Detail. See every worker and its current state. Inspect what is planned without pretending an empty state is progress. Route each task to the right agent, model, and effort. Then resume the same conversation without losing context. SPUR keeps the operator in view while the agents do the work. Install Community free.'
HERO_90_SCRIPT='Running multiple coding agents can make more work disappear into more terminals. The difficult part is not asking an agent to write code. It is knowing what is running, where the result lives, and whether you can recover the context when something stops. SPUR is a common outer harness for Claude Code, Codex, Kiro, and Gemini. Session Detail gives the operator one durable place to ask, steer, and inspect. Worker state remains visible while tasks run in isolated worktrees. Plans show what exists and what does not; an empty state stays labeled as an empty state. Routing is explicit, so the operator chooses the agent, model, and effort instead of hiding those decisions behind automation. Saved conversations can resume without discarding the working context. The current product proves session, worker, routing, and resume visibility. Continuous human ownership of Reject, Retry, and Approve is still a product direction, not approved launch proof. SPUR keeps agent work visible, isolated, recoverable, and reviewable from one session. Install Community free.'
HERO_MUSIC_PROMPT='Instrumental minimal electronic score for a precise developer tool demo; calm forward pulse, warm analog texture, restrained percussion, no vocals, no dramatic drops, clear space for narration, confident quiet ending.'
mkdir -p "$AUDIO_ROOT"
```

- [ ] **Step 3: run the three approved paid audio jobs synchronously**

```bash
higgsfield generate create inworld_text_to_speech \
  --prompt "$HERO_45_SCRIPT" --voice 'Simon (en)' \
  --wait --json > "$AUDIO_ROOT/narration-45s.job.json"
higgsfield generate create inworld_text_to_speech \
  --prompt "$HERO_90_SCRIPT" --voice 'Simon (en)' \
  --wait --json > "$AUDIO_ROOT/narration-90s.job.json"
higgsfield generate create sonilo_music \
  --duration 90 --prompt "$HERO_MUSIC_PROMPT" \
  --wait --json > "$AUDIO_ROOT/music-90s.job.json"
```

Each JSON file must contain a terminal successful result and `.[0].result_url`. Do not print raw job JSON into the user-facing handoff.

- [ ] **Step 4: download the immutable results and normalize them to stereo 48 kHz WAV**

```bash
for asset in narration-45s narration-90s music-90s; do
  result_url="$(jq -er '.[0].result_url' "$AUDIO_ROOT/$asset.job.json")"
  curl --fail --location "$result_url" --output "$AUDIO_ROOT/$asset.source"
done
ffmpeg -nostdin -y -v error -i "$AUDIO_ROOT/narration-45s.source" \
  -af 'loudnorm=I=-16:TP=-1.5:LRA=7' -ar 48000 -ac 2 "$AUDIO_ROOT/narration-45s.wav"
ffmpeg -nostdin -y -v error -i "$AUDIO_ROOT/narration-90s.source" \
  -af 'loudnorm=I=-16:TP=-1.5:LRA=7' -ar 48000 -ac 2 "$AUDIO_ROOT/narration-90s.wav"
ffmpeg -nostdin -y -v error -i "$AUDIO_ROOT/music-90s.source" \
  -af 'loudnorm=I=-24:TP=-2:LRA=11,apad=whole_dur=90,atrim=0:90' \
  -ar 48000 -ac 2 "$AUDIO_ROOT/music-90s.wav"
```

- [ ] **Step 5: create a clean 45-second edit from the same music identity**

Use the opening 42 seconds and the final four seconds of the 90-second track with a one-second equal-gain crossfade. This yields exactly 45 seconds and preserves the confident ending.

```bash
ffmpeg -nostdin -y -v error -i "$AUDIO_ROOT/music-90s.wav" \
  -filter_complex '[0:a]atrim=0:42,asetpts=PTS-STARTPTS[open];[0:a]atrim=86:90,asetpts=PTS-STARTPTS[close];[open][close]acrossfade=d=1:c1=tri:c2=tri[out]' \
  -map '[out]' -ar 48000 -ac 2 "$AUDIO_ROOT/music-45s.wav"
```

- [ ] **Step 6: enforce the timing gate before opening Palmier**

```bash
for asset in narration-45s narration-90s music-45s music-90s; do
  ffprobe -v error -show_entries format=filename,duration,size \
    -show_entries stream=codec_name,sample_rate,channels \
    -of json "$AUDIO_ROOT/$asset.wav"
done
```

Gate:

- `narration-45s.wav` must be no longer than 43.0 seconds.
- `narration-90s.wav` must end between 84.0 and 88.0 seconds.
- `music-45s.wav` must be exactly 45.0 seconds; `music-90s.wav` must be exactly 90.0 seconds.
- If either narration misses its window, stop and present its measured duration plus a copy-revision proposal for approval. Do not silently change words, splice in artificial silence, or use `atempo` beyond ±5%.

No git commit: all files in this task are generated and ignored.

## Task 3: Generate progress-rail plates and visualize them through Notebook MCP

**Files:**

- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Generate (ignored): `docs/product_launch/media_pack/ph_ready/overlays/progress-1.png` through `progress-5.png`

- [ ] **Step 1: reload the notebook from disk before editing**

Use Notebook MCP in this order:

1. `notebook_open` with the absolute notebook path.
2. `notebook_reload` so the on-disk `.ipynb` is authoritative.
3. `notebook_get_notebook` and verify the existing approved-audio cell is present.

If the Notebook MCP transport is closed, reconnect through the live SpurLab notebook socket, reopen the same notebook, and repeat the reload. Do not edit the `.ipynb` as raw JSON.

- [ ] **Step 2: insert one Python cell after the approved audio-system cell**

Use `notebook_insert_cell` with this complete source:

```python
from pathlib import Path
import base64
from PIL import Image, ImageDraw
from IPython.display import HTML, display

pack_root = Path("/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack")
overlay_root = pack_root / "ph_ready" / "overlays"
overlay_root.mkdir(parents=True, exist_ok=True)

width, height = 1920, 1080
x0, x1, y0, y1 = 200, 1720, 982, 988
track_color = "#2A2E38"
fill_color = "#7FB4CA"

paths = []
for stage in range(1, 6):
    image = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((x0, y0, x1, y1), radius=3, fill=track_color)
    fill_x = x0 + round((x1 - x0) * stage / 5)
    draw.rounded_rectangle((x0, y0, fill_x, y1), radius=3, fill=fill_color)
    path = overlay_root / f"progress-{stage}.png"
    image.save(path)
    paths.append(path)

def png_data_url(path: Path) -> str:
    encoded = base64.b64encode(path.read_bytes()).decode("ascii")
    return f"data:image/png;base64,{encoded}"

controls = "".join(
    f'<input type="radio" name="spur-rail" id="spur-rail-{i}" {"checked" if i == 1 else ""}>'
    f'<label for="spur-rail-{i}">{i}/5</label>'
    for i in range(1, 6)
)
plates = "".join(
    f'<div class="spur-rail-plate spur-rail-plate-{i}"><img src="{png_data_url(path)}" alt="SPUR progress rail {i} of 5"></div>'
    for i, path in enumerate(paths, 1)
)

display(HTML(f"""
<style>
.spur-rail-review {{ background:#0B0E14; border:1px solid #2A2E38; border-radius:16px; padding:18px; color:#E6E1CF; font-family:ui-monospace,SFMono-Regular,Menlo,monospace; }}
.spur-rail-review input {{ position:absolute; opacity:0; }}
.spur-rail-review label {{ display:inline-block; margin:0 8px 14px 0; padding:6px 10px; border:1px solid #2A2E38; border-radius:999px; cursor:pointer; }}
.spur-rail-review input:checked + label {{ color:#0B0E14; background:#7FB4CA; border-color:#7FB4CA; }}
.spur-rail-plates {{ background:#111620; border-radius:10px; overflow:hidden; }}
.spur-rail-plate {{ display:none; }}
.spur-rail-plate img {{ display:block; width:100%; height:auto; }}
#spur-rail-1:checked ~ .spur-rail-plates .spur-rail-plate-1,
#spur-rail-2:checked ~ .spur-rail-plates .spur-rail-plate-2,
#spur-rail-3:checked ~ .spur-rail-plates .spur-rail-plate-3,
#spur-rail-4:checked ~ .spur-rail-plates .spur-rail-plate-4,
#spur-rail-5:checked ~ .spur-rail-plates .spur-rail-plate-5 {{ display:block; }}
</style>
<div class="spur-rail-review">
  <div style="font-size:12px;letter-spacing:.12em;margin-bottom:12px;color:#7FB4CA">PALMIER OVERLAY PLATES · CLICK TO REVIEW</div>
  {controls}
  <div class="spur-rail-plates">{plates}</div>
</div>
"""))
```

- [ ] **Step 3: run, save, reload, and verify**

1. `notebook_run_cell` on the inserted cell.
2. Confirm the output has five clickable stages and no Python error.
3. `notebook_save`.
4. `notebook_reload`.
5. `notebook_read_cell` and confirm the exact source survived the round trip.

Then verify the generated files:

```bash
for stage in 1 2 3 4 5; do
  ffprobe -v error -select_streams v:0 -show_entries stream=width,height,pix_fmt \
    -of default=noprint_wrappers=1 \
    "docs/product_launch/media_pack/ph_ready/overlays/progress-$stage.png"
done
```

Each plate must be 1920×1080 RGBA.

- [ ] **Step 4: commit only the notebook source of truth**

```bash
git diff --check
git add docs/product_launch/media_pack/product-hunt-media-pack.ipynb
git commit -m "docs(product-launch): D4.n add interactive rail review"
```

## Task 4: Build the 45-second Palmier timeline

**External project mutation:** `/Users/kevintruong/Documents/Palmier Pro/SPUR Product Hunt Hero - Real TUI.palmier`

**Output:** `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-45s.mp4`

- [ ] **Step 1: open the existing Palmier project and prove the baseline timeline still exists**

Call `manage_project` with `action="open"` and the exact project path. Call `get_timeline` once. Record the active timeline and timeline list; do not rename, duplicate over, or delete the 23.2-second baseline.

- [ ] **Step 2: inspect all source windows before placing clips**

Call `inspect_media` with `maxFrames=8` for these exact windows:

| Media ref | Start | End | Required visible evidence |
|---|---:|---:|---|
| `791B452C` | 8.0 | 20.0 | Session Detail operator surface |
| `14D82963` | 15.0 | 29.0 | Worker list and state |
| `82D9D60A` | 8.0 | 20.0 | Plans / explicit empty state |
| `63605F31` | 45.0 | 59.0 | Agent, model, and effort controls |
| `4B29113A` | 0.0 | 9.2 | Resumed-from-prior-conversation marker |

Fail closed if the required evidence is not visible throughout the candidate window; do not substitute the D4 diagnostic capture.

- [ ] **Step 3: import and inspect the generated assets**

Call `import_media` once for the audio directory with `folder="Product Hunt Dual Hero/Audio"`, and once for the overlay directory with `folder="Product Hunt Dual Hero/Overlays"`. Import the approved resume still separately as `Resume approved hold`. Call `get_media`, then `inspect_media` on every returned audio/image media ref before it is used.

- [ ] **Step 4: create an empty timeline named `Kinetic Operator Cut — 45s`**

Call `get_timeline` and require that no timeline already has this exact name. If one exists, stop and ask whether to resume it; do not create a duplicate or delete it. Call `create_timeline` with only that name, `set_active_timeline` with the returned timeline ID, then `set_project_settings` with:

```json
{"fps":30,"width":1920,"height":1080}
```

Call `get_timeline` and confirm the new timeline is empty and active.

- [ ] **Step 5: assemble exact picture timing on one video track**

Use `add_clips` with the existing matte and reviewed media refs:

| Timeline frames | Media ref | Source seconds |
|---|---|---|
| 0–90 | `4387A028` | still via `endFrame=90` |
| 90–300 | `791B452C` | 8.0–15.0 |
| 300–540 | `14D82963` | 17.0–25.0 |
| 540–750 | `82D9D60A` | 12.0–19.0 |
| 750–990 | `63605F31` | 50.0–58.0 |
| 990–1200 | `4B29113A` | 2.2–9.2 |
| 1200–1350 | `4387A028` | still via `endFrame=1350` |

Omit `trackIndex` on every entry so Palmier creates one shared picture track. The resulting picture duration must be exactly 1,350 frames.

- [ ] **Step 6: add progress overlays and approved text**

Add the five overlay images on one shared top video track:

| Frames | Plate |
|---|---|
| 90–300 | `progress-1.png` |
| 300–540 | `progress-2.png` |
| 540–750 | `progress-3.png` |
| 750–990 | `progress-4.png` |
| 990–1200 | `progress-5.png` |

Add one top text track with these exact entries and no animation:

| Frames | Content |
|---|---|
| 0–90 | `SPUR\nONE DURABLE HARNESS FOR CODING AGENTS` |
| 90–300 | `01 / SESSION\nONE OPERATOR HOME` |
| 300–540 | `02 / WORKERS\nSTATE STAYS VISIBLE` |
| 540–750 | `03 / PLAN\nEMPTY MEANS EMPTY` |
| 750–990 | `04 / ROUTING\nAGENT · MODEL · EFFORT` |
| 990–1200 | `05 / RESUME\nCONTEXT SURVIVES` |
| 1200–1350 | `SPUR\nVISIBLE · ISOLATED · RECOVERABLE\nINSTALL COMMUNITY FREE` |

Title and CTA use centered 76 px bold ivory text with 0.06 tracking and `centerX=0.5`, `centerY=0.5`, `width=0.78`, `height=0.38`. Proof cues use the locked upper-left cue style.

- [ ] **Step 7: apply the restrained shared grade and keyframed pushes**

Apply the locked grade to the five product-picture clip IDs, not to mattes, overlays, or text.

For each product clip, call `set_keyframes` twice. Scale rows are clip-relative:

```json
[[0,1.0,1.0,"hold"],[30,1.0,1.0,"smooth"],[LAST,1.08,1.08,"smooth"]]
```

Position rows are:

| Beat | Final top-left position |
|---|---|
| Session | `[-0.040,-0.040]` |
| Workers | `[-0.040,-0.034]` |
| Plan | `[-0.040,-0.038]` |
| Routing | `[-0.044,-0.050]` |
| Resume | `[-0.040,-0.036]` |

Use rows `[[0,0,0,"hold"],[30,0,0,"smooth"],[LAST,x,y,"smooth"]]`, where `LAST` is clip duration minus one: 209, 239, 209, 239, 209 respectively. Inspect after keyframing; if proof text becomes clipped, reduce the final scale to 1.06 rather than moving beyond the safe canvas.

- [ ] **Step 8: add audio, captions, and music ducking**

Add `narration-45s.wav` at frame 0 and `music-45s.wav` at frame 0 on separate audio tracks. Set narration volume to 1.0. Set music volume keyframes, clip-relative, to:

```json
[[0,0.05,"linear"],[60,0.10,"smooth"],[120,0.14,"smooth"],[1170,0.14,"hold"],[1200,0.06,"smooth"],[1320,0.04,"smooth"],[1349,0.0,"smooth"]]
```

Call `add_captions` with language `en`, `maxWords=7`, animation `off`, `centerX=0.5`, `centerY=0.84`, and the locked caption style. Read the caption clips from `get_timeline`; compare their concatenated words against the exact 45-second script, then correct every transcription mismatch with `update_text`. The corrected concatenation must equal the approved script ignoring punctuation and case.

- [ ] **Step 9: inspect every beat before export**

Call `inspect_timeline` for frames 45, 105, 330, 570, 780, 1020, and 1260. Then inspect 12 evenly sampled frames across 0–1350. Confirm cue placement, rail state, caption legibility, status-bar visibility, and proof evidence. Do not export until the composited timeline is exactly 1,350 frames.

No git commit: this task mutates the external Palmier project only.

## Task 5: Build the 90-second Palmier timeline

**External project mutation:** `/Users/kevintruong/Documents/Palmier Pro/SPUR Product Hunt Hero - Real TUI.palmier`

**Output:** `docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-90s.mp4`

- [ ] **Step 1: create an empty timeline named `Kinetic Operator Cut — 90s`**

Call `get_timeline` and require that no timeline already has this exact name. If one exists, stop and ask whether to resume it. Otherwise call `create_timeline`, activate the returned timeline, set 1920×1080 at 30 fps, and confirm it is empty.

- [ ] **Step 2: assemble exact picture timing**

| Timeline frames | Picture | Source seconds |
|---|---|---|
| 0–150 | SPUR matte `4387A028` | still |
| 150–300 | Session `791B452C` | 8.0–13.0 |
| 300–450 | Workers `14D82963` | 18.0–23.0 |
| 450–810 | Session `791B452C` | 8.0–20.0 |
| 810–1230 | Workers `14D82963` | 15.0–29.0 |
| 1230–1590 | Plan `82D9D60A` | 8.0–20.0 |
| 1590–2010 | Routing `63605F31` | 45.0–59.0 |
| 2010–2286 | Resume `4B29113A` | 0.0–9.2 |
| 2286–2310 | Approved resume still | still via `endFrame=2310` |
| 2310–2520 | SPUR matte `4387A028` | still |
| 2520–2700 | SPUR matte `4387A028` | still |

The approved still is a real-source derivative and only holds the final resume evidence for 24 frames. The picture duration must be exactly 2,700 frames.

- [ ] **Step 3: add overlays and exact authored text**

Use progress plates only on the five proof beats: frames 450–810, 810–1230, 1230–1590, 1590–2010, and 2010–2310.

Add these text entries:

| Frames | Content |
|---|---|
| 0–150 | `MORE AGENTS.\nMORE HIDDEN STATE.` |
| 150–450 | `THE HARD PART IS KNOWING\nWHAT IS RUNNING · WHERE IT LIVES · HOW TO RECOVER` |
| 450–810 | `01 / SESSION\nONE DURABLE OPERATOR HOME` |
| 810–1230 | `02 / WORKERS\nVISIBLE STATE · ISOLATED WORKTREES` |
| 1230–1590 | `03 / PLAN\nHONEST INVENTORY · EMPTY STAYS EMPTY` |
| 1590–2010 | `04 / ROUTING\nAGENT · MODEL · EFFORT STAY EXPLICIT` |
| 2010–2310 | `05 / RESUME\nRETURN WITHOUT DISCARDING CONTEXT` |
| 2310–2520 | Four truth-card text entries defined below |
| 2520–2700 | `SPUR\nVISIBLE · ISOLATED · RECOVERABLE · REVIEWABLE\nINSTALL COMMUNITY FREE` |

Hook/problem/title/CTA use centered text. Proof beats use the locked upper-left cue style. Build the truth card as four separate left-aligned entries on the open ink matte, all spanning frames 2310–2520:

| Content | Color | Size | Center X/Y | Width/height |
|---|---|---:|---|---|
| `PROVEN NOW` | `#7FB4CA` | 48 px bold | 0.33 / 0.34 | 0.48 / 0.10 |
| `SESSION · WORKERS · ROUTING · RESUME` | `#E6E1CF` | 34 px | 0.41 / 0.43 | 0.64 / 0.10 |
| `NOT PROVEN YET` | `#957FB8` | 48 px bold | 0.36 / 0.60 | 0.54 / 0.10 |
| `CONTINUOUS REJECT · RETRY · APPROVE OWNERSHIP` | `#E6E1CF` | 34 px | 0.48 / 0.69 | 0.78 / 0.10 |

Use no text background, outline, panel chrome, or UI-like dividers so the card cannot be mistaken for SPUR UI.

- [ ] **Step 4: grade and animate product picture**

Apply the same locked grade to every product-picture clip, including the two montage clips, but not the resume still. Apply 100→104% pushes to the two five-second montage clips and the locked 100→108% pushes to the five proof clips. Use the same evidence anchors as the 45-second timeline. Keep the 24-frame resume still static.

- [ ] **Step 5: add 90-second audio, captions, and music ducking**

Add `narration-90s.wav` at frame 0 and `music-90s.wav` at frame 0. Set narration volume to 1.0. Set music volume keyframes to:

```json
[[0,0.05,"linear"],[90,0.10,"smooth"],[180,0.14,"smooth"],[2250,0.14,"hold"],[2280,0.06,"smooth"],[2500,0.06,"hold"],[2520,0.04,"smooth"],[2699,0.0,"smooth"]]
```

Generate captions with the same locked style and correct them against the exact 90-second script. The concatenated corrected caption words must equal the approved script ignoring punctuation and case.

- [ ] **Step 6: inspect the full story and truth boundary**

Inspect frames 75, 225, 375, 480, 840, 1260, 1620, 2040, 2295, 2385, and 2580, then 12 evenly sampled frames over 0–2700. Confirm:

- Every cue is readable at 512-pixel review width.
- No caption or rail covers terminal status, worker state, routing controls, or the resume marker.
- `NOT PROVEN YET` is as legible as `PROVEN NOW`.
- No part of the truth card resembles a captured product panel.
- Timeline duration is exactly 2,700 frames.

No git commit: this task mutates the external Palmier project only.

## Task 6: Export, decode, review, and update the notebook handoff

**Files:**

- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Generate (ignored): the two final MP4 review candidates

- [ ] **Step 1: export the 45-second active timeline by ID**

Call `get_timeline`, find the timeline whose name exactly equals `Kinetic Operator Cut — 45s`, and copy its returned ID. Refuse to export if there is zero match or more than one match. Call `export_project` with that exact ID and these remaining fields:

```json
{
  "mode":"video",
  "codec":"H.264",
  "resolution":"1080p",
  "overwrite":false,
  "outputPath":"/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-45s.mp4"
}
```

Poll with `manage_exports(action="list")` at non-busy intervals until the exact job is terminal. A failed export is a hard stop.

- [ ] **Step 2: export the 90-second timeline the same way**

Call `get_timeline`, require exactly one timeline named `Kinetic Operator Cut — 90s`, and pass its returned ID to `export_project` with output path `/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/hero-video-palmier-pro-90s.mp4`. Poll to terminal completion.

- [ ] **Step 3: verify both containers and fully decode both streams**

```bash
PACK_ROOT="$PWD/docs/product_launch/media_pack"
for duration in 45 90; do
  video="$PACK_ROOT/ph_ready/hero-video-palmier-pro-${duration}s.mp4"
  ffprobe -v error \
    -show_entries format=filename,duration,size \
    -show_entries stream=index,codec_type,codec_name,width,height,pix_fmt,r_frame_rate,sample_rate,channels \
    -of json "$video"
  ffmpeg -nostdin -v error -xerror -i "$video" -f null -
  shasum -a 256 "$video"
done
```

Required: H.264 video, 1920×1080, yuv420p, 30/1 fps, one audio stream, exact 45.000/90.000-second durations, zero decode errors.

- [ ] **Step 4: measure mix loudness and inspect audible transitions**

```bash
for duration in 45 90; do
  video="$PACK_ROOT/ph_ready/hero-video-palmier-pro-${duration}s.mp4"
  ffmpeg -nostdin -hide_banner -i "$video" \
    -filter_complex ebur128=peak=true -f null - 2>&1 | tail -n 24
done
```

Listen to the opening fade, the 45-second music splice, the 90-second truth sentence, and both CTAs. Narration must remain intelligible; there must be no clipping, vocals in the music, hard splice, or abrupt tail.

- [ ] **Step 5: create and visually inspect contact sheets outside the repository**

```bash
REVIEW_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/spur-dual-hero-review.XXXXXX")"
ffmpeg -nostdin -y -v error -i "$PACK_ROOT/ph_ready/hero-video-palmier-pro-45s.mp4" \
  -vf 'fps=1/5,scale=512:-1,tile=3x3' -frames:v 1 "$REVIEW_ROOT/hero-45-contact.png"
ffmpeg -nostdin -y -v error -i "$PACK_ROOT/ph_ready/hero-video-palmier-pro-90s.mp4" \
  -vf 'fps=1/9,scale=512:-1,tile=5x2' -frames:v 1 "$REVIEW_ROOT/hero-90-contact.png"
```

Open both PNGs with the image viewer. Confirm evidence order, caption/cue legibility, truth-card balance, and restrained motion by additionally checking the Palmier samples from Tasks 4 and 5. Remove the temporary review directory after inspection.

- [ ] **Step 6: append a final interactive review cell through Notebook MCP**

Reload the notebook from disk. Insert a final code cell with this complete source; it renders two controlled local-video players, a radio-button duration switch, and measured metadata without embedding either MP4 as base64:

```python
from pathlib import Path
import hashlib
import json
import subprocess
from IPython.display import HTML, display

pack_root = Path("/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack")
candidates = {
    "45": pack_root / "ph_ready" / "hero-video-palmier-pro-45s.mp4",
    "90": pack_root / "ph_ready" / "hero-video-palmier-pro-90s.mp4",
}

rows = []
panels = []
for duration, path in candidates.items():
    if not path.is_file():
        raise FileNotFoundError(path)
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error",
        "-show_entries", "format=duration,size:stream=codec_name,codec_type,width,height,pix_fmt,r_frame_rate",
        "-of", "json", str(path),
    ]))
    video_stream = next(stream for stream in probe["streams"] if stream["codec_type"] == "video")
    has_audio = any(stream["codec_type"] == "audio" for stream in probe["streams"])
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    relative_src = f"ph_ready/{path.name}"
    rows.append(
        "<tr>"
        f"<td>{duration}s</td><td>{float(probe['format']['duration']):.3f}s</td>"
        f"<td>{video_stream['width']}×{video_stream['height']}</td>"
        f"<td>{video_stream['codec_name'].upper()} · {video_stream['pix_fmt']} · {video_stream['r_frame_rate']} fps</td>"
        f"<td>{'yes' if has_audio else 'no'}</td><td>{int(probe['format']['size']):,}</td>"
        f"<td><code>{digest}</code></td></tr>"
    )
    panels.append(
        f'<div class="spur-hero-panel spur-hero-panel-{duration}">'
        f'<video controls preload="metadata" src="{relative_src}"></video></div>'
    )

display(HTML(f"""
<style>
.spur-hero-review {{ background:#0B0E14; color:#E6E1CF; border:1px solid #2A2E38; border-radius:16px; padding:20px; font-family:ui-monospace,SFMono-Regular,Menlo,monospace; }}
.spur-hero-review input {{ position:absolute; opacity:0; }}
.spur-hero-review label {{ display:inline-block; margin:0 8px 14px 0; padding:7px 12px; border:1px solid #2A2E38; border-radius:999px; cursor:pointer; }}
.spur-hero-review input:checked + label {{ color:#0B0E14; background:#7FB4CA; border-color:#7FB4CA; }}
.spur-hero-panel {{ display:none; }}
.spur-hero-panel video {{ display:block; width:100%; border-radius:10px; background:#000; }}
#spur-hero-45:checked ~ .spur-hero-panels .spur-hero-panel-45,
#spur-hero-90:checked ~ .spur-hero-panels .spur-hero-panel-90 {{ display:block; }}
.spur-hero-review table {{ width:100%; margin-top:16px; border-collapse:collapse; font-size:12px; }}
.spur-hero-review th,.spur-hero-review td {{ padding:8px; border-top:1px solid #2A2E38; text-align:left; vertical-align:top; }}
.spur-hero-review code {{ color:#7FB4CA; overflow-wrap:anywhere; }}
</style>
<div class="spur-hero-review">
  <div style="font-size:12px;letter-spacing:.12em;margin-bottom:12px;color:#7FB4CA">45S / 90S REVIEW CANDIDATES</div>
  <input type="radio" name="spur-hero-duration" id="spur-hero-45" checked><label for="spur-hero-45">45 seconds</label>
  <input type="radio" name="spur-hero-duration" id="spur-hero-90"><label for="spur-hero-90">90 seconds</label>
  <div class="spur-hero-panels">{''.join(panels)}</div>
  <table><thead><tr><th>Cut</th><th>Duration</th><th>Canvas</th><th>Video</th><th>Audio</th><th>Bytes</th><th>SHA-256</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
</div>
"""))
```

Run the cell, save, reload, and verify the rendered HTML survives.

- [ ] **Step 7: run repository and notebook completion checks**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git diff --check
git status --short
```

The existing proof-manifest test must remain green. The two review candidates are intentionally outside that manifest.

- [ ] **Step 8: commit the final notebook handoff**

```bash
git add docs/product_launch/media_pack/product-hunt-media-pack.ipynb
git commit -m "docs(product-launch): D4.o add dual hero review handoff"
```

- [ ] **Step 9: apply verification-before-completion and report evidence**

Before claiming completion, re-run the final FFprobe/decode/status checks. Report:

- both absolute output paths;
- exact durations and codecs;
- both SHA-256 hashes;
- Higgsfield result URLs without raw job JSON;
- Palmier timeline names;
- notebook path;
- the explicit statement that both exports remain review candidates and do not prove continuous Reject → Retry → Approve ownership.

## Plan self-review checklist

- [ ] Every beat in the approved spec maps to exact timeline frames.
- [ ] Both outputs include the same five reviewed proof beats in the approved order.
- [ ] The 90-second truth boundary is explicit and not styled as product UI.
- [ ] Only Higgsfield audio generation is authorized; no image/video generation job is present.
- [ ] Notebook MCP owns notebook edits and begins with a disk reload.
- [ ] Palmier timeline IDs and generated import IDs are captured from tool receipts before later mutations; no guessed ID is used.
- [ ] The 23.2-second baseline file, baseline timeline, and proof manifest remain unchanged.
- [ ] No unresolved implementation choices remain in this plan.
