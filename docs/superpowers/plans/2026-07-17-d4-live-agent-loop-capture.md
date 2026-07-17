# D4 Live Agent-Loop Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a separately gated, real SPUR TUI capture that shows a human rejecting, retrying, and approving a worker result before requesting final brain synthesis in the same session.

**Architecture:** Extend the existing `problem-plan-loop-drive` story instead of creating a duplicate journey. Keep the observe-only and minimal `SPUR_DEMO_ALLOW_PLAN_LOOP` paths unchanged; add `SPUR_DEMO_ALLOW_HITL_LOOP` for the higher-spend two-attempt path, and parameterize the existing live-capture wrapper so a tiny dedicated D4 wrapper can reuse its cast/GIF/MP4 pipeline with a distinct output stem.

**Tech Stack:** Bash, shell-use, SPUR TUI, asciinema cast, agg, ffmpeg, repository static story contracts.

---

## File structure

- Modify `scripts/e2e/demos/tui-live/story-contract.test.sh`: static red/green contract for D4 gating, proof order, read-only prompts, synthesis, and capture wrapper.
- Modify `scripts/e2e/demos/tui-live/lib.sh`: D4 opt-in guard and the real reject/retry/approve/synthesize helper.
- Modify `scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh`: route the new D4 gate before the existing minimal seed branch.
- Modify `scripts/e2e/demos/tui-live/capture-live-seed.sh`: allow an explicit gate and stable-output prefix without duplicating conversion logic.
- Create `scripts/e2e/demos/tui-live/capture-live-hitl.sh`: small D4-specific wrapper that selects the new gate and output stem.
- Modify `scripts/e2e/demos/tui-live/README.md`: document the higher-spend real HITL path and its proof contract.
- Modify `scripts/e2e/demos/tui-live/PROBLEM_STORIES.md`: add D4 proof anchors and distinguish minimal seed from continuous HITL.

The generated `out/` cast, log, GIF, and MP4 remain ignored runtime evidence. They are not committed or promoted into the media pack by this plan.

### Task 1: Add the failing D4 static story contract

**Files:**
- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Add assertions for the new D4 contract**

Append these assertions before the final failure check:

```bash
hitl_capture="$root/capture-live-hitl.sh"
plan_loop="$root/journeys/problem-plan-loop-drive.sh"

assert_has "$lib" 'require_hitl_loop_opt_in() {' \
  'D4 HITL loop has a dedicated spend guard'
assert_has "$lib" 'seed_task_id="demo-hitl-$$"' \
  'D4 HITL loop uses a per-run correlation tag'
assert_has "$lib" 'D4 FINDING:' \
  'D4 first attempt asks for a visible evidence marker'
assert_has "$lib" 'Make no file changes.' \
  'D4 worker prompts remain read-only'
assert_has "$lib" 'Decision: Reject' \
  'D4 captures the real reject confirmation'
assert_has "$lib" 'Retry Task' \
  'D4 captures the real retry confirmation'
assert_has "$lib" 'SOURCE:' \
  'D4 retry requests exact source evidence'
assert_has "$lib" 'RECOMMENDATION:' \
  'D4 retry requests a recommendation'
assert_has "$lib" 'Decision: Approve' \
  'D4 captures the real approve confirmation'
assert_has "$lib" 'D4 SYNTHESIS:' \
  'D4 final brain response has a deterministic marker'
assert_has "$lib" 'Do not call tools or delegate.' \
  'D4 synthesis forbids another delegation'
assert_matches "$lib" '(?s)press_key d.*Decision: Reject.*press_key R.*Retry Task.*press_key a.*Decision: Approve' \
  'D4 review actions stay reject then retry then approve'
assert_matches "$plan_loop" '(?s)SPUR_DEMO_ALLOW_HITL_LOOP.*trigger_submit_plan_hitl_review_and_synthesize.*SPUR_DEMO_ALLOW_PLAN_LOOP.*trigger_submit_plan_one_task_and_observe' \
  'D4 branch precedes the existing minimal plan-loop branch'
assert_has "$hitl_capture" 'SPUR_DEMO_ALLOW_HITL_LOOP=1' \
  'D4 capture wrapper enables only the HITL gate'
assert_has "$hitl_capture" 'SPUR_DEMO_CAPTURE_STEM_PREFIX=15-live-hitl-agent-loop' \
  'D4 capture wrapper uses a distinct stable output stem'
assert_has "$root/capture-live-seed.sh" 'SPUR_DEMO_CAPTURE_STEM_PREFIX' \
  'shared live capture supports an explicit output stem'
assert_has "$root/capture-live-seed.sh" '"$OUT/${stem_prefix}.log"' \
  'shared live capture publishes a stable audit log'
```

