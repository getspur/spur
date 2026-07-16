# Product Launch Media Pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the misleading Product Hunt media bundle with a truth-validated capture pipeline and a designed, static-first HTML launch handoff.

**Architecture:** A root `proof-manifest.json` binds each published asset to one real source, proof timestamp or segment, checksum, caption, and channel. Shell contracts validate the manifest, media specifications, HTML portability, and visible proof text before `refresh.sh` atomically publishes derivatives. The HTML remains a hand-authored, scripts-off baseline; the hero renderer consumes only manifest-approved segments.

**Tech Stack:** Bash, jq, ffmpeg/ffprobe, tesseract OCR, VHS, Node.js/Puppeteer, static HTML/CSS, existing SPUR TUI demo harness.

---

## File map

| Path | Responsibility |
|---|---|
| `docs/product_launch/media_pack/tests/media-contract.test.sh` | Media, manifest, HTML, and OCR acceptance contract |
| `docs/product_launch/media_pack/proof-manifest.json` | Approved proof sources, timestamps, checksums, captions, channels, and outputs |
| `docs/product_launch/media_pack/refresh.sh` | Stage exact manifest-selected stills and atomically publish Product Hunt derivatives |
| `scripts/e2e/demos/tui-live/story-contract.test.sh` | Static journey/tape assertions, extended for fail-closed Session Detail proof |
| `scripts/e2e/demos/tui-live/tapes/04-session-resume.tape` and `09-13*.tape` | Runtime capture navigation and proof dwell |
| `docs/product_launch/media_pack/demo_render/content-graph.json` | Manifest-aligned hero segment timeline |
| `docs/product_launch/media_pack/demo_render/build.sh` | Assemble the approved hero without unrelated captions |
| `docs/product_launch/media_pack/demo_render/html/thumbnail.html` | Legible 512 by 512 SPUR thumbnail source |
| `docs/product_launch/media_pack/html/index.html` | Static Open Design launch handoff |
| `docs/product_launch/media_pack/MANIFEST.md` | Human source-of-truth and rebuild guide |
| `docs/product_launch/media_pack/tests/contact-sheet.sh` | Repeatable visual review sheets in a caller-selected output directory |

## Task 1: Add the failing truth contract

**Files:**
- Create: `docs/product_launch/media_pack/tests/media-contract.test.sh`
- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Write the failing media contract**

Create an executable Bash test with the following concrete contract:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/proof-manifest.json"
HTML="$ROOT/html/index.html"
failures=0

pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }
require() { command -v "$1" >/dev/null || { printf 'missing tool: %s\n' "$1" >&2; exit 2; }; }

require jq
require ffprobe
require shasum
require tesseract

[[ -f "$MANIFEST" ]] || fail "proof manifest exists"
[[ -f "$HTML" ]] || fail "HTML artifact exists"

if [[ -f "$MANIFEST" ]]; then
  while IFS=$'\t' read -r id source timestamp checksum; do
    path="$ROOT/$source"
    [[ -f "$path" ]] || { fail "$id source exists"; continue; }
    duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$path")"
    awk -v t="$timestamp" -v d="$duration" 'BEGIN { exit !(t >= 0 && t < d) }' \
      && pass "$id timestamp in range" || fail "$id timestamp in range"
    actual="$(shasum -a 256 "$path" | awk '{print $1}')"
    [[ "$actual" == "$checksum" ]] && pass "$id checksum" || fail "$id checksum"
  done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") |
    [.id,.source,(.timestamp_sec|tostring),.approved_source_sha256] | @tsv' "$MANIFEST")
fi

rg -q '<div[^>]+id="gallery"[^>]*></div>' "$HTML" \
  && fail "gallery is not JavaScript-only" || pass "gallery is static"
rg -q 'https?://|<script[^>]+src=|<link[^>]+href=' "$HTML" \
  && fail "HTML has no remote resources" || pass "HTML has no remote resources"
rg -q '—|–|&mdash;|&#8212;|&ndash;|&#8211;' "$HTML" \
  && fail "artifact has no banned dash glyphs" || pass "artifact has no banned dash glyphs"

while IFS= read -r ref; do
  target="$(cd "$(dirname "$HTML")" && pwd)/$ref"
  [[ -f "$target" ]] && pass "HTML asset exists: $ref" || fail "HTML asset exists: $ref"
done < <(sed -nE 's/.*(src|href)="(\.\.\/[^"#]+)".*/\2/p' "$HTML")

