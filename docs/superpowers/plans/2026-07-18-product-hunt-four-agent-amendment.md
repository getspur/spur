# Product Hunt Four-Agent Campaign Amendment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the failed Gemini campaign path with one verified four-agent real-repository campaign using Claude Code, Grok, Codex, and OpenCode, then resume the 45-second and 90-second Product Hunt media workflow from that source.

**Architecture:** Preserve the existing diagnostic captures and the hardened fresh-session/result-provenance helpers. Amend only the campaign contract: four independent read-only tasks, a `Workers (4)` proof, four operator approvals, and one Grok Reject → Retry → Approve path. A separate bounded authentication preflight must prove all four worker transports before the paid capture begins.

**Tech Stack:** Bash + shell-use/asciinema, SPUR TUI/MCP/beads, Notebook MCP/Jupyter, jq, ffmpeg/ffprobe, Higgsfield TTS, Palmier Pro MCP, Git.

---

## Scope and precedence

- Approved amended design: `docs/superpowers/specs/2026-07-18-product-hunt-real-repository-hero-redesign.md`
- Base plan: `docs/superpowers/plans/2026-07-18-product-hunt-real-repository-hero.md`
- This amendment supersedes the base plan wherever it says three tasks, Gemini, `Workers (3)`, three findings, or three approvals.
- Existing commits through `84323249f` remain valid foundations. In particular, keep fresh-session isolation, bounded Workers-panel extraction, normalized Output proof, 420-second waits, full-fidelity capture, and fail-closed synthesis return.
- Diagnostic captures `20260718T053244Z` and `20260718T054533Z` remain archival and non-promotable.
- New four-agent capture stem: `17-live-product-hunt-four-agent-loop`. Do not reuse the diagnostic `16-live-product-hunt-audit-loop` stable filenames.
- Never run another paid capture until Task 4 passes for all four agents.

## File responsibility map

| File | Responsibility |
|---|---|
| `scripts/e2e/demos/tui-live/story-contract.test.sh` | Four-task identities, routing, review order, provenance, and synthesis contract |
| `scripts/e2e/demos/tui-live/lib.sh` | Real four-agent prompt, Workers proof, HITL sequence, and correlated synthesis |
| `scripts/e2e/demos/tui-live/capture-live-hitl.sh` | New non-colliding four-agent output stem and full-fidelity wrapper |
| `scripts/e2e/demos/tui-live/README.md` | Operator-facing four-agent capture instructions and stop conditions |
| `scripts/e2e/demos/tui-live/PROBLEM_STORIES.md` | Human-readable four-agent user journey |
| `docs/product_launch/media_pack/tests/media-contract.test.sh` | Notebook copy and promoted-campaign runtime contract |
| `docs/product_launch/media_pack/product-hunt-media-pack.ipynb` | Interactive four-agent proof/copy review source of truth |
| `docs/product_launch/media_pack/proof-manifest.json` | Four-agent source markers, hashes, truth windows, and deliverable provenance |

## Task 1: Write the failing four-agent TUI contract

**Files:**

- Modify: `scripts/e2e/demos/tui-live/story-contract.test.sh:211-320`

- [ ] **Step 1: add the fourth correlation identity and remove Gemini acceptance**

Require these exact declarations and prompt fragments:

```bash
assert_has "$lib" 'local handoff_task_id="ph-media-handoff-$$"' \
  'PH audit declares the media handoff task correlation id'
assert_has "$lib" 'Call submit_plan with exactly FOUR independent read-only tasks.' \
  'PH audit requests a populated four-task plan'
assert_has "$lib" 'Worker: claude-code.' \
  'PH audit routes ACP positioning to Claude Code'
assert_has "$lib" 'Worker: grok.' \
  'PH audit routes TUI proof to Grok'
assert_has "$lib" 'Worker: codex.' \
  'PH audit routes launch readiness to Codex'
assert_has "$lib" 'Worker: opencode.' \
  'PH audit routes media handoff to OpenCode'
assert_has "$lib" 'PH HANDOFF FINDING:' \
  'PH audit exposes the handoff result before approval'
assert_count_at_least "$lib" 'effort: medium.' 4 \
  'PH audit requests visible effort for all four workers'
assert_not_matches "$lib" 'Worker: gemini\.|Workers \(3\)' \
  'PH audit cannot fall back to the failed Gemini story'
assert_has "$hitl_capture" 'SPUR_DEMO_CAPTURE_STEM_PREFIX=17-live-product-hunt-four-agent-loop' \
  'PH capture uses a new four-agent stem instead of overwriting diagnostics'
```

