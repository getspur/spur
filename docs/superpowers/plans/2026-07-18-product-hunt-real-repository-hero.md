# Product Hunt Real-Repository Hero Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the archival feature montage with verified 45-second and 90-second launch videos derived from one real `spur` repository brain→plan→worker→HITL→synthesis campaign.

**Architecture:** Extend the existing opt-in live HITL journey from one demo task to three correlated read-only audit tasks, capture one complete campaign, and lock its source bytes and proof windows in the media manifest and notebook. Regenerate narration, create notebook-authored title/CTA/rail plates, then build two new versioned Palmier timelines from the same capture: the short cut uses the approval path, while the long cut includes Reject → Retry → Approve.

**Tech Stack:** Bash + shell-use/asciinema, SPUR TUI and MCP plan loop, beads, ffmpeg/ffprobe, jq, Notebook MCP/Jupyter + Python/Pillow, Higgsfield Inworld TTS, Palmier Pro MCP, Git.

---

## Locked inputs and preservation rules

- Approved design: `docs/superpowers/specs/2026-07-18-product-hunt-real-repository-hero-redesign.md`
- Interactive design source: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Existing Palmier project: `/Users/kevintruong/Documents/Palmier Pro/SPUR Product Hunt Hero - Real TUI.palmier`
- Existing baseline source and the `Kinetic Operator Cut — 45s` timeline are archival. Never delete or overwrite them.
- New capture stem: `16-live-product-hunt-audit-loop`
- New Palmier timelines:
  - `Real Repository Loop — 45s v2`
  - `Real Repository Loop — 90s v2`
- New exports:
  - `docs/product_launch/media_pack/ph_ready/hero-video-real-repo-45s-v2.mp4`
  - `docs/product_launch/media_pack/ph_ready/hero-video-real-repo-90s-v2.mp4`
- Run compile-heavy Rust commands only through `scripts/spur-cargo`; this plan changes shell/docs/media and does not require a Rust build unless an unexpected TUI code change becomes necessary.
- Paid model/TTS calls and the real read-only capture have already been approved. Do not broaden worker prompts to file writes.

## File responsibility map

| File | Responsibility |
|---|---|
| `scripts/e2e/demos/tui-live/lib.sh` | Exact three-task launch-audit interaction and hard proof gates |
| `scripts/e2e/demos/tui-live/capture-live-hitl.sh` | Higher-spend opt-in wrapper and versioned output stem |
| `scripts/e2e/demos/tui-live/capture-live-seed.sh` | Opt-in full-duration 2560×1600 encode while preserving default previews |
| `scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh` | Human-readable story beat that invokes the HITL helper |
| `scripts/e2e/demos/tui-live/story-contract.test.sh` | Static safety, identity, prompt, order, and read-only contract |
| `scripts/e2e/demos/tui-live/README.md` and `PROBLEM_STORIES.md` | Operator instructions and exact promotable proof |
| `docs/product_launch/media_pack/refresh.sh` | Non-destructive hydration of the promoted capture |
| `docs/product_launch/media_pack/proof-manifest.json` | Source hashes, required markers, truth windows, and final output hashes |
| `docs/product_launch/media_pack/tests/media-contract.test.sh` | Runtime validation of the real campaign and exact exports |
| `docs/product_launch/media_pack/product-hunt-media-pack.ipynb` | Interactive proof review plus deterministic visual plates |
| `docs/product_launch/media_pack/.gitignore` | Ignore generated source copies, audio, plates, and versioned exports |

## Task 1: Write the failing three-task capture contract

**Files:**

- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: replace the obsolete one-task D4 assertion block**

Replace the existing D4 assertions from `require_hitl_loop_opt_in` through the `15-live-hitl-agent-loop` stem check. Do not append a second campaign contract: the old assertions require `seed_task_id="demo-hitl-$$"` and would make the new helper unimplementable. Use this replacement block:

```bash
capture_seed="$root/capture-live-seed.sh"
assert_has "$lib" 'require_hitl_loop_opt_in() {' \
  'PH audit preserves the dedicated spend guard'
assert_has "$lib" 'beads-backed project' \
  'PH audit preserves the beads preflight'
assert_has "$lib" 'local positioning_task_id="ph-acp-positioning-$$"' \
  'PH audit declares the positioning task correlation id'
assert_has "$lib" 'local proof_task_id="ph-tui-proof-$$"' \
  'PH audit declares the proof task correlation id'
assert_has "$lib" 'local readiness_task_id="ph-launch-readiness-$$"' \
  'PH audit declares the readiness task correlation id'
assert_has "$lib" 'land_plan_inspector_for_task() {' \
  'PH audit has a hard retrying Plan Inspector landing helper'
assert_has "$lib" 'Call submit_plan with exactly THREE independent read-only tasks.' \
  'PH audit requests a populated three-task plan'
assert_has "$lib" 'Worker: gemini.' \
  'PH audit routes captured TUI proof to Gemini'
assert_has "$lib" 'Worker: claude-code.' \
  'PH audit routes ACP positioning to Claude Code'
assert_has "$lib" 'Worker: codex.' \
  'PH audit routes launch readiness to Codex'
assert_count_at_least "$lib" 'effort: medium.' 3 \
  'PH audit requests visible resolved effort for every worker'
assert_has "$lib" 'Leave every completed task awaiting_review for the operator.' \
  'PH audit prevents the brain from auto-reviewing worker results'
assert_has "$lib" 'PH PROOF FINDING:' \
  'PH audit first review exposes the proof-worker marker'
assert_has "$lib" 'SOURCE: <exact path>' \
  'PH audit retry requires an exact source path'
assert_has "$lib" 'WINDOW: <exact seconds or line range>' \
  'PH audit retry requires an exact proof window'
assert_has "$lib" 'PH POSITIONING FINDING:' \
  'PH audit exposes the positioning result before approval'
assert_has "$lib" 'PH READINESS FINDING:' \
  'PH audit exposes the readiness result before approval'
assert_has "$lib" 'PH AUDIT SYNTHESIS:' \
  'PH audit ends with brain synthesis in the originating session'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+land_plan_inspector_for_task "\$positioning_task_id"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key j[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key d[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key R[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*' \
  'PH audit preserves clean approval before proof Reject then Retry then Approve'
assert_has "$hitl_capture" 'SPUR_DEMO_CAPTURE_STEM_PREFIX=16-live-product-hunt-audit-loop' \
  'PH capture wrapper preserves the D4 stem by using a new versioned stem'
assert_has "$hitl_capture" 'SPUR_CAPTURE_FULL_FIDELITY=1' \
  'PH capture requests the full-duration 2560x1600 encode path'
assert_has "$hitl_capture" 'SPUR_AGG_IDLE_LIMIT="${SPUR_AGG_IDLE_LIMIT:-6.0}"' \
  'PH capture preserves proof dwells instead of truncating them to 1.5 seconds'
assert_has "$capture_seed" 'local full_fidelity="${SPUR_CAPTURE_FULL_FIDELITY:-0}"' \
  'capture seed keeps full-fidelity encoding opt-in'
assert_has "$capture_seed" 'scale=2560:1600:force_original_aspect_ratio=decrease' \
  'full-fidelity encode has the approved capture geometry'
assert_has "$plan_loop" 'trigger_submit_plan_hitl_review_and_synthesize' \
  'PH journey still invokes the guarded campaign helper'
```