- [ ] **Step 2: Run the contract and verify RED**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: exit `1`; existing assertions pass, and the new assertions fail because `require_hitl_loop_opt_in`, `demo-hitl`, the D4 review sequence, and `capture-live-hitl.sh` do not exist.

- [ ] **Step 3: Commit the failing contract**

```bash
git add scripts/e2e/demos/tui-live/story-contract.test.sh
git commit -m "test(tui-live): D4.d require real HITL loop capture"
```

### Task 2: Implement the gated D4 journey and shared capture mode

**Files:**
- Modify: `scripts/e2e/demos/tui-live/lib.sh`
- Modify: `scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh`
- Modify: `scripts/e2e/demos/tui-live/capture-live-seed.sh`
- Create: `scripts/e2e/demos/tui-live/capture-live-hitl.sh`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Add the dedicated D4 spend guard**

Place this beside `require_plan_loop_opt_in` in `lib.sh`:

```bash
require_hitl_loop_opt_in() {
  if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" != "1" ]]; then
    cat >&2 <<'EOF'
error: live D4 HITL loop is opt-in (real brain + up to two worker attempts).

  SPUR_DEMO_ALLOW_HITL_LOOP=1 bash journeys/problem-plan-loop-drive.sh

Recommended capture wrapper: ./capture-live-hitl.sh
Optional: SPUR_DEMO_PLAN_LOOP_WAIT_S=300
EOF
    return 2
  fi
}
```

- [ ] **Step 2: Add the real D4 helper**

Add this after `trigger_submit_plan_one_task_and_observe` in `lib.sh`:

```bash
trigger_submit_plan_hitl_review_and_synthesize() {
  require_hitl_loop_opt_in
  local seed_task_id="demo-hitl-$$"

  land_session_detail "Attach Session Detail for the D4 HITL loop" 2.5
  sleep_ms 0.8
  printf '+ D4 seed: ask brain for one read-only deep-dive task %s\n' "$seed_task_id"

  type_slow "D4 LIVE CAPTURE. "
  type_text "Call submit_plan with exactly ONE task. "
  type_text "Task id: ${seed_task_id}. Worker: codex. deps: none. "
  type_text "Prompt: Read scripts/e2e/demos/tui-live/PROBLEM_STORIES.md and "
  type_text "identify one evidence gap in problem-plan-loop-drive. "
  type_text "Return exactly one line beginning D4 FINDING:. Make no file changes. "
  type_text "After submit_plan succeeds, reply with plan_id only."
  sleep_ms 0.5
  press_key Enter

  story_hard_proof \
    "The human prompt is correlated to this D4 run" \
    "$seed_task_id" 2.5
  story_hard_proof \
    "The brain accepts the D4 turn in Session Detail" \
    "THINK" 2.5

  press_key Alt+p
  story_hard_proof \
    "The correlated worker result reaches human review" \
    "awaiting_review" 4.0

  press_key d
  story_hard_proof \
    "The human rejects the incomplete first result" \
    "Decision: Reject" 3.5
  press_key Enter
  story_hard_proof \
    "The rejected task becomes eligible for another attempt" \
    "rejected" 3.0

  press_key R
  story_hard_proof \
    "The human opens the retry instruction surface" \
    "Retry Task" 3.0
  type_slow "READ ONLY. Add exactly two lines: SOURCE: <exact path> and "
  type_text "RECOMMENDATION: <one sentence>. Make no file changes."
  story_hard_proof \
    "The retry visibly carries stronger evidence requirements" \
    "SOURCE:" 2.5
  press_key Enter

  story_hard_proof \
    "The improved second attempt returns to human review" \
    "awaiting_review" 4.0
  press_key a
  story_hard_proof \
    "The human approves the improved evidence" \
    "Decision: Approve" 3.5
  press_key Enter
  story_hard_proof \
    "The correlated task records the approval" \
    "approved" 3.0

  return_to_session_detail
  story_session_land "The same brain session remains the operator home" 2.5
  type_slow "D4 HITL COMPLETE. Synthesize the approved worker evidence. "
  type_text "Begin with a marker made from D4, one space, SYNTHESIS, then a colon. "
  type_text "Follow the marker with one sentence. "
  type_text "Do not call tools or delegate."
  sleep_ms 0.5
  press_key Enter
  story_hard_proof \
    "The brain synthesizes approved evidence in the same session" \
    "D4 SYNTHESIS:" 4.0
}
```