- [ ] **Step 2: require bounded Workers-panel proof for exactly four agents**

Add a structural assertion requiring this order before `press_key Alt+d` collapses the panel:

```bash
story_workers_panel_hard_proof "The session exposes exactly four campaign workers" "Workers (4)" 2.5
story_workers_panel_hard_proof "The positioning task is routed to Claude Code" "claude-code" 2.5
story_workers_panel_hard_proof "The proof task is routed to Grok" "grok" 2.5
story_workers_panel_hard_proof "The readiness task is routed to Codex" "codex" 2.5
story_workers_panel_hard_proof "The handoff task is routed to OpenCode" "opencode" 2.5
```

The assertion must reject global `story_hard_proof` for these five anchors.

- [ ] **Step 3: require all four reviews before synthesis**

Extend the campaign-order assertion to require:

```text
positioning finding → Approve
proof finding → Reject → Retry → SOURCE/WINDOW/RECOMMENDATION → Approve
readiness finding → Approve
handoff finding → Approve
return to the originating Session Detail → correlated synthesis
```

The handoff result must use `story_plan_inspector_result_hard_proof` with `PH HANDOFF FINDING:`. The synthesis request must list `${handoff_task_id}` in addition to the other three IDs, while the marker instruction and final hard proof must preserve the existing run-correlation anchor: `PH AUDIT SYNTHESIS: ${proof_task_id}`.

- [ ] **Step 4: run RED verification**

```bash
bash -n scripts/e2e/demos/tui-live/story-contract.test.sh
bash scripts/e2e/demos/tui-live/story-contract.test.sh
```

Expected: syntax exits 0; the contract exits non-zero only for the new four-agent requirements. Existing fresh-session, Workers-boundary, normalized-Output, capture, and synthesis-return assertions remain green.

- [ ] **Step 5: commit the failing contract**

```bash
git add scripts/e2e/demos/tui-live/story-contract.test.sh
git commit -m "test(tui-live): D4.al require four-agent ACP campaign"
```

## Task 2: Implement the four-agent real-project journey

**Files:**

- Modify: `scripts/e2e/demos/tui-live/lib.sh:1317-1407`
- Modify: `scripts/e2e/demos/tui-live/capture-live-hitl.sh`
- Modify: `scripts/e2e/demos/tui-live/README.md`
- Modify: `scripts/e2e/demos/tui-live/PROBLEM_STORIES.md`

- [ ] **Step 1: add the fourth task and replace Gemini with Grok**

Keep the existing helper structure and add this identity beside the other three:

```bash
local handoff_task_id="ph-media-handoff-$$"
```

Replace the campaign request with exactly four independent tasks:

```bash
type_text "Call submit_plan with exactly FOUR independent read-only tasks. "
type_text "Task 1 id: ${positioning_task_id}. Worker: claude-code. effort: medium. deps: none. "
type_text "Inspect the approved ACP category line vs docs/integration. Return exactly one line beginning PH POSITIONING FINDING:. Make no file changes. "
type_text "Task 2 id: ${proof_task_id}. Worker: grok. effort: medium. deps: none. "
type_text "Inspect real TUI captures for one launch claim needing stronger proof. Return exactly one line beginning PH PROOF FINDING:. Make no file changes. "
type_text "Task 3 id: ${readiness_task_id}. Worker: codex. effort: medium. deps: none. "
type_text "Inspect pacing and accessibility. Return exactly one line beginning PH READINESS FINDING:. Make no file changes. "
type_text "Task 4 id: ${handoff_task_id}. Worker: opencode. effort: medium. deps: none. "
type_text "Inspect manifest locks, filenames, and Product Hunt delivery notes. Return exactly one line beginning PH HANDOFF FINDING:. Make no file changes. "
```

Keep the existing instruction that the brain must leave completed tasks `awaiting_review`.

- [ ] **Step 2: prove the four-worker panel before collapsing it**

Use the five exact `story_workers_panel_hard_proof` calls from Task 1 in this order: `Workers (4)`, `claude-code`, `grok`, `codex`, `opencode`. Do not accept stale/global terminal matches.

- [ ] **Step 3: approve the OpenCode handoff result after readiness**

After readiness records approval, append:

```bash
press_key j
story_hard_proof "The inspector advances to the handoff task" "$handoff_task_id" 2.5
story_hard_proof "The handoff result reaches operator review" "awaiting_review" 4.0
story_plan_inspector_result_hard_proof \
  "The handoff result exposes its finding" "PH HANDOFF FINDING:" 3.5
press_key a
story_hard_proof "The operator selects approval for handoff" "Decision: Approve" 3.5
press_key Enter
story_hard_proof "The handoff task records approval" "approved" 3.0
```

- [ ] **Step 4: correlate synthesis to all four task IDs**

Replace the synthesis request with:

```bash
type_text "Synthesize approved evidence from ${positioning_task_id}, ${proof_task_id}, ${readiness_task_id}, and ${handoff_task_id} in one concise launch-audit paragraph. "
type_text "Begin the response with the words PH AUDIT SYNTHESIS, then a colon, one space, and the proof task id ${proof_task_id}. "
type_text "Do not call tools or delegate."
```

Keep the existing outgoing-prompt self-satisfaction guard and run-correlated synthesis proof.

- [ ] **Step 5: update operator documentation**

Use this exact roster in both documentation files:

```text
The opt-in Product Hunt capture submits four independent read-only tasks in the
real spur project: ACP positioning (Claude Code), TUI proof (Grok), launch
readiness (Codex), and media handoff (OpenCode). The Grok proof task is rejected
once for a missing exact source window, retried with
SOURCE/WINDOW/RECOMMENDATION requirements, and approved. All four findings must
be approved before the correlated PH AUDIT SYNTHESIS appears in the originating
Session Detail. Any worker transport fallback makes the capture non-promotable.
```

- [ ] **Step 6: version the four-agent capture stem**

In `capture-live-hitl.sh`, change only the stem export:

```bash
export SPUR_DEMO_CAPTURE_STEM_PREFIX=17-live-product-hunt-four-agent-loop
```

Keep the existing 420-second wait, full-fidelity flag, idle limit, and direct `exec` unchanged.

- [ ] **Step 7: run GREEN verification**

```bash
bash -n scripts/e2e/demos/tui-live/lib.sh
bash -n scripts/e2e/demos/tui-live/capture-live-hitl.sh
bash -n scripts/e2e/demos/tui-live/story-contract.test.sh
bash scripts/e2e/demos/tui-live/story-contract.test.sh
shellcheck scripts/e2e/demos/tui-live/lib.sh
git diff --check
```

Expected: every command exits 0 and the contract ends with `All story-contract checks passed`.

- [ ] **Step 8: commit the implementation**

```bash
git add scripts/e2e/demos/tui-live/lib.sh \
  scripts/e2e/demos/tui-live/capture-live-hitl.sh \
  scripts/e2e/demos/tui-live/README.md \
  scripts/e2e/demos/tui-live/PROBLEM_STORIES.md
git commit -m "feat(tui-live): D4.am route four working ACP agents"
```

## Task 3: Update the Notebook and copy contract

**Files:**

- Modify: `docs/product_launch/media_pack/tests/media-contract.test.sh`
- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`

- [ ] **Step 1: write the failing Notebook copy assertions**

Extract code/markdown cell sources with jq and require these exact strings. Define `NOTEBOOK="$ROOT/product-hunt-media-pack.ipynb"` beside the existing manifest path first:

```bash
notebook_source="$(jq -r '.cells[].source | if type == "array" then join("") else . end' "$NOTEBOOK")"
for required in \
  'from Claude Code and Codex to Grok, OpenCode, and beyond' \
  'delegates four read-only deep dives' \
  'four read-only deep dives: ACP positioning, TUI proof, launch readiness, and media handoff' \
  'Task 4 · Media handoff'; do
  [[ "$notebook_source" == *"$required"* ]] \
    && pass "four-agent notebook copy: $required" \
    || fail "four-agent notebook copy: $required"
done
[[ "$notebook_source" != *'Kiro, Gemini'* && "$notebook_source" != *'three read-only deep dives'* ]] \
  && pass "notebook removes the superseded three-agent copy" \
  || fail "notebook removes the superseded three-agent copy"