- [ ] **Step 2: run the contract and verify the new assertions fail**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: non-zero exit with failures for the three `ph-*` task IDs, hard Plan Inspector landing, human-review hold, ordered clean/retry approval path, versioned stem, and full-duration encode mode. Existing non-D4 safety assertions must continue to pass.

- [ ] **Step 3: commit the failing contract**

```bash
git add scripts/e2e/demos/tui-live/story-contract.test.sh
git commit -m "test(tui-live): D4.t require real campaign capture"
```

## Task 2: Implement the three-worker real-project HITL journey

**Files:**

- Modify: `scripts/e2e/demos/tui-live/lib.sh`
- Modify: `scripts/e2e/demos/tui-live/capture-live-hitl.sh`
- Modify: `scripts/e2e/demos/tui-live/capture-live-seed.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh`
- Modify: `scripts/e2e/demos/tui-live/README.md`
- Modify: `scripts/e2e/demos/tui-live/PROBLEM_STORIES.md`

- [ ] **Step 1: replace the HITL helper with the exact real campaign**

Add the hard landing helper immediately before the campaign helper, then replace `trigger_submit_plan_hitl_review_and_synthesize` in `lib.sh` with:

```bash
land_plan_inspector_for_task() {
  local task_id="$1"
  local timeout_s="${2:-180}"
  local deadline=$((SECONDS + timeout_s))

  while (( SECONDS < deadline )); do
    press_key Alt+p
    sleep_ms 0.6
    if soft_has_text "Task detail" 1200 && soft_has_text "$task_id" 1200; then
      printf '+ proof: Plan Inspector is pinned to %s\n' "$task_id"
      story_dwell 2.5
      return 0
    fi
    sleep_ms 0.4
  done

  printf 'fatal: Plan Inspector never pinned to %s within %ss\n' "$task_id" "$timeout_s" >&2
  return 1
}

trigger_submit_plan_hitl_review_and_synthesize() {
  require_hitl_loop_opt_in
  local positioning_task_id="ph-acp-positioning-$$"
  local proof_task_id="ph-tui-proof-$$"
  local readiness_task_id="ph-launch-readiness-$$"

  land_session_detail "Attach Session Detail for the Product Hunt audit" 2.5
  sleep_ms 0.8
  printf '+ PH audit: ask the brain for three read-only tasks in the real project\n'

  type_slow "PRODUCT HUNT LIVE CAPTURE. "
  type_text "Audit docs/product_launch/media_pack in the real spur repository. "
  type_text "Call submit_plan with exactly THREE independent read-only tasks. "
  type_text "Task 1 id: ${positioning_task_id}. Worker: claude-code. effort: medium. deps: none. "
  type_text "Prompt: Check the approved ACP-compatible positioning against repository docs and integration boundaries. "
  type_text "Return exactly one line beginning PH POSITIONING FINDING:. Make no file changes. "
  type_text "Task 2 id: ${proof_task_id}. Worker: gemini. effort: medium. deps: none. "
  type_text "Prompt: Inspect the real TUI captures and identify one launch claim that needs stronger source proof. "
  type_text "Return exactly one line beginning PH PROOF FINDING:. Make no file changes. "
  type_text "Task 3 id: ${readiness_task_id}. Worker: codex. effort: medium. deps: none. "
  type_text "Prompt: Review Product Hunt pacing, accessibility, and handoff completeness. "
  type_text "Return exactly one line beginning PH READINESS FINDING:. Make no file changes. "
  type_text "After submit_plan succeeds, reply with plan_id only. "
  type_text "When workers finish, do not call review_task, retry_plan_task, or merge_plan. "
  type_text "Leave every completed task awaiting_review for the operator."
  sleep_ms 0.5
  press_key Enter

  story_hard_proof "The prompt carries the positioning task identity" "$positioning_task_id" 2.5
  story_hard_proof "The prompt carries the proof task identity" "$proof_task_id" 2.5
  story_hard_proof "The prompt carries the readiness task identity" "$readiness_task_id" 2.5
  story_hard_proof "The brain accepts the Product Hunt audit" "THINK" 2.5

  press_key Alt+d
  story_hard_proof "The inline lineage panel is visible" "Workers" 2.5
  story_hard_proof "The worker panel shows Claude Code" "claude-code" 2.5
  story_hard_proof "The worker panel shows Gemini" "gemini" 2.5
  story_hard_proof "The worker panel shows Codex" "codex" 2.5
  story_dwell 3.0
  press_key Alt+d

  land_plan_inspector_for_task "$positioning_task_id" 180
  story_hard_proof "The positioning result reaches review" "awaiting_review" 4.0
  story_hard_proof "The positioning finding is visible" "summary: PH POSITIONING FINDING:" 3.5
  press_key a
  story_hard_proof "The operator opens clean positioning approval" "Decision: Approve" 3.5
  press_key Enter
  story_hard_proof "The positioning task records approval" "approved" 3.0

  press_key j
  story_hard_proof "The proof task is selected" "$proof_task_id" 2.5
  story_hard_proof "The proof result reaches review" "awaiting_review" 4.0
  story_hard_proof "The first proof finding is visible" "summary: PH PROOF FINDING:" 3.5

  press_key d
  story_hard_proof "The operator rejects evidence without an exact source window" "Decision: Reject" 3.5
  press_key Enter
  story_hard_proof "The proof task records rejection" "rejected" 3.0

  press_key R
  story_hard_proof "The operator opens retry instructions" "Retry Task" 3.0
  type_slow "READ ONLY. Re-run the same check and return exactly three lines: "
  type_text "SOURCE: <exact path>; WINDOW: <exact seconds or line range>; "
  type_text "RECOMMENDATION: <one sentence>. Make no file changes."
  story_hard_proof "The retry visibly requests an exact source" "SOURCE:" 2.5
  story_hard_proof "The retry visibly requests an exact window" "WINDOW:" 2.5
  press_key Enter

  story_hard_proof "The retry returns to review" "awaiting_review" 4.0
  story_hard_proof "The retry remains on the proof task" "$proof_task_id" 2.5
  story_hard_proof "The retry exposes source evidence" "summary: SOURCE:" 3.5
  story_hard_proof "The retry exposes a proof window" "WINDOW:" 3.5
  story_hard_proof "The retry exposes a recommendation" "RECOMMENDATION:" 3.5
  press_key a
  story_hard_proof "The operator approves the grounded proof result" "Decision: Approve" 3.5
  press_key Enter
  story_hard_proof "The proof task records approval" "approved" 3.0

  press_key j
  story_hard_proof "The readiness task is selected" "$readiness_task_id" 2.5
  story_hard_proof "The readiness result reaches review" "awaiting_review" 4.0
  story_hard_proof "The readiness finding is visible" "summary: PH READINESS FINDING:" 3.5
  press_key a
  story_hard_proof "The operator approves launch readiness" "Decision: Approve" 3.5
  press_key Enter
  story_hard_proof "The readiness task records approval" "approved" 3.0

  return_to_session_detail
  story_session_land "The same brain session remains the operator home" 2.5
  type_slow "PRODUCT HUNT AUDIT COMPLETE. Synthesize the approved findings for "
  type_text "${proof_task_id}, ${positioning_task_id}, and ${readiness_task_id}. "
  type_text "Begin with PH AUDIT SYNTHESIS: and follow with one sentence. "
  type_text "Do not call tools or delegate."
  sleep_ms 0.5
  press_key Enter
  story_hard_proof "The brain synthesizes all approved findings in the same session" \
    "PH AUDIT SYNTHESIS:" 4.0
}
```