for image in "$ROOT"/ph_ready/gallery-*.png; do
  dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$image")"
  [[ "$dims" == "1270x760" ]] && pass "$(basename "$image") dimensions" \
    || fail "$(basename "$image") dimensions"
done

if [[ -f "$MANIFEST" ]]; then
  while IFS=$'\t' read -r id output term; do
    image="$ROOT/ph_ready/$output"
    [[ -f "$image" ]] || { fail "$id published output exists"; continue; }
    ocr="$(tesseract "$image" stdout --psm 6 2>/dev/null | tr '[:lower:]' '[:upper:]')"
    needle="$(printf '%s' "$term" | tr '[:lower:]' '[:upper:]')"
    [[ "$ocr" == *"$needle"* ]] && pass "$id visibly proves $term" \
      || fail "$id visibly proves $term"
  done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") as $a |
    $a.expected_proof[] | [$a.id,$a.output,.] | @tsv' "$MANIFEST")
fi

thumb="$ROOT/ph_ready/thumbnail-240.png"
[[ "$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$thumb")" == "240x240" ]] \
  && pass "thumbnail dimensions" || fail "thumbnail dimensions"

hero="$ROOT/ph_ready/hero-video-ph-ready.mp4"
hero_spec="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height -of csv=s=x:p=0 "$hero")"
hero_duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$hero")"
[[ "$hero_spec" == "h264x1920x1080" ]] && pass "hero codec and dimensions" || fail "hero codec and dimensions"
awk -v d="$hero_duration" 'BEGIN { exit !(d <= 60.0) }' && pass "hero duration" || fail "hero duration"

[[ "$failures" -eq 0 ]] || exit 1
printf 'All media-pack contracts passed\n'
```

Mark it executable with `chmod +x docs/product_launch/media_pack/tests/media-contract.test.sh`.

- [ ] **Step 2: Extend the journey test with a failing tape assertion**

Add the following assertion inside the tape loop so a required Session Detail proof cannot accept initialization failure:

```bash
assert_lacks "$file" 'Wait\+Screen.*Failed to initialize' \
  "$tape does not accept failed initialization as Session Detail proof"
```

Add a specific ordered assertion for the plan-loop resolution:

```bash
assert_matches "$root/tapes/13-problem-plan-loop-drive.tape" \
  '# STORY: PROOF - return home to Session Detail\nHide\nEscape\nShow\nWait\+Screen@[1-9][0-9]*s /Session ·\|INSERT/' \
  'plan-loop returns to Session Detail before resolution proof'
```

Add a resume-tape assertion that distinguishes `Session Detail` from the `Sessions` picker:

```bash
assert_has "$root/tapes/04-session-resume.tape" '/Session ·|INSERT/' \
  'resume tape requires Session Detail rather than matching Sessions picker text'
```

- [ ] **Step 3: Run both tests and verify RED**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: the story contract fails on tapes that accept `Failed to initialize`; the media contract fails because `proof-manifest.json` is absent and the HTML gallery is JavaScript-only.

- [ ] **Step 4: Commit the failing tests**

```bash
git add scripts/e2e/demos/tui-live/story-contract.test.sh \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "test(product-launch): MP.b guard media proof contract"
```

## Task 2: Make the capture contracts fail closed

**Files:**
- Modify: `scripts/e2e/demos/tui-live/tapes/04-session-resume.tape`
- Modify: `scripts/e2e/demos/tui-live/tapes/09-product-e2e-flow.tape`
- Modify: `scripts/e2e/demos/tui-live/tapes/10-problem-ops-visibility.tape`
- Modify: `scripts/e2e/demos/tui-live/tapes/11-problem-plan-progress.tape`
- Modify: `scripts/e2e/demos/tui-live/tapes/12-problem-backlog-triage.tape`
- Modify: `scripts/e2e/demos/tui-live/tapes/13-problem-plan-loop-drive.tape`

- [ ] **Step 1: Remove failure from all required Session Detail waits**

Use this exact required proof shape in tapes `09` through `13`:

```text
Wait+Screen@50s /Session ·|INSERT/
Sleep 3s
```

Do not include `Failed to initialize` in a success expression. Keep failure visible by allowing VHS to stop and return nonzero.

In `04-session-resume.tape`, replace the broad picker-matching wait:

```text
Wait+Screen@25s /Session/
```

with the Session Detail proof:

```text
Wait+Screen@25s /Session ·|INSERT/
Sleep 3s
```

- [ ] **Step 2: Make the plan-loop resolution return to Session Detail**

After the Dashboard stream/task proof, replace the permissive resolution wait with:

```text
# STORY: PROOF - return home to Session Detail
Hide
Escape
Show
Wait+Screen@15s /Session ·|INSERT/
Sleep 3s

# STORY: RESOLUTION
Wait+Screen@15s /Session ·|INSERT/
Sleep 3s
```

- [ ] **Step 3: Run the static contract and verify GREEN**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: all story-contract checks pass.

- [ ] **Step 4: Commit the capture fix**

```bash
git add scripts/e2e/demos/tui-live/tapes/{04-session-resume,09-product-e2e-flow,10-problem-ops-visibility,11-problem-plan-progress,12-problem-backlog-triage,13-problem-plan-loop-drive}.tape
git commit -m "fix(e2e-demos): MP.c require visible session proof"
```

## Task 3: Re-capture and approve semantic proof frames

**Files:**
- Create: `docs/product_launch/media_pack/proof-manifest.json`
- Create: `docs/product_launch/media_pack/tests/contact-sheet.sh`
- Regenerate ignored source captures under: `scripts/e2e/demos/tui-live/out/`

- [ ] **Step 1: Re-render the five observe-only value films**

Run:

```bash
cd scripts/e2e/demos/tui-live
SPUR_DEMO_STORIES_ONLY=1 SPUR_DEMO_STORY_PACE=1 ./render.sh
vhs -q tapes/04-session-resume.tape
```

Expected: tapes `09` through `13` and the resume tape pass and produce 2560 by 1600 MP4/GIF pairs. Do not enable `SPUR_DEMO_ALLOW_PLAN_LOOP`; this task has no model-spend requirement.

- [ ] **Step 2: Add a repeatable contact-sheet command**

Create `tests/contact-sheet.sh` with explicit output handling:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${1:?usage: contact-sheet.sh OUTPUT_DIR}"
mkdir -p "$DEST"

for source in "$ROOT"/live_demos/{04-session-resume,09-product-e2e-flow,10-problem-ops-visibility,11-problem-plan-progress,12-problem-backlog-triage,13-problem-plan-loop-drive}.mp4; do
  stem="$(basename "$source" .mp4)"
  ffmpeg -y -v error -i "$source" \
    -vf 'fps=1/4,scale=480:300,tile=4x3:padding=2:margin=2:color=0x0B0E14' \
    -frames:v 1 "$DEST/$stem-contact.png"
done

gallery=(
  "$ROOT/ph_ready/gallery-01-session-detail-1270x760.png"
  "$ROOT/ph_ready/gallery-02-workers-plan-loop-1270x760.png"
  "$ROOT/ph_ready/gallery-03-plan-progress-1270x760.png"
  "$ROOT/ph_ready/gallery-04-specialist-routing-1270x760.png"
  "$ROOT/ph_ready/gallery-05-session-resume-1270x760.png"
)
if [[ -f "${gallery[0]}" && -f "${gallery[1]}" && -f "${gallery[2]}" && -f "${gallery[3]}" && -f "${gallery[4]}" ]]; then
  ffmpeg -y -v error \
    -i "${gallery[0]}" -i "${gallery[1]}" -i "${gallery[2]}" -i "${gallery[3]}" -i "${gallery[4]}" \
    -filter_complex '[0:v]scale=508:304[a];[1:v]scale=508:304[b];[2:v]scale=508:304[c];[3:v]scale=508:304[d];[4:v]scale=508:304[e];[a][b][c][d][e]xstack=inputs=5:layout=0_0|508_0|1016_0|0_304|508_304[out]' \
    -map '[out]' -frames:v 1 "$DEST/gallery-contact.png"
fi
```

- [ ] **Step 3: Copy fresh captures into the pack and inspect contact sheets**

Run:

```bash
cp scripts/e2e/demos/tui-live/out/{04-session-resume,09-product-e2e-flow,10-problem-ops-visibility,11-problem-plan-progress,12-problem-backlog-triage,13-problem-plan-loop-drive}.{mp4,gif} \
  docs/product_launch/media_pack/live_demos/
bash docs/product_launch/media_pack/tests/contact-sheet.sh /tmp/spur-media-review
```

Inspect every generated contact sheet. Record exact timestamps where the intended proof is readable. Reject any journey whose required screen is absent rather than relabeling it.

- [ ] **Step 4: Create the proof manifest with reviewed values**

Use `shasum -a 256` on the fresh sources and build the complete manifest with reviewed timestamps supplied as shell variables:

```bash
: "${SESSION_TS:?set SESSION_TS from the reviewed contact sheet}"
: "${WORKERS_TS:?set WORKERS_TS from the reviewed contact sheet}"
: "${PLAN_TS:?set PLAN_TS from the reviewed contact sheet}"
: "${SPECIALIST_TS:?set SPECIALIST_TS from the reviewed contact sheet}"
: "${RESUME_TS:?set RESUME_TS from the reviewed contact sheet}"

PACK=docs/product_launch/media_pack
SHA13="$(shasum -a 256 "$PACK/live_demos/13-problem-plan-loop-drive.mp4" | awk '{print $1}')"
SHA11="$(shasum -a 256 "$PACK/live_demos/11-problem-plan-progress.mp4" | awk '{print $1}')"
SHA09="$(shasum -a 256 "$PACK/live_demos/09-product-e2e-flow.mp4" | awk '{print $1}')"
SHA04="$(shasum -a 256 "$PACK/live_demos/04-session-resume.mp4" | awk '{print $1}')"

jq -n \
  --arg captured_at "2026-07-16" \
  --arg sha13 "$SHA13" --arg sha11 "$SHA11" --arg sha09 "$SHA09" --arg sha04 "$SHA04" \
  --argjson session_ts "$SESSION_TS" --argjson workers_ts "$WORKERS_TS" \
  --argjson plan_ts "$PLAN_TS" --argjson specialist_ts "$SPECIALIST_TS" --argjson resume_ts "$RESUME_TS" \
  '{
    version: 1,
    captured_at: $captured_at,
    assets: [
      {id:"session-detail-home",kind:"still",source:"live_demos/13-problem-plan-loop-drive.mp4",journey:"problem-plan-loop-drive",timestamp_sec:$session_ts,expected_proof:["Session","INSERT"],caption:"Session Detail keeps the plan and worker loop in one place.",output:"gallery-01-session-detail-1270x760.png",channel:["product-hunt-gallery","html"],approved_source_sha256:$sha13,status:"approved"},
      {id:"workers-plan-loop",kind:"still",source:"live_demos/13-problem-plan-loop-drive.mp4",journey:"problem-plan-loop-drive",timestamp_sec:$workers_ts,expected_proof:["EXEC","stream"],caption:"Worker output stays visible beside the plan loop.",output:"gallery-02-workers-plan-loop-1270x760.png",channel:["product-hunt-gallery","html"],approved_source_sha256:$sha13,status:"approved"},
      {id:"plan-progress",kind:"still",source:"live_demos/11-problem-plan-progress.mp4",journey:"problem-plan-progress",timestamp_sec:$plan_ts,expected_proof:["Progress","Work item"],caption:"Campaign progress becomes one decision surface.",output:"gallery-03-plan-progress-1270x760.png",channel:["product-hunt-gallery","html"],approved_source_sha256:$sha11,status:"approved"},
      {id:"specialist-routing",kind:"still",source:"live_demos/09-product-e2e-flow.mp4",journey:"product-e2e-flow",timestamp_sec:$specialist_ts,expected_proof:["agent=","model=","effort="],caption:"Agent, model, and effort stay explicit before dispatch.",output:"gallery-04-specialist-routing-1270x760.png",channel:["product-hunt-gallery","html"],approved_source_sha256:$sha09,status:"approved"},
      {id:"session-resume",kind:"still",source:"live_demos/04-session-resume.mp4",journey:"session-resume",timestamp_sec:$resume_ts,expected_proof:["Session","INSERT"],caption:"Saved history returns to the operator surface.",output:"gallery-05-session-resume-1270x760.png",channel:["product-hunt-gallery","html"],approved_source_sha256:$sha04,status:"approved"}
    ],
    hero: {output:"hero-video-ph-ready.mp4",segments:[]}
  }' > "$PACK/proof-manifest.json"
```

The timestamp variables must come from the inspected fresh captures. If one required proof is absent, stop and repair that tape instead of assigning a nearby unrelated timestamp.

- [ ] **Step 5: Run the media test and verify it advances past manifest checks**

Run:

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: manifest source, timestamp, and checksum checks pass; the test still fails on the JavaScript-only HTML until Task 6.

- [ ] **Step 6: Commit the proof map and review helper**

```bash
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/tests/contact-sheet.sh
git commit -m "feat(product-launch): MP.d bind assets to reviewed proof"
```

## Task 4: Replace byte-size selection with staged semantic publishing

**Files:**
- Modify: `docs/product_launch/media_pack/refresh.sh`
- Create: `docs/product_launch/media_pack/demo_render/html/thumbnail.html`
- Modify: `docs/product_launch/media_pack/demo_render/scripts/render-html-frames.mjs`

- [ ] **Step 1: Remove `pick_best` and candidate timestamp lists**

Delete the byte-size selection function and all calls that pass multiple candidate timestamps.

- [ ] **Step 2: Implement manifest-driven staged extraction**

Use a temporary stage and publish only after every extraction succeeds:

```bash
MANIFEST="$PACK/proof-manifest.json"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/spur-media-pack.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/gallery_stills" "$STAGE/ph_ready"

while IFS=$'\t' read -r id source timestamp output; do
  src="$PACK/$source"
  raw="$STAGE/gallery_stills/$id.png"
  [[ -f "$src" ]] || { printf 'missing source for %s: %s\n' "$id" "$src" >&2; exit 1; }
  duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$src")"
  awk -v t="$timestamp" -v d="$duration" 'BEGIN { exit !(t >= 0 && t < d) }' \
    || { printf 'timestamp out of range for %s: %s >= %s\n' "$id" "$timestamp" "$duration" >&2; exit 1; }
  ffmpeg -y -v error -ss "$timestamp" -i "$src" -frames:v 1 "$raw"
  ffmpeg -y -v error -i "$raw" \
    -vf 'scale=1270:760:force_original_aspect_ratio=increase,crop=1270:760' \
    "$STAGE/ph_ready/$output"
done < <(jq -r '.assets[] | select(.status == "approved" and .kind == "still") |
  [.id,.source,(.timestamp_sec|tostring),.output] | @tsv' "$MANIFEST")

rm -rf "$PACK/gallery_stills.approved" "$PACK/ph_ready.approved"
mv "$STAGE/gallery_stills" "$PACK/gallery_stills.approved"
mv "$STAGE/ph_ready" "$PACK/ph_ready.approved"
```

After all dimension, checksum, and OCR checks pass against the staged paths, replace the published directories with `mv` operations. Preserve non-derived hero and marketing files explicitly rather than deleting them implicitly.

- [ ] **Step 3: Generate a legible SPUR thumbnail**

Render a 512 by 512 HTML title tile using the existing Puppeteer renderer, then derive 240 by 240 with ffmpeg. Use visible copy `SPUR` and `Control tower for CLI coding agents.` with the fixed palette. Do not rely on terminal text.

- [ ] **Step 4: Run refresh and verify GREEN for derivatives**

Run:

```bash
docs/product_launch/media_pack/refresh.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: manifest, checksum, timestamp, image dimensions, thumbnail, and path checks pass. HTML static-content and hero-caption checks may remain red for later tasks.

- [ ] **Step 5: Commit the publisher**

```bash
git add docs/product_launch/media_pack/refresh.sh \
  docs/product_launch/media_pack/gallery_stills \
  docs/product_launch/media_pack/ph_ready
git commit -m "fix(product-launch): MP.e publish semantic proof frames"
```

## Task 5: Rebuild the hero from matching evidence

**Files:**
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Modify: `docs/product_launch/media_pack/demo_render/content-graph.json`
- Modify: `docs/product_launch/media_pack/demo_render/build.sh`
- Modify: `docs/product_launch/media_pack/demo_render/html/01-title.html`
- Modify: `docs/product_launch/media_pack/demo_render/html/03-end.html`
- Modify: `docs/product_launch/media_pack/demo_render/html/cap-session.html`
- Modify: `docs/product_launch/media_pack/demo_render/html/cap-workers.html`
- Modify: `docs/product_launch/media_pack/demo_render/html/cap-plans.html`
- Delete or stop using: `docs/product_launch/media_pack/demo_render/html/cap-specialists.html`
- Delete or stop using: `docs/product_launch/media_pack/demo_render/html/cap-resume.html`

- [ ] **Step 1: Write a failing caption-source assertion**

Extend `media-contract.test.sh`:

```bash
jq -e '([.hero.segments[].id] | sort) == ["plans","session","workers"]' \
  "$MANIFEST" >/dev/null || fail "hero declares session, workers, and plans segments"
jq -e 'all(.hero.segments[];
  (.caption == null) or ((.proof_terms // []) | length > 0))' \
  "$MANIFEST" >/dev/null || fail "every hero caption has proof terms"
```

Run the media contract and confirm it fails against the current unrelated specialist/resume captions.

- [ ] **Step 2: Make the hero graph plan-loop-specific**

Define only title, Session Detail, workers/DELEGATE, plan progress, and install segments. Specialist and resume copy must be absent unless their own clips are included in the manifest. Use regular hyphens or punctuation instead of em/en dash glyphs.

```json
{
  "title": "SPUR Product Hunt hero demo",
  "duration_sec": 43,
  "fps": 30,
  "canvas": { "width": 1920, "height": 1080 },
  "segments": [
    { "id": "title", "duration_sec": 3, "kind": "html", "html": "html/01-title.html" },
    { "id": "session", "kind": "video", "asset_id": "session-detail-home", "caption": "Session Detail is the operator home.", "proof_terms": ["Session", "INSERT"] },
    { "id": "workers", "kind": "video", "asset_id": "workers-plan-loop", "caption": "Drive brain and worker loops in one session.", "proof_terms": ["DELEGATE", "Workers"] },
    { "id": "plans", "kind": "video", "asset_id": "plan-progress", "caption": "Plan progress stays visible.", "proof_terms": ["Progress", "Work item"] },
    { "id": "end", "duration_sec": 3, "kind": "html", "html": "html/03-end.html" }
  ]
}
```

- [ ] **Step 3: Update the build to consume manifest-approved segments**

Resolve each `asset_id` through `proof-manifest.json`, verify checksum before ffmpeg, and derive trim offsets from approved segment fields. Keep the current inline HTML frame render and H.264 concatenation, but remove hard-coded caption timing unrelated to the manifest.

- [ ] **Step 4: Build and verify GREEN**

Run:

```bash
docs/product_launch/media_pack/demo_render/build.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: hero is H.264, 1920 by 1080, at most 60 seconds, and every caption has manifest proof terms.

- [ ] **Step 5: Commit the hero repair**

```bash
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/demo_render \
  docs/product_launch/media_pack/ph_ready/hero-video-ph-ready.mp4 \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "fix(product-launch): MP.f align hero captions to proof"
```

## Task 6: Build the static Open Design HTML artifact

**Files:**
- Modify: `docs/product_launch/media_pack/html/index.html`

- [ ] **Step 1: Replace the JavaScript-rendered inventory with static semantic HTML**

Use this document structure, fully populated from approved manifest values:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>SPUR Product Hunt media pack</title>
  <style>
    :root{--bg:#0B0E14;--surface:#11141C;--text:#E6E1CF;--muted:#8B8680;--accent:#7FB4CA;--violet:#957FB8;--border:#2A2E38}
    *{box-sizing:border-box} html{background:var(--bg);color:var(--text)}
    body{margin:0;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;line-height:1.5}
    code,.mono{font-family:"SFMono-Regular",Consolas,"Liberation Mono",monospace}
    main,header,footer{width:min(1180px,calc(100% - 40px));margin-inline:auto}
    .hero{min-height:92vh;display:grid;grid-template-columns:minmax(0,1.15fr) minmax(320px,.85fr);align-items:center;gap:56px}
    .eyebrow{font:600 12px/1.2 "SFMono-Regular",monospace;letter-spacing:.12em;text-transform:uppercase;color:var(--accent)}
    h1{font-size:clamp(48px,7vw,96px);line-height:.95;letter-spacing:-.055em;margin:20px 0 28px;max-width:10ch}
    .lede{font-size:clamp(20px,2vw,28px);color:var(--muted);max-width:28ch}
    .proof-frame{border:1px solid var(--border);background:#000;box-shadow:0 30px 80px rgba(0,0,0,.35)}
    .proof-frame img,.proof-frame video{display:block;width:100%;height:auto}
    .chapter{display:grid;grid-template-columns:minmax(0,1.55fr) minmax(260px,.45fr);gap:40px;padding:96px 0;border-top:1px solid var(--border)}
    .chapter:nth-child(even){grid-template-columns:minmax(260px,.45fr) minmax(0,1.55fr)}
    .chapter:nth-child(even) .proof-frame{order:2}
    .meta{font:12px/1.6 "SFMono-Regular",monospace;color:var(--muted)}
    table{width:100%;border-collapse:collapse} th,td{text-align:left;padding:14px 10px;border-bottom:1px solid var(--border);vertical-align:top}
    a{color:var(--accent)} .marketing{border:1px solid var(--violet);padding:28px}
    @media(max-width:800px){.hero,.chapter,.chapter:nth-child(even){grid-template-columns:1fr}.chapter:nth-child(even) .proof-frame{order:0}}
  </style>
</head>
<body>
  <header class="hero">
    <div><p class="eyebrow">SPUR Product Hunt media pack</p><h1>Control tower for CLI coding agents.</h1><p class="lede">Real sessions. Real workers. One review surface.</p></div>
    <div class="proof-frame"><video controls poster="../ph_ready/gallery-01-session-detail-1270x760.png" src="../ph_ready/hero-video-ph-ready.mp4"></video></div>
  </header>
  <main>
    <section aria-labelledby="proof-title"><p class="eyebrow">Product proof</p><h2 id="proof-title">Five visible reasons to trust the control plane.</h2>
      <article class="chapter"><figure class="proof-frame"><img src="../ph_ready/gallery-01-session-detail-1270x760.png" alt="SPUR Session Detail with composer and transcript"></figure><div><p class="eyebrow">01 Session Detail</p><h3>The operator stays in the session.</h3><p>Composer, ReAct, workers, and plan state share one working context.</p><p class="meta">problem-plan-loop-drive | approved capture</p></div></article>
      <article class="chapter"><figure class="proof-frame"><img src="../ph_ready/gallery-02-workers-plan-loop-1270x760.png" alt="SPUR worker evidence inside a plan loop"></figure><div><p class="eyebrow">02 Workers</p><h3>Delegation remains visible.</h3><p>Worker and DELEGATE evidence stays connected to the operator session.</p><p class="meta">problem-plan-loop-drive | approved capture</p></div></article>
      <article class="chapter"><figure class="proof-frame"><img src="../ph_ready/gallery-03-plan-progress-1270x760.png" alt="SPUR plan progress view"></figure><div><p class="eyebrow">03 Plan progress</p><h3>A campaign becomes inventory.</h3><p>Lifecycle, ownership, and task progress form one decision surface.</p><p class="meta">problem-plan-progress | approved capture</p></div></article>
      <article class="chapter"><figure class="proof-frame"><img src="../ph_ready/gallery-04-specialist-routing-1270x760.png" alt="SPUR specialist routing with agent model and effort"></figure><div><p class="eyebrow">04 Specialist routing</p><h3>Choose the right worker without losing context.</h3><p>Agent, model, and effort remain explicit before dispatch.</p><p class="meta">product-e2e-flow | approved capture</p></div></article>
      <article class="chapter"><figure class="proof-frame"><img src="../ph_ready/gallery-05-session-resume-1270x760.png" alt="SPUR resumed session with history"></figure><div><p class="eyebrow">05 Resume</p><h3>Close the laptop. Keep the session.</h3><p>Saved history returns to the same operator surface.</p><p class="meta">session-resume | approved capture</p></div></article>
    </section>
    <section aria-labelledby="handoff-title"><p class="eyebrow">Launch handoff</p><h2 id="handoff-title">Approved Product Hunt uploads.</h2><table><thead><tr><th>Field</th><th>File</th><th>Specification</th><th>Proof</th></tr></thead><tbody>
      <tr><td>Thumbnail</td><td><a href="../ph_ready/thumbnail-240.png">thumbnail-240.png</a></td><td>240 by 240 PNG</td><td>SPUR identity</td></tr>
      <tr><td>Gallery 1</td><td><a href="../ph_ready/gallery-01-session-detail-1270x760.png">gallery-01-session-detail-1270x760.png</a></td><td>1270 by 760 PNG</td><td>Session Detail</td></tr>
      <tr><td>Gallery 2</td><td><a href="../ph_ready/gallery-02-workers-plan-loop-1270x760.png">gallery-02-workers-plan-loop-1270x760.png</a></td><td>1270 by 760 PNG</td><td>Workers</td></tr>
      <tr><td>Gallery 3</td><td><a href="../ph_ready/gallery-03-plan-progress-1270x760.png">gallery-03-plan-progress-1270x760.png</a></td><td>1270 by 760 PNG</td><td>Plan progress</td></tr>
      <tr><td>Gallery 4</td><td><a href="../ph_ready/gallery-04-specialist-routing-1270x760.png">gallery-04-specialist-routing-1270x760.png</a></td><td>1270 by 760 PNG</td><td>Specialist routing</td></tr>
      <tr><td>Gallery 5</td><td><a href="../ph_ready/gallery-05-session-resume-1270x760.png">gallery-05-session-resume-1270x760.png</a></td><td>1270 by 760 PNG</td><td>Resume</td></tr>
      <tr><td>Video</td><td><a href="../ph_ready/hero-video-ph-ready.mp4">hero-video-ph-ready.mp4</a></td><td>1920 by 1080 H.264</td><td>Plan and worker loop</td></tr>
    </tbody></table></section>
    <section class="marketing" aria-labelledby="marketing-title"><p class="eyebrow">Marketing treatment, not product proof</p><h2 id="marketing-title">Social assets stay in their lane.</h2></section>
    <details><summary>Provenance and rebuild</summary><pre><code>bash docs/product_launch/media_pack/tests/media-contract.test.sh</code></pre></details>
  </main>
  <footer><p>SPUR. Real TUI captures, reviewed against the launch journey.</p></footer>
</body>
</html>
```

Populate all five chapters and every upload table row as static HTML. Do not leave an empty `tbody`, gallery container, or film container in the implementation.

- [ ] **Step 2: Add optional progressive enhancement only after static completeness**

If copy buttons or filters add real value, include a single inline script that attaches to existing elements. The artifact must remain complete when that script is removed.

- [ ] **Step 3: Run the media contract and verify GREEN**

Run:

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: all media, HTML, path, remote-resource, and banned-glyph checks pass.

- [ ] **Step 4: Commit the HTML artifact**

```bash
git add docs/product_launch/media_pack/html/index.html
git commit -m "feat(product-launch): MP.g design launch media handoff"
```

## Task 7: Update handoff documentation and run final critique

**Files:**
- Modify: `docs/product_launch/media_pack/MANIFEST.md`
- Modify: `docs/product_launch/media_pack/demo_render/README.md`
- Modify: `docs/product_launch/media_pack/marketing/video_review/VIDEO_REVIEW.md`

- [ ] **Step 1: Update documentation from actual outputs**

Document:

- the proof manifest as canonical asset mapping;
- exact approved filenames;
- semantic timestamp selection instead of byte-size ranking;
- static HTML handoff path;
- hero build and verification commands;
- Product Hunt proof versus marketing channel separation;
- observe-only capture default and explicit live-seed spend gate.

Remove claims that the old Sessions picker frames are Session Detail, and remove any statement that the hero demonstrates specialists or resume unless those clips are actually present.

- [ ] **Step 2: Run all automated verification**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
docs/product_launch/media_pack/demo_render/build.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git diff --check
```

Expected: every command exits zero and the hero rebuild leaves the media contract green.

- [ ] **Step 3: Generate visual review sheets**

Run:

```bash
rm -rf /tmp/spur-media-final-review
bash docs/product_launch/media_pack/tests/contact-sheet.sh /tmp/spur-media-final-review
```

Inspect the hero timeline, five proof frames, marketing separation, and 240 by 240 thumbnail.

- [ ] **Step 4: Run the Open Design critique**

Score philosophy, hierarchy, execution, specificity, and restraint from 1 to 5. Revise any dimension below 3. Confirm:

- no purple gradient background;
- no generic feature icons;
- no invented metric;
- no card grid competing with the hero;
- one cyan primary accent;
- no em/en dash glyph in rendered copy;
- every caption matches the visible product state.

- [ ] **Step 5: Commit the finished handoff**

```bash
git add docs/product_launch/media_pack/MANIFEST.md \
  docs/product_launch/media_pack/demo_render/README.md \
  docs/product_launch/media_pack/marketing/video_review/VIDEO_REVIEW.md
git commit -m "docs(product-launch): MP.h finalize media pack handoff"
```

## Task 8: Final repository verification

**Files:** No new files.

- [ ] **Step 1: Confirm commit and worktree scope**

Run:

```bash
git status --short
git log --oneline -8
git diff HEAD~6 --stat
```

Expected: only pre-existing unrelated changes remain uncommitted; media-pack changes are committed in the planned TDD sequence.

- [ ] **Step 2: Report final artifacts**

Report clickable paths for:

- `docs/product_launch/media_pack/html/index.html`
- `docs/product_launch/media_pack/ph_ready/hero-video-ph-ready.mp4`
- `docs/product_launch/media_pack/ph_ready/thumbnail-240.png`
- the five approved Product Hunt gallery images
- `docs/product_launch/media_pack/proof-manifest.json`
- `docs/product_launch/media_pack/tests/media-contract.test.sh`

Include the verification commands and exact pass results. Do not claim completion from visual inspection alone.