```

- [ ] **Step 2: run RED verification and commit**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

Expected: non-zero only for the new four-agent Notebook copy requirements.

```bash
git add docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "test(product-launch): D4.an require four-agent notebook copy"
```

- [ ] **Step 3: reload and amend the Notebook through Notebook MCP**

Call `notebook_context_pack`, `notebook_open` with the absolute worktree path, and `notebook_reload` before editing. Update only the cells containing the locked positioning, 45-second script, 90-second script, and product-frame task list:

```text
Positioning: SPUR gives any ACP-compatible coding agent—from Claude Code and Codex to Grok, OpenCode, and beyond—one durable outer harness.
45s: delegates four read-only deep dives
90s: four read-only deep dives: ACP positioning, TUI proof, launch readiness, and media handoff
Frame: Task 1 ACP positioning; Task 2 Real TUI proof; Task 3 Launch readiness; Task 4 Media handoff
```

Preserve the approved CTA `beta.otobank.com` / `COMMUNITY FREE`, all existing interactive HTML behavior, and all archival media references.

- [ ] **Step 4: persist, reload, and verify GREEN**

Flush the Notebook MCP buffer, reload the notebook from disk, and require the changed cells and rendered outputs to persist. Then run:

```bash
python3 -m json.tool docs/product_launch/media_pack/product-hunt-media-pack.ipynb >/dev/null
bash docs/product_launch/media_pack/tests/media-contract.test.sh
git diff --check
```

Expected: every command exits 0.

- [ ] **Step 5: commit the Notebook amendment**

```bash
git add docs/product_launch/media_pack/product-hunt-media-pack.ipynb \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "docs(product-launch): D4.ao visualize four-agent campaign"
```

## Task 4: Prove all four authenticated worker paths

**Files:** None. This task mutates only beads/delegation state and records results on `bd-1345r`.

- [ ] **Step 1: verify advertised workers**

Call `list_available_workers` and require exact names `claude-code`, `grok`, `codex`, and `opencode`. A list hit is necessary but not sufficient.

- [ ] **Step 2: create one beads-backed preflight issue per worker**

Create four child tasks under epic `bd-ctdqy`, each labeled `spur:preflight:d4-four-agent` and `spur:agent:<name>`. Each body must require no file changes and this exact one-line output:

```text
PH WORKER READY: <agent-name>
```

Record the four returned issue IDs before dispatch.

- [ ] **Step 3: dispatch the four independent probes**

Call `delegate_parallel` with one task per issue ID. Every task uses the matching agent, `effort: low`, `enable_worker_mcp: false`, and this structure:

```text
CONTEXT: Product Hunt four-agent transport preflight.
GOAL: Prove this configured worker can accept a real SPUR prompt.
CONSTRAINTS: Read-only; make no file changes; do not call tools.
EXPECTED_OUTPUT: Return exactly PH WORKER READY: <agent-name>
```

- [ ] **Step 4: verify results and close the preflight issues**

Require all four delegations to complete without fallback and each result to contain its matching marker. Add a completion audit to each issue, close it, and comment the four delegation IDs on `bd-1345r`.

If any worker returns an authentication, deleted-client, transport, model, or fallback error, stop. Do not run the campaign and do not substitute a different worker without user approval.

## Task 5: Capture and promote one four-agent campaign

**Files:**

- Generate (ignored): `scripts/e2e/demos/tui-live/out/17-live-product-hunt-four-agent-loop.{cast,gif,mp4,log}`
- Modify: `docs/product_launch/media_pack/tests/media-contract.test.sh`
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Modify: `docs/product_launch/media_pack/refresh.sh`

- [ ] **Step 1: run the paid capture once**

Only after Task 4 passes, run from `scripts/e2e/demos/tui-live`:

```bash
SPUR_BIN=/Users/kevintruong/.cargo/bin/spur \
SPUR_DEMO_PROJECT=/Volumes/Projects/spur \
SPUR_DEMO_STORY_PACE=1 \
SPUR_AGG_SPEED=1.15 \
SPUR_DEMO_PLAN_LOOP_WAIT_S=420 \
./capture-live-hitl.sh
```

Expected: exit 0. If the run fails after model spend, preserve timestamped diagnostics and stop; do not retry in the same execution turn.

- [ ] **Step 2: enforce four-agent source gates**

Require non-empty stable cast/GIF/MP4/log, a full external decode, H.264 2560×1600 at 30 fps, and these fixed-string markers in the log:

```text
ph-acp-positioning-
ph-tui-proof-
ph-launch-readiness-
ph-media-handoff-
Workers (4)
claude-code
grok
codex
opencode
PH POSITIONING FINDING:
PH PROOF FINDING:
PH READINESS FINDING:
PH HANDOFF FINDING:
Decision: Reject
Retry Task
SOURCE:
WINDOW:
RECOMMENDATION:
Decision: Approve
PH AUDIT SYNTHESIS:
```

Also reject `gemini`, `deleted_client`, any worker fallback, and any `retry 4/3` campaign row.

- [ ] **Step 3: write the failing promoted-source contract**

Update the `.real_campaign` test to require the exact four-worker array:

```json
["claude-code", "grok", "codex", "opencode"]
```

Require all 20 markers above and the existing eight truth-window keys: `prompt`, `plan`, `workers`, `approve_clean`, `reject`, `retry`, `approve_retry`, `synthesis`. Run the test and verify it fails before changing the manifest.

```bash
git add docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "test(product-launch): D4.ap require four-agent campaign proof"
```

- [ ] **Step 4: promote non-destructively and lock provenance**

Promote the exact `17-live-product-hunt-four-agent-loop` source with `cp -n -p`, verify with `cmp`, calculate SHA-256, measure all eight windows from the actual video, and update `refresh.sh` plus `.real_campaign` with the exact four workers and measured values. If that media-pack destination already exists with different bytes, stop and amend the stem contract before any future capture; never overwrite it.

- [ ] **Step 5: verify and commit**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
ffmpeg -nostdin -v error -i docs/product_launch/media_pack/live_demos/17-live-product-hunt-four-agent-loop.mp4 -f null -
git diff --check
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/refresh.sh \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "docs(product-launch): D4.aq lock four-agent campaign source"
```