The three tasks are deliberately independent and submitted in positioning → proof → readiness order. `PlanInspectorView` derives `stage_idx` from dependency depth, so all three remain in stage 0 and `j` advances through this stable plan order even after the selected task changes status.

- [ ] **Step 2: version the capture wrapper without overwriting D4 media**

Change `capture-live-hitl.sh` to:

```bash
#!/usr/bin/env bash
# Capture the higher-spend real-repository Product Hunt HITL story.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_DEMO_ALLOW_HITL_LOOP=1
export SPUR_DEMO_ALLOW_PLAN_LOOP=0
export SPUR_DEMO_CAPTURE_STEM_PREFIX=16-live-product-hunt-audit-loop
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-420}"
export SPUR_CAPTURE_FULL_FIDELITY=1
export SPUR_AGG_IDLE_LIMIT="${SPUR_AGG_IDLE_LIMIT:-6.0}"
exec "$ROOT/capture-live-seed.sh"
```

- [ ] **Step 3: add an opt-in full-duration capture encode**

In `capture-live-seed.sh`, declare `local full_fidelity="${SPUR_CAPTURE_FULL_FIDELITY:-0}"` beside `preview_w`. Change the existing sampled-preview `if [[ -f "$gif_out" ]] ...; then` to an `elif` without changing its body, then insert this branch immediately before it:

```bash
if [[ -f "$gif_out" && "$full_fidelity" == "1" ]]; then
  command -v ffmpeg >/dev/null 2>&1 || return 1
  echo "==> ffmpeg full-fidelity mp4 (2560x1600, 30 fps)"
  ffmpeg -nostdin -hide_banner -loglevel error -y -i "$gif_out" \
    -vf 'fps=30,scale=2560:1600:force_original_aspect_ratio=decrease,pad=2560:1600:(ow-iw)/2:(oh-ih)/2:color=0x0B0E14' \
    -c:v libx264 -pix_fmt yuv420p -preset medium -crf 18 \
    -movflags +faststart "$mp4_out" || return 1
  echo "mp4: $mp4_out"
```

This produces one `if`/`elif` chain ending at the existing `fi`. The full-fidelity branch must preserve the complete GIF duration; it must not use the current `n // 120` sampling loop.

- [ ] **Step 4: revise the visible journey beat**

In `journeys/problem-plan-loop-drive.sh`, replace the D4 beat with:

```bash
if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "Real Product Hunt audit: three deep dives, evidence retry, approvals, then brain synthesis."
  trigger_submit_plan_hitl_review_and_synthesize
```

Keep the existing `elif` branches unchanged.

- [ ] **Step 5: update the operator documentation**

In `README.md` and `PROBLEM_STORIES.md`, replace the one-task D4 description with these facts:

```text
The opt-in Product Hunt capture submits three independent read-only tasks in the
real spur project: ACP positioning (Claude Code), TUI proof (Gemini), and launch
readiness (Codex). The proof task is rejected once for a missing exact source
window, retried with SOURCE/WINDOW/RECOMMENDATION requirements, and approved.
The remaining two findings are approved before PH AUDIT SYNTHESIS appears in the
originating Session Detail. Stable outputs use the
16-live-product-hunt-audit-loop stem. Any missing task identity, state, finding,
decision, or synthesis marker fails the journey.
```

Keep the spend warning and `.beads` preflight language.

- [ ] **Step 6: run syntax and contract verification**

```bash
bash -n scripts/e2e/demos/tui-live/lib.sh
bash -n scripts/e2e/demos/tui-live/capture-live-hitl.sh
bash -n scripts/e2e/demos/tui-live/capture-live-seed.sh
bash -n scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: all syntax checks exit 0 and the contract ends with `All story-contract checks passed`.

- [ ] **Step 7: commit the implementation**

```bash
git add scripts/e2e/demos/tui-live/lib.sh \
  scripts/e2e/demos/tui-live/capture-live-hitl.sh \
  scripts/e2e/demos/tui-live/capture-live-seed.sh \
  scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh \
  scripts/e2e/demos/tui-live/README.md \
  scripts/e2e/demos/tui-live/PROBLEM_STORIES.md