Keep every proof in this helper hard. Do not replace missing D4 state with `story_soft_proof`.

- [ ] **Step 3: Route the D4 branch before the minimal seed**

Replace the opt-in branch in `journeys/problem-plan-loop-drive.sh` with:

```bash
if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "D4 live loop: deep dive, reject, retry, approve, then synthesize."
  trigger_submit_plan_hitl_review_and_synthesize
elif [[ "${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in seed: submit one safe task and watch DELEGATE/Done in this session."
  trigger_submit_plan_one_task_and_observe
elif [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in light kick: wake the brain, then re-check workers in session."
  trigger_brain_for_loop_observation
else
  printf '+ safe default: observe only; no brain turn or worker spend\n'
  printf '  SPUR_DEMO_ALLOW_HITL_LOOP=1 → D4 reject/retry/approve loop\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_LOOP=1 → 1-task submit_plan + session wait\n'
  printf '  SPUR_DEMO_ALLOW_AGENT_SEND=1 → light brain kick only\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_START=1 → Start/Resume selected plan\n'
fi
```

- [ ] **Step 4: Parameterize the existing live capture wrapper**

In `capture-live-seed.sh`, replace the unconditional plan-loop export with:

```bash
if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" != "1" ]]; then
  export SPUR_DEMO_ALLOW_PLAN_LOOP=1
fi
export SPUR_DEMO_ALLOW_HITL_LOOP="${SPUR_DEMO_ALLOW_HITL_LOOP:-0}"
export SPUR_DEMO_CAPTURE_STEM_PREFIX="${SPUR_DEMO_CAPTURE_STEM_PREFIX:-14-live-plan-loop-seed}"
```

Replace the hard-coded stem with:

```bash
stem_prefix="$SPUR_DEMO_CAPTURE_STEM_PREFIX"
stem="${stem_prefix}-${stamp}"
```

Replace each stable-copy target `14-live-plan-loop-seed.<ext>` with `${stem_prefix}.<ext>`. Also replace the final summary glob `"$OUT"/14-live-plan-loop-seed*` with `"$OUT"/${stem_prefix}*` so the report follows the selected capture mode. Extend the startup summary with:

```bash
echo "SPUR_DEMO_ALLOW_HITL_LOOP: $SPUR_DEMO_ALLOW_HITL_LOOP"
echo "stable output stem:         $stem_prefix"
```

After the journey finishes, publish the timestamped log to the stable stem:

```bash
cp -p "$log" "$OUT/${stem_prefix}.log"
```

- [ ] **Step 5: Add the dedicated D4 wrapper**

Create executable `scripts/e2e/demos/tui-live/capture-live-hitl.sh`:

```bash
#!/usr/bin/env bash
# Capture the higher-spend D4 human-in-the-loop plan story.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_DEMO_ALLOW_HITL_LOOP=1
export SPUR_DEMO_ALLOW_PLAN_LOOP=0
export SPUR_DEMO_CAPTURE_STEM_PREFIX=15-live-hitl-agent-loop
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-300}"
exec "$ROOT/capture-live-seed.sh"
```

Run:

```bash
chmod +x scripts/e2e/demos/tui-live/capture-live-hitl.sh
```

- [ ] **Step 6: Run the static contract and verify GREEN**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: exit `0` and final line `All story-contract checks passed`.

- [ ] **Step 7: Run shell syntax checks**

Run:

```bash
bash -n scripts/e2e/demos/tui-live/lib.sh \
  scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh \
  scripts/e2e/demos/tui-live/capture-live-seed.sh \
  scripts/e2e/demos/tui-live/capture-live-hitl.sh
```

Expected: exit `0` with no output.

- [ ] **Step 8: Commit the implementation**

```bash
git add scripts/e2e/demos/tui-live/lib.sh \
  scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh \
  scripts/e2e/demos/tui-live/capture-live-seed.sh \
  scripts/e2e/demos/tui-live/capture-live-hitl.sh
git commit -m "feat(tui-live): D4.e capture real HITL agent loop"
```

### Task 3: Document the real D4 capture contract

**Files:**
- Modify: `scripts/e2e/demos/tui-live/README.md`
- Modify: `scripts/e2e/demos/tui-live/PROBLEM_STORIES.md`
- Test: `scripts/e2e/demos/tui-live/story-contract.test.sh`

- [ ] **Step 1: Update README commands and safety language**

Add this command under the plan-loop section:

```bash
# LIVE D4: deep dive → reject → retry with evidence → approve → brain synthesis
# Real brain turn + up to two worker attempts.
./capture-live-hitl.sh
# → out/15-live-hitl-agent-loop.{cast,gif,mp4,log}
```

Document that `SPUR_DEMO_ALLOW_HITL_LOOP=1` is a separate higher-spend gate, uses real Plan Inspector decisions, and fails rather than soft-passing when HITL proof is absent.

- [ ] **Step 2: Update the problem-story proof anchors**

In `PROBLEM_STORIES.md`, extend `problem-plan-loop-drive` with:

```markdown
Optional D4 live proof adds `awaiting_review`, `Decision: Reject`, `Retry Task`,
`Decision: Approve`, and `D4 SYNTHESIS:`. The higher-spend D4 branch is separate
from the minimal one-task seed and remains opt-in.
```

- [ ] **Step 3: Run documentation and static contracts**