## Task 6: Resume media production with four-agent inputs

**Files:**

- Modify: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Modify: `docs/product_launch/media_pack/proof-manifest.json`
- Modify: `docs/product_launch/media_pack/MANIFEST.md`
- Generate (ignored): narration, overlays, Palmier timelines, and v2 exports listed in the base plan

- [ ] **Step 1: regenerate only the amended 85/179-word narration**

Use the exact two scripts from the approved amended design. Require Higgsfield authentication first, keep the Simon voice and existing music identities, and do not time-stretch narration.

- [ ] **Step 2: render the eight measured truth windows in the Notebook**

Reload from disk through Notebook MCP, rerun the proof-window cell against the new SHA-locked campaign, and verify that the Workers frame visibly contains all four agents.

- [ ] **Step 3: build the Palmier timelines without overwriting archival media**

Use the base plan's exact 1,350/2,700-frame spines with these substitutions:

```text
45s cue: 02 / PLAN — FOUR TASKS, FULLY VISIBLE
90s worker beat: Claude Code · Grok · Codex · OpenCode
Narration: approved amended 85/179-word scripts
Source: new four-agent `.real_campaign` SHA and windows
```

- [ ] **Step 4: run final verification and commit provenance**

Require exact durations/frames, AAC stereo 48 kHz, full decode, boundary/midpoint visual review, unchanged archival timeline IDs, Notebook disk persistence, clean media contracts, and SHA-256 for source plus both exports. Then update `MANIFEST.md`, record beads completion/approval audits, and commit:

```bash
git add docs/product_launch/media_pack/proof-manifest.json \
  docs/product_launch/media_pack/product-hunt-media-pack.ipynb \
  docs/product_launch/media_pack/MANIFEST.md \
  docs/product_launch/media_pack/tests/media-contract.test.sh
git commit -m "docs(product-launch): D4.ar record four-agent hero exports"
```

## Completion checklist

- [ ] Claude Code, Grok, Codex, and OpenCode each pass a real authenticated preflight without fallback.
- [ ] One real `spur` campaign contains four correlated read-only tasks.
- [ ] `Workers (4)` and all four agent identities are visible in the bounded current-run panel.
- [ ] Grok's proof task demonstrates Reject → Retry → Approve on the same task.
- [ ] All four findings are approved before synthesis in the originating session.
- [ ] Notebook copy and proof review use the approved four-agent 85/179-word design.
- [ ] The promoted source is SHA-locked, fully decodable, and supplies both cuts.
- [ ] Both actual exports pass exact frame/audio/visual gates.
- [ ] `beta.otobank.com` and `COMMUNITY FREE` remain present.
- [ ] Prior captures, timelines, exports, and diagnostic evidence remain intact.
- [ ] Beads and Git describe the same final state.