git commit -m "feat(tui-live): D4.u capture real launch audit loop"
```

## Task 3: Capture and promote the real campaign

**Files:**

- Generate (ignored): `scripts/e2e/demos/tui-live/out/16-live-product-hunt-audit-loop.{cast,mp4,log}`
- Generate (ignored): `docs/product_launch/media_pack/live_demos/16-live-product-hunt-audit-loop.{cast,mp4,log}`
- Modify: `docs/product_launch/media_pack/refresh.sh`
- Modify: `docs/product_launch/media_pack/.gitignore`
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Modify: `docs/product_launch/media_pack/tests/media-contract.test.sh`

- [ ] **Step 1: preflight workers, project, tools, and free space**

Use `list_available_workers` and require `gemini`, `claude-code`, and `codex`. Then run:

```bash
test -d /Volumes/Projects/spur/.beads
command -v ffmpeg
command -v ffprobe
command -v jq
command -v shasum
df -h /Volumes/Projects/spur
```

Expected: every command exits 0 and at least 2 GiB is free. Stop before model spend if a worker, `.beads`, tool, or space gate fails.

- [ ] **Step 2: run the real capture from the isolated implementation worktree**

```bash
cd scripts/e2e/demos/tui-live
SPUR_DEMO_PROJECT=/Volumes/Projects/spur \
SPUR_DEMO_STORY_PACE=1 \
SPUR_AGG_SPEED=1.15 \
SPUR_DEMO_PLAN_LOOP_WAIT_S=420 \
./capture-live-hitl.sh
```

Expected: journey exit 0 and stable files under `out/16-live-product-hunt-audit-loop.*`. Do not retry blindly after paid work; inspect the stable log first.

- [ ] **Step 3: enforce the log and media gates**

```bash
CAPTURE_ROOT="$PWD/out"
STEM="16-live-product-hunt-audit-loop"
test -s "$CAPTURE_ROOT/$STEM.log"
test -s "$CAPTURE_ROOT/$STEM.cast"
test -s "$CAPTURE_ROOT/$STEM.mp4"
for marker in \
  'ph-tui-proof-' \
  'ph-acp-positioning-' \
  'ph-launch-readiness-' \
  'PH PROOF FINDING:' \
  'Decision: Reject' \
  'Retry Task' \
  'SOURCE:' \
  'WINDOW:' \
  'PH POSITIONING FINDING:' \
  'PH READINESS FINDING:' \
  'Decision: Approve' \
  'PH AUDIT SYNTHESIS:'; do
  rg -F "$marker" "$CAPTURE_ROOT/$STEM.log"
done
ffprobe -v error -show_entries format=duration,size \
  -show_entries stream=codec_name,width,height,r_frame_rate \
  -of json "$CAPTURE_ROOT/$STEM.mp4"
ffmpeg -nostdin -v error -i "$CAPTURE_ROOT/$STEM.mp4" -f null -
```

Expected: all markers resolve, video is H.264 at the approved 2560×1600 capture geometry and 30 fps, and the full decode exits 0.

- [ ] **Step 4: copy the capture non-destructively into the media pack**

```bash
PACK_ROOT="$(git rev-parse --show-toplevel)/docs/product_launch/media_pack"
mkdir -p "$PACK_ROOT/live_demos"
for ext in cast mp4 log; do
  cp -n -p "$CAPTURE_ROOT/$STEM.$ext" "$PACK_ROOT/live_demos/$STEM.$ext"
  cmp "$CAPTURE_ROOT/$STEM.$ext" "$PACK_ROOT/live_demos/$STEM.$ext"