Run:

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash -n scripts/e2e/demos/tui-live/*.sh \
  scripts/e2e/demos/tui-live/journeys/*.sh
```

Expected: both commands exit `0`; story contract ends with `All story-contract checks passed`.

- [ ] **Step 4: Commit the documentation**

```bash
git add scripts/e2e/demos/tui-live/README.md \
  scripts/e2e/demos/tui-live/PROBLEM_STORIES.md
git commit -m "docs(tui-live): D4.f document real HITL capture"
```

### Task 4: Verify safe-default behavior before spending

**Files:**
- No tracked file changes.
- Runtime outputs: ignored shell-use cache/cast and `scripts/e2e/demos/tui-live/out/`.

- [ ] **Step 1: Confirm the dedicated guard rejects an unapproved direct call**

Run a shell that sources `lib.sh` and calls only the guard without the flag:

```bash
bash -c 'source scripts/e2e/demos/tui-live/lib.sh; require_hitl_loop_opt_in'
```

Expected: exit `2`; message says the D4 HITL loop is opt-in and may use up to two worker attempts.

- [ ] **Step 2: Run the safe-default story**

Run:

```bash
SPUR_DEMO_STORY_PACE=0 \
  bash scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh
```

Expected: exit `0`; output includes `safe default: observe only`; no D4 or minimal seed is sent.

- [ ] **Step 3: Re-run the static contract**

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: exit `0` and `All story-contract checks passed`.

### Task 5: Run and audit the authorized real capture

**Files:**
- No tracked source changes unless runtime evidence exposes a tested defect.
- Runtime outputs: `scripts/e2e/demos/tui-live/out/15-live-hitl-agent-loop*`.

- [ ] **Step 1: Run the approved D4 capture**

Run:

```bash
cd scripts/e2e/demos/tui-live
SPUR_DEMO_PLAN_LOOP_WAIT_S=300 ./capture-live-hitl.sh
```

Expected: the wrapper prints `SPUR_DEMO_ALLOW_HITL_LOOP: 1`, uses stable output stem `15-live-hitl-agent-loop`, and the journey exits `0`.

- [ ] **Step 2: Verify proof ordering in the capture log**

Run:

```bash
rg -n 'demo-hitl-|awaiting_review|Decision: Reject|Retry Task|Decision: Approve|D4 SYNTHESIS:' \
  out/15-live-hitl-agent-loop*.log
```

Expected: at least one match for every marker, in the listed order, all tied to the same run log.

- [ ] **Step 3: Verify runtime artifacts**

Run:

```bash
ls -lh out/15-live-hitl-agent-loop.{cast,log}
ffprobe -v error -select_streams v:0 \
  -show_entries stream=codec_name,width,height \
  -of default=noprint_wrappers=1 out/15-live-hitl-agent-loop.mp4
```

Expected: cast and log exist; if conversion dependencies are available, MP4 is H.264 with even dimensions and is playable. Absence of GIF/MP4 is non-fatal only when the log explicitly reports the missing conversion dependency; the cast remains mandatory.

- [ ] **Step 4: Visually review the film before media-pack ingestion**

Review the MP4 at 1× speed and confirm:

- the task correlation ID is readable;
- Reject, Retry, and Approve confirmations are each visible long enough to read;
- the second attempt is distinguishable from the first;
- final brain synthesis occurs in the same Session Detail;
- no file-changing action or unsupported claim appears.

Do not update `proof-manifest.json` in this task. If the film passes visual review, create a follow-up media-ingestion plan using the actual file checksum, timestamps, and crops.

### Task 6: Final source verification

**Files:**
- All files from Tasks 1–3.

- [ ] **Step 1: Run all scoped checks**

```bash
bash scripts/e2e/demos/tui-live/story-contract.test.sh
bash -n scripts/e2e/demos/tui-live/lib.sh \
  scripts/e2e/demos/tui-live/capture-live-seed.sh \
  scripts/e2e/demos/tui-live/capture-live-hitl.sh \
  scripts/e2e/demos/tui-live/journeys/problem-plan-loop-drive.sh
git diff --check
```

Expected: story contract passes, syntax checks are silent, and `git diff --check` emits no errors.

- [ ] **Step 2: Confirm only intended commits/files**

```bash
git status --short
git log -5 --oneline
```

Expected: the D4 plan/test/implementation/docs commits are present. Runtime `out/` artifacts are ignored. Pre-existing unrelated worktree changes remain untouched.

## Plan self-review

- Spec coverage: the plan covers the separate spend gate, read-only two-attempt task, real Reject/Retry/Approve controls, same-session synthesis, static contract, safe-default regression, runtime capture, and visual audit.
- Scope: media-pack ingestion and Open Design artifact rendering remain a follow-up because their exact checksum, timestamps, and crops cannot be known before the real capture exists.
- Type/name consistency: the plan uses `SPUR_DEMO_ALLOW_HITL_LOOP`, `trigger_submit_plan_hitl_review_and_synthesize`, `SPUR_DEMO_CAPTURE_STEM_PREFIX`, and `15-live-hitl-agent-loop` consistently.