done
shasum -a 256 "$PACK_ROOT/live_demos/$STEM.mp4"
```

Expected: `cmp` passes for all three files. If a destination exists with different bytes, stop and choose a new stem; never overwrite it.

- [ ] **Step 5: write the failing real-campaign manifest contract**

Add this block to `tests/media-contract.test.sh` after the existing manifest checks:

```bash
if [[ -f "$MANIFEST" ]]; then
  campaign_source="$(jq -r '.real_campaign.source // empty' "$MANIFEST")"
  campaign_cast="$(jq -r '.real_campaign.cast // empty' "$MANIFEST")"
  campaign_log="$(jq -r '.real_campaign.log // empty' "$MANIFEST")"
  campaign_sha="$(jq -r '.real_campaign.approved_source_sha256 // empty' "$MANIFEST")"
  if jq -e '(.real_campaign.required_markers | length) == 12 and
      ([.real_campaign.windows | keys[]] | sort) ==
      ["approve_clean","approve_retry","plan","prompt","reject","retry","synthesis","workers"]' \
      "$MANIFEST" >/dev/null; then
    pass "real campaign declares markers and eight truth windows"
  else
    fail "real campaign declares markers and eight truth windows"
  fi
  for ref in "$campaign_source" "$campaign_cast" "$campaign_log"; do
    [[ -n "$ref" && -s "$ROOT/$ref" ]] && pass "real campaign source exists: $ref" \
      || fail "real campaign source exists: $ref"
  done
  if [[ -n "$campaign_log" && -f "$ROOT/$campaign_log" ]]; then
    while IFS= read -r marker; do
      rg -q --fixed-strings -- "$marker" "$ROOT/$campaign_log" \
        && pass "real campaign log marker: $marker" \
        || fail "real campaign log marker: $marker"
    done < <(jq -r '.real_campaign.required_markers[]' "$MANIFEST")
  fi
  if [[ -n "$campaign_source" && -f "$ROOT/$campaign_source" ]]; then
    actual_campaign_sha="$(shasum -a 256 "$ROOT/$campaign_source" | awk '{print $1}')"
    [[ "$actual_campaign_sha" == "$campaign_sha" ]] && pass "real campaign checksum" \
      || fail "real campaign checksum"
    campaign_duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$ROOT/$campaign_source")"
    while IFS=$'\t' read -r id start end; do
      if awk -v s="$start" -v e="$end" -v d="$campaign_duration" \
          'BEGIN { exit !(s >= 0 && e > s && e <= d) }'; then
        pass "real campaign window in range: $id"
      else
        fail "real campaign window in range: $id"
      fi
    done < <(jq -r '.real_campaign.windows | to_entries[] |
      [.key,(.value.start_sec|tostring),(.value.end_sec|tostring)] | @tsv' "$MANIFEST")
    campaign_video_facts="$(ffprobe -v error -select_streams v:0 \
      -show_entries stream=codec_name,width,height,r_frame_rate -of json "$ROOT/$campaign_source")"
    jq -e '.streams[0] | .codec_name == "h264" and .width == 2560 and
      .height == 1600 and .r_frame_rate == "30/1"' \
      <<<"$campaign_video_facts" >/dev/null \
      && pass "real campaign is H.264 2560x1600 at 30 fps" \
      || fail "real campaign is H.264 2560x1600 at 30 fps"
  fi
fi
```

Run `bash docs/product_launch/media_pack/tests/media-contract.test.sh` and expect failure because `.real_campaign` is not defined yet. Commit the failing test:

```bash
git add docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "test(product-launch): D4.v require real campaign proof"
```

- [ ] **Step 6: inspect the promoted source and record actual truth windows**

Generate a temporary two-second contact sheet outside the repository:

```bash
REVIEW_DIR="$(mktemp -d "${TMPDIR:-/tmp}/spur-real-campaign.XXXXXX")"
ffmpeg -nostdin -v error -i "$PACK_ROOT/live_demos/$STEM.mp4" \
  -vf 'fps=1/2,scale=640:-1,tile=4x4' -vsync 0 "$REVIEW_DIR/sheet-%02d.png"
printf '%s\n' "$REVIEW_DIR"
```

Inspect every sheet and the source video. Record exact start/end seconds for `prompt`, `plan`, `workers`, `approve_clean`, `reject`, `retry`, `approve_retry`, and `synthesis`. `approve_clean` must be the first-attempt ACP-positioning approval used by the 45-second cut; `approve_retry` must be the second-attempt TUI-proof approval used by the 90-second cut. Each window must begin on a frame where its required identity/proof is already visible. Keep the review directory until Task 5 is approved, then delete only that exact `spur-real-campaign.*` directory.

- [ ] **Step 7: add the real campaign manifest using the measured values**

Use `apply_patch` to add a top-level `real_campaign` object to `proof-manifest.json`. Set the immutable fields to `id: product-hunt-audit-v2`, `status: approved`, and the three versioned `live_demos/16-live-product-hunt-audit-loop.{mp4,cast,log}` paths. Copy the lowercase SHA-256 printed in Step 4 directly into `approved_source_sha256`; never enter a descriptive token and replace it later.

Add the 12 markers already enumerated by the log gate. Under `windows`, add exactly the eight measured windows from Step 6. Every window has numeric `start_sec`/`end_sec` values and a `proof_terms` array:

- `prompt`: `PRODUCT HUNT LIVE CAPTURE` plus all three `ph-*` task prefixes.
- `plan`: all three task prefixes and the populated-plan identity.
- `workers`: `claude-code`, `gemini`, `codex`, and the visible model/effort labels from the lineage panel.
- `approve_clean`: the positioning task prefix, `attempt 1`, and `Decision: Approve`.
- `reject`: the proof task prefix, `attempt 1`, and `Decision: Reject`.
- `retry`: the proof task prefix plus `SOURCE:`, `WINDOW:`, and `RECOMMENDATION:`.
- `approve_retry`: the proof task prefix, `attempt 2`, and `Decision: Approve`.
- `synthesis`: `PH AUDIT SYNTHESIS:` plus the same Session Detail/repository identity.

Before staging, require `approved_source_sha256` to match `^[0-9a-f]{64}$`, every window to satisfy `end_sec > start_sec`, and the exact eight-key set enforced by the test. Also add the three new source filenames to `refresh.sh` using `cp -n -p`, and add these ignore rules:

```gitignore
ph_ready/hero-video-real-repo-45s-v2.mp4
ph_ready/hero-video-real-repo-90s-v2.mp4
```

- [ ] **Step 8: verify and commit source promotion metadata**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git diff --check
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/refresh.sh \
  docs/product_launch/media_pack/.gitignore
git commit -m "docs(product-launch): D4.w lock real campaign source"
```

Expected: media contracts pass and heavy media remains ignored.

## Task 4: Regenerate versioned narration and reuse the approved music

**Files:**

- Generate (ignored): `docs/product_launch/media_pack/ph_ready/audio/narration-real-repo-45s.{job.json,source,wav}`
- Generate (ignored): `docs/product_launch/media_pack/ph_ready/audio/narration-real-repo-90s.{job.json,source,wav}`
- Reuse unchanged: `docs/product_launch/media_pack/ph_ready/audio/music-45s.wav`
- Reuse unchanged: `docs/product_launch/media_pack/ph_ready/audio/music-90s.wav`

- [ ] **Step 1: verify Higgsfield authentication and the approved voice**

```bash
higgsfield auth status
higgsfield model get inworld_text_to_speech --json
```

Expected: authenticated and `Simon (en)` is available. Do not start a paid job if either check fails.

- [ ] **Step 2: declare the exact approved scripts**

```bash
PACK_ROOT="$PWD/docs/product_launch/media_pack"
AUDIO_ROOT="$PACK_ROOT/ph_ready/audio"
HERO_45_SCRIPT='More coding agents create more hidden work. SPUR keeps it visible, reviewable, and recoverable. SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Kiro, Gemini, and beyond—one durable outer harness. Inside the real SPUR repository, the operator asks the brain to audit this Product Hunt launch. The brain submits a populated plan and delegates three read-only deep dives. Worker state and evidence stay visible. The operator approves the result. The brain synthesizes the findings in the same durable session. Try SPUR Community free.'
HERO_90_SCRIPT='More coding agents create more hidden work. The hard part is knowing what is running, where the evidence lives, and whether the operator can recover the context. SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Kiro, Gemini, and beyond—one durable outer harness. Inside the real SPUR repository, the operator asks the brain to audit this Product Hunt launch. The brain submits a populated plan with three read-only deep dives: ACP positioning, TUI proof, and launch readiness. Each worker keeps its agent, model, effort, attempt, and current state visible. The operator can inspect the plan and the evidence without leaving the durable session. One finding arrives without enough source proof. The operator rejects it and explains what is missing. SPUR retries the same task instead of hiding the failure. The new attempt returns with the requested evidence, and the operator approves it. Only approved findings return to the brain. It synthesizes the launch recommendation in the same conversation, with the real project still in view. SPUR makes multi-agent work visible, reviewable, and recoverable. Try SPUR Community free.'
mkdir -p "$AUDIO_ROOT"
```

- [ ] **Step 3: run only the two approved paid TTS jobs**

```bash
higgsfield generate create inworld_text_to_speech \
  --prompt "$HERO_45_SCRIPT" --voice 'Simon (en)' --wait --json \
  > "$AUDIO_ROOT/narration-real-repo-45s.job.json"
higgsfield generate create inworld_text_to_speech \
  --prompt "$HERO_90_SCRIPT" --voice 'Simon (en)' --wait --json \
  > "$AUDIO_ROOT/narration-real-repo-90s.job.json"
```

Require successful terminal jobs and `.[0].result_url`. Do not regenerate music.

- [ ] **Step 4: download and normalize to stereo 48 kHz WAV**

```bash
for asset in narration-real-repo-45s narration-real-repo-90s; do
  result_url="$(jq -er '.[0].result_url' "$AUDIO_ROOT/$asset.job.json")"
  curl --fail --location "$result_url" --output "$AUDIO_ROOT/$asset.source"
  ffmpeg -nostdin -y -v error -i "$AUDIO_ROOT/$asset.source" \
    -af 'loudnorm=I=-16:TP=-1.5:LRA=7' -ar 48000 -ac 2 "$AUDIO_ROOT/$asset.wav"
done
```

- [ ] **Step 5: enforce timing and integrity**

```bash
for asset in narration-real-repo-45s narration-real-repo-90s music-45s music-90s; do
  ffprobe -v error -show_entries format=filename,duration,size \
    -show_entries stream=codec_name,sample_rate,channels \
    -of json "$AUDIO_ROOT/$asset.wav"
done
```

Gates:

- 45-second narration: 38.0–42.0 seconds.
- 90-second narration: 82.0–87.0 seconds.
- Both narrations: PCM, stereo, 48 kHz, peak below −1.5 dBFS.
- Music: exactly 45.0/90.0 seconds and byte-identical to the already approved files.
- If narration misses, stop and request copy approval; do not use `atempo` or silently edit words.

No commit: audio files are generated and ignored.

## Task 5: Build the notebook proof review and v2 plates

**Files:**

- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Generate (ignored): `docs/product_launch/media_pack/ph_ready/overlays/real-repo-*`

- [ ] **Step 1: reload the notebook from disk through Notebook MCP**

Call `notebook_context_pack`, `notebook_open` with the absolute path, then `notebook_reload`. Read the final design cell and append after it; never rewrite the whole notebook document.

- [ ] **Step 2: append an executable proof-window review cell**

Use `notebook_insert_cell(kind="code", code_type="python")` with code that:

1. Reads `.real_campaign.windows` and the capture path from `proof-manifest.json`.
2. Uses ffmpeg to extract the midpoint of each of the eight windows into `ph_ready/overlays/real-repo-review/`.
3. Embeds the eight PNGs as base64.
4. Renders CSS-only radio controls labeled `PROMPT`, `PLAN`, `WORKERS`, `APPROVE CLEAN`, `REJECT`, `RETRY`, `APPROVE RETRY`, and `SYNTHESIS`.
5. Shows start/end seconds and required proof terms beside each frame.

Use this complete data loop:

```python
from pathlib import Path
import base64, json, subprocess
from IPython.display import HTML, display

pack = Path("/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack")
manifest = json.loads((pack / "proof-manifest.json").read_text())
campaign = manifest["real_campaign"]
source = pack / campaign["source"]
review = pack / "ph_ready" / "overlays" / "real-repo-review"
review.mkdir(parents=True, exist_ok=True)
order = ["prompt", "plan", "workers", "approve_clean", "reject", "retry", "approve_retry", "synthesis"]
cards = []
for index, key in enumerate(order, 1):
    window = campaign["windows"][key]
    midpoint = (window["start_sec"] + window["end_sec"]) / 2
    image = review / f"{index:02d}-{key}.png"
    subprocess.run([
        "ffmpeg", "-nostdin", "-y", "-v", "error", "-ss", f"{midpoint:.3f}",
        "-i", str(source), "-frames:v", "1", str(image)
    ], check=True)
    encoded = base64.b64encode(image.read_bytes()).decode("ascii")
    cards.append((index, key, window, f"data:image/png;base64,{encoded}"))
```

Build the HTML using the same radio/panel pattern as the existing progress-rail and design cells. Run the cell and require eight legible frames. If a midpoint lacks its proof term, correct the manifest window and rerun the media contract before continuing.

- [ ] **Step 3: append a deterministic plate-generation cell**

Generate these 1920×1080 RGB PNGs using Pillow, SF Mono, ink `#0B0E14`, ivory `#E6E1CF`, and cyan `#7FB4CA`:

```text
title-real-repo-45s.png / title-real-repo-90s.png
  SPUR
  ANY ACP-COMPATIBLE CODING AGENT
  ONE DURABLE OUTER HARNESS
  ACP = AGENT CLIENT PROTOCOL

cta-real-repo-45s.png / cta-real-repo-90s.png
  SPUR
  ONE DURABLE OUTER HARNESS
  beta.otobank.com
  COMMUNITY FREE
```

Also generate five `real-repo-45-progress-*.png` rails and seven `real-repo-90-progress-*.png` rails at y=982–988, x=200–1720. Display every plate family in a CSS-radio review panel.

- [ ] **Step 4: convert long plates to constant-frame H.264 video**

```bash
OVERLAY_ROOT="$PWD/docs/product_launch/media_pack/ph_ready/overlays"
ffmpeg -nostdin -y -v error -loop 1 -i "$OVERLAY_ROOT/title-real-repo-45s.png" \
  -t 4 -r 30 -c:v libx264 -pix_fmt yuv420p "$OVERLAY_ROOT/title-real-repo-45s.mp4"
ffmpeg -nostdin -y -v error -loop 1 -i "$OVERLAY_ROOT/cta-real-repo-45s.png" \
  -t 5 -r 30 -c:v libx264 -pix_fmt yuv420p "$OVERLAY_ROOT/cta-real-repo-45s.mp4"
ffmpeg -nostdin -y -v error -loop 1 -i "$OVERLAY_ROOT/title-real-repo-90s.png" \
  -t 7 -r 30 -c:v libx264 -pix_fmt yuv420p "$OVERLAY_ROOT/title-real-repo-90s.mp4"
ffmpeg -nostdin -y -v error -loop 1 -i "$OVERLAY_ROOT/cta-real-repo-90s.png" \
  -t 6 -r 30 -c:v libx264 -pix_fmt yuv420p "$OVERLAY_ROOT/cta-real-repo-90s.mp4"
```

Require 120, 150, 210, and 180 frames respectively. Decode every plate and sample first/middle/last frames externally.

- [ ] **Step 5: persist notebook outputs and verify**

Run both cells, call `notebook_open` on the same path to flush the in-memory buffer, reload from disk, and confirm the cells and outputs survive. Then run:

```bash
python3 -m json.tool docs/product_launch/media_pack/product-hunt-media-pack.ipynb >/dev/null
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git diff --check
```

- [ ] **Step 6: commit the notebook proof source**

```bash
git add docs/product_launch/media_pack/product-hunt-media-pack.ipynb \
  docs/product_launch/media_pack/proof-manifest.json
git commit -m "docs(product-launch): D4.x add real campaign proof review"
```

## Task 6: Build `Real Repository Loop — 45s v2` in Palmier Pro

**Files:**

- External mutation: `/Users/kevintruong/Documents/Palmier Pro/SPUR Product Hunt Hero - Real TUI.palmier`
- Read source: `docs/product_launch/media_pack/proof-manifest.json`
- Read media: promoted campaign, v2 narration, approved music, v2 plates/rails

- [ ] **Step 1: preserve and verify the Palmier project**

Use Palmier MCP to open the existing project. Record project ID, baseline timeline ID, all timeline names, and the baseline picture clip IDs. Stop if `Real Repository Loop — 45s v2` already exists. Create a recoverable project backup outside the repository before the first mutation.

- [ ] **Step 2: import and inspect every required media item**

Import:

- `live_demos/16-live-product-hunt-audit-loop.mp4`
- `ph_ready/audio/narration-real-repo-45s.wav`
- `ph_ready/audio/music-45s.wav`
- `ph_ready/overlays/title-real-repo-45s.mp4`
- `ph_ready/overlays/cta-real-repo-45s.mp4`
- five `real-repo-45-progress-*.png` files

Call `inspect_media` on each returned media ID. Require correct resolution/duration and reject missing or black plate sources based on external decode, not Palmier preview alone.

- [ ] **Step 3: create the exact 1,350-frame picture spine**

Create and activate `Real Repository Loop — 45s v2`, then set 1920×1080, 30 fps. Add picture clips on one track at:

| Frames | Source |
|---:|---|
| 0–120 | title plate |
| 120–330 | campaign `prompt` window |
| 330–570 | campaign `plan` window |
| 570–810 | campaign `workers` window |
| 810–1020 | campaign `approve_clean` window |
| 1020–1200 | campaign `synthesis` window |
| 1200–1350 | CTA plate |

For each campaign clip, convert manifest seconds to source frames with `round(seconds × 30)`. Set speed so the complete source window fills its assigned timeline duration. Allow 0.75–1.25× only; if a window requires more, revise the manifest window or use a final-frame hold rather than accelerating proof.

- [ ] **Step 4: add cues, rail, and restrained motion**

Add upper-left cues over the five product beats:

```text
01 / BRAIN      ASK IN THE REAL REPOSITORY
02 / PLAN       THREE TASKS, FULLY VISIBLE
03 / WORKERS    STATE AND EVIDENCE STAY VISIBLE
04 / APPROVE    THE OPERATOR CLOSES THE LOOP
05 / SYNTHESIS  APPROVED FINDINGS RETURN TO BRAIN
```

Add the matching rail plate for each beat at the locked bottom rail position. Keep repository footer, task identity, state, evidence, and review action unobscured. Apply 100→106% pushes only after first-frame proof is legible.

- [ ] **Step 5: add audio, captions, and ducking**

Add music for frames 0–1350 and narration at frame 0. Set music keyframes:

```text
0:0.05, 60:0.10, 120:0.14, 1050:0.14, 1200:0.08, 1320:0.04, 1349:0.00
```

Generate English captions with max seven words, animation off, centerX=0.5, centerY=0.84, and the approved style. Correct every caption so concatenated words equal the approved 85-word script ignoring punctuation and case.

- [ ] **Step 6: inspect proof frames and timeline invariants**

Inspect frames 60, 225, 450, 690, 915, 1110, and 1275 plus every transition boundary. Require 1,350 frames, 1920×1080 at 30 fps, legible cues/captions, visible real repository identity, and no changed archival timeline clip IDs.

No git commit: the Palmier project and generated media are external/ignored.

## Task 7: Build `Real Repository Loop — 90s v2` in Palmier Pro

**Files:**

- External mutation: same Palmier project
- Read source: same campaign manifest and media pack

- [ ] **Step 1: create a separate exact 2,700-frame timeline**

Require that `Real Repository Loop — 90s v2` does not already exist. Create it, activate it, and set 1920×1080 at 30 fps.

- [ ] **Step 2: add the complete picture spine**

| Frames | Source |
|---:|---|
| 0–210 | 90s title plate |
| 210–510 | `prompt` window |
| 510–840 | `plan` window |
| 840–1230 | `workers` window |
| 1230–1590 | `reject` window |
| 1590–1950 | `retry` window |
| 1950–2250 | `approve_retry` window |
| 2250–2520 | `synthesis` window |
| 2520–2700 | 90s CTA plate |

Use the same 0.75–1.25× rule and final-frame holds. Never start a beat before its required proof is visible.

- [ ] **Step 3: add seven proof cues and rails**

```text
01 / BRAIN      ASK IN THE REAL REPOSITORY
02 / PLAN       THREE READ-ONLY DEEP DIVES
03 / WORKERS    AGENT · MODEL · EFFORT · ATTEMPT
04 / REJECT     ASK FOR THE MISSING SOURCE PROOF
05 / RETRY      THE SAME TASK RETURNS WITH EVIDENCE
06 / APPROVE    HUMAN REVIEW CLOSES THE TASK
07 / SYNTHESIS  ONLY APPROVED FINDINGS RETURN
```

Use one cue and one caption sentence per beat. Preserve all action/state markers.

- [ ] **Step 4: add audio, captions, and ducking**

Add `music-90s.wav` for frames 0–2700 and `narration-real-repo-90s.wav` at frame 0. Use music keyframes:

```text
0:0.05, 90:0.10, 210:0.14, 2250:0.14, 2520:0.08, 2640:0.04, 2699:0.00
```

Generate and correct captions against the exact 179-word script. No caption may cover plan/task identity, worker evidence, review decisions, or the repository footer.

- [ ] **Step 5: inspect proof frames and preservation**

Inspect frames 105, 360, 675, 1035, 1410, 1770, 2100, 2385, and 2610 plus every boundary. Require 2,700 frames, correct beat order, full HITL correlation, and unchanged archival timeline IDs.

No git commit.

## Task 8: Export, verify, record hashes, and hand off

**Files:**

- Generate (ignored): `docs/product_launch/media_pack/ph_ready/hero-video-real-repo-45s-v2.mp4`
- Generate (ignored): `docs/product_launch/media_pack/ph_ready/hero-video-real-repo-90s-v2.mp4`
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Modify: `docs/product_launch/media_pack/MANIFEST.md`

- [ ] **Step 1: export by exact timeline ID to versioned filenames**

Use Palmier `export_timeline` for each new timeline ID. Set H.264, 1920×1080, 30 fps, AAC stereo 48 kHz, and `overwrite=false`. Poll each export job to completion. If a target exists, compare hashes and stop rather than overwrite.

- [ ] **Step 2: enforce exact external media gates**

```bash
PACK_ROOT="$PWD/docs/product_launch/media_pack"
HERO45="$PACK_ROOT/ph_ready/hero-video-real-repo-45s-v2.mp4"
HERO90="$PACK_ROOT/ph_ready/hero-video-real-repo-90s-v2.mp4"
for video in "$HERO45" "$HERO90"; do
  ffprobe -v error -count_frames \
    -show_entries format=duration,size \
    -show_entries stream=codec_name,width,height,r_frame_rate,nb_read_frames,sample_rate,channels \
    -of json "$video"
  ffmpeg -nostdin -v error -i "$video" -f null -
  shasum -a 256 "$video"
done
```

Require:

- 45s: H.264, 1920×1080, 30 fps, exactly 1,350 decoded frames, 45.000 seconds.
- 90s: H.264, 1920×1080, 30 fps, exactly 2,700 decoded frames, 90.000 seconds.
- Both: AAC stereo 48 kHz and full decode success.

- [ ] **Step 3: inspect actual-export contact sheets through Notebook MCP**

Append one code cell that extracts boundary/midpoint frames from both exported files into a temporary directory, embeds them in two CSS-radio panels, and labels every frame with timeline time, beat, and expected proof. Run it, flush the notebook with `notebook_open`, reload from disk, and require all outputs to survive.

The 45s sample frames are `0, 60, 120, 225, 330, 450, 570, 690, 810, 915, 1020, 1110, 1200, 1275, 1349`. The 90s sample frames are `0, 105, 210, 360, 510, 675, 840, 1035, 1230, 1410, 1590, 1770, 1950, 2100, 2250, 2385, 2520, 2610, 2699`.

- [ ] **Step 4: record final output hashes in the manifest**

Use `apply_patch` to add `.real_campaign.outputs.hero_45` and `.real_campaign.outputs.hero_90`. The file, duration, and frame fields are respectively `ph_ready/hero-video-real-repo-45s-v2.mp4` / `45` / `1350` and `ph_ready/hero-video-real-repo-90s-v2.mp4` / `90` / `2700`. Copy each lowercase SHA-256 from Step 2 directly into its `sha256` field in that same patch. Before staging, require both hashes to match `^[0-9a-f]{64}$` and recompute them from the two export files.

Extend `media-contract.test.sh` to recompute each output hash and enforce its declared duration/frame count. No descriptive or sentinel string may ever be valid manifest data.

- [ ] **Step 5: update the handoff manifest**

In `MANIFEST.md`, mark both v2 videos as the Product Hunt deliverables, link them to `.real_campaign`, list the exact source capture ID and narration/music identities, and explicitly label the earlier montage exports as archival.

- [ ] **Step 6: run final repository and media verification**

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
python3 -m json.tool docs/product_launch/media_pack/product-hunt-media-pack.ipynb >/dev/null
git diff --check
git status --short
```

Expected: all contracts pass, notebook JSON is valid, no whitespace errors, and only the manifest/notebook/handoff documentation intended for the final commit is modified.

- [ ] **Step 7: commit final provenance**

```bash
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/product-hunt-media-pack.ipynb \
  docs/product_launch/media_pack/MANIFEST.md \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "docs(product-launch): D4.y record real hero exports"
```

- [ ] **Step 8: apply verification-before-completion and reconcile beads**

Re-run the exact final commands after the commit and record:

- capture/source SHA-256;
- 45s/90s output SHA-256;
- duration/frame/audio facts;
- actual-export visual review verdict;
- archival baseline/timeline preservation check;
- clean worktree status.

Only then add the completion/approval audit comments, process scope signal `aa86bf1f-b243-4de5-b87a-bc024e25ef1d`, remove the generic `signal:scope-drift` label when no unprocessed scope signal remains, and close the revised capture/edit/export tasks in dependency order.

## Completion checklist

- [ ] One real `spur` campaign supplies both cuts.
- [ ] The plan is populated with three correlated read-only tasks.
- [ ] Gemini, Claude Code, and Codex results are visible and attributable.
- [ ] The 90s cut proves Reject → Retry → Approve on the same task.
- [ ] Both cuts show final brain synthesis in the originating session.
- [ ] The exact ACP-compatible positioning is present.
- [ ] `beta.otobank.com` and `COMMUNITY FREE` are present on both CTA plates.
- [ ] Narration matches the approved 85/179-word scripts.
- [ ] Both actual exports pass exact media and visual gates.
- [ ] Prior captures, timelines, and exports remain intact and labeled archival.
- [ ] Notebook interactive proof review persists after disk reload.
- [ ] Beads and git records describe the same final state.
