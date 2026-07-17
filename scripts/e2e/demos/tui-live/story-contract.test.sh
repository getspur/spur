#!/usr/bin/env bash
# Static contract for the five value films. The live UAT remains the runtime proof.
# Operator home = Session Detail (not dashboard).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failures=0

pass() {
  printf 'PASS %s\n' "$1"
}

fail() {
  printf 'FAIL %s\n' "$1" >&2
  failures=$((failures + 1))
}

assert_has() {
  local file="$1"
  local needle="$2"
  local label="$3"
  if rg -q --fixed-strings -- "$needle" "$file"; then
    pass "$label"
  else
    fail "$label (missing: $needle)"
  fi
}

assert_matches() {
  local file="$1"
  local pattern="$2"
  local label="$3"
  if rg -q --multiline --regexp "$pattern" -- "$file"; then
    pass "$label"
  else
    fail "$label (missing regex: $pattern)"
  fi
}

assert_not_matches() {
  local file="$1"
  local pattern="$2"
  local label="$3"
  if rg -q --multiline --regexp "$pattern" -- "$file"; then
    fail "$label (found regex: $pattern)"
  else
    pass "$label"
  fi
}

assert_lacks() {
  local file="$1"
  local needle="$2"
  local label="$3"
  if rg -q --fixed-strings -- "$needle" "$file"; then
    fail "$label (found: $needle)"
  else
    pass "$label"
  fi
}

assert_count_at_least() {
  local file="$1"
  local needle="$2"
  local minimum="$3"
  local label="$4"
  local count
  count="$(rg -c --fixed-strings -- "$needle" "$file" || true)"
  if [[ "${count:-0}" -ge "$minimum" ]]; then
    pass "$label"
  else
    fail "$label (found ${count:-0}, need $minimum: $needle)"
  fi
}

lib="$root/lib.sh"
assert_has "$lib" 'story_beat() {' 'shared story beat helper'
assert_has "$lib" 'story_hard_proof() {' 'shared hard-proof helper'
assert_has "$lib" 'story_soft_proof() {' 'shared labeled soft-proof helper'
assert_has "$lib" 'land_session_detail() {' 'session detail landing helper'
assert_has "$lib" 'story_session_land() {' 'session land alias'
assert_has "$lib" 'story_session_workers() {' 'session workers panel helper'
assert_has "$lib" 'story_session_plan_inspector() {' 'Alt+p plan inspector helper'
assert_has "$lib" 'return_to_session_detail() {' 'return home to session helper'
assert_has "$lib" 'story_resolution() {' 'shared resolution helper'
assert_has "$lib" 'Session Detail is the operator home' 'docs session-first home in lib'
assert_has "$lib" 'land_session_detail' 'start_live_tui lands session after dashboard cold-start'
assert_has "$lib" "type_slow \"@worker:\${worker}\"" 'worker cascade avoids paste burst'
assert_lacks "$lib" 'press_key 1' 'Agents focus never uses digit 1'
assert_has "$lib" 'press_key Alt+d' 'workers panel uses Alt+d'
assert_has "$lib" 'press_key Alt+p' 'plan inspector uses Alt+p'
assert_has "$lib" 'seed_task_id="demo-echo-$$"' 'plan-loop seed uses a per-run correlation tag'
assert_lacks "$lib" 'Brain transcript acknowledges plan submission' 'seed prompt text never claims brain-result proof'

stories=(
  problem-plan-loop-drive
  product-e2e-flow
  problem-ops-visibility
  problem-plan-progress
  problem-backlog-triage
)

for story in "${stories[@]}"; do
  file="$root/journeys/$story.sh"
  assert_has "$file" 'story_beat "HOOK"' "$story has a hook"
  assert_has "$file" 'story_beat "ORIENTATION"' "$story has orientation"
  assert_has "$file" 'story_beat "ACTION"' "$story has action"
  assert_has "$file" 'story_beat "PROOF"' "$story names its proof"
  assert_has "$file" 'story_session_land' "$story lands Session Detail home"
  assert_has "$file" 'story_resolution' "$story resolves the opening problem"
  assert_has "$file" 'Session Detail' "$story names Session Detail in narrative"
done

probes=(
  lineage-dashboard
  sessions-picker
  palette-open
  session-resume
  explore-browser
  explore-agents-tab
  composer-draft
  agent-send
)

for probe in "${probes[@]}"; do
  file="$root/journeys/$probe.sh"
  assert_lacks "$file" 'wait_text "Lineage"' "$probe does not assume dashboard startup"
  assert_not_matches "$file" '^[[:space:]]*press_key[[:space:]]+s[[:space:]]*$' "$probe avoids the obsolete dashboard sessions shortcut"
done

assert_has "$root/journeys/lineage-dashboard.sh" 'return_to_dashboard' 'lineage probe explicitly opens dashboard'
assert_has "$root/journeys/lineage-dashboard.sh" 'story_dashboard_land' 'lineage probe proves dashboard landing'
assert_has "$root/journeys/sessions-picker.sh" 'story_session_land' 'sessions probe proves session-first startup'
assert_has "$root/journeys/sessions-picker.sh" 'open_sessions_picker' 'sessions probe uses palette navigation helper'
assert_has "$root/journeys/palette-open.sh" 'story_session_land' 'palette probe starts from Session Detail'
assert_has "$root/journeys/palette-open.sh" 'return_to_session_detail' 'palette probe returns to Session Detail'
assert_has "$root/journeys/session-resume.sh" 'story_session_land' 'resume probe starts from Session Detail'
assert_has "$root/journeys/explore-browser.sh" 'story_session_land' 'explore probe starts from Session Detail'
assert_has "$root/journeys/explore-browser.sh" 'return_to_session_detail' 'explore probe returns to Session Detail'
assert_has "$root/journeys/explore-agents-tab.sh" 'story_session_land' 'explore Agents probe starts from Session Detail'
assert_has "$root/journeys/explore-agents-tab.sh" 'return_to_session_detail' 'explore Agents probe returns to Session Detail'
assert_has "$root/journeys/composer-draft.sh" 'story_session_land' 'draft probe starts from Session Detail'
assert_has "$root/journeys/composer-draft.sh" 'open_sessions_picker' 'draft probe exercises session switching'
assert_matches "$root/journeys/composer-draft.sh" 'press_key n\nwait_text "has an unsent draft"\nexpect_text "save and start a new session"\npress_key n\nwait_text "Sessions"' 'draft probe proves and cancels the switch-safety confirmation in order'
assert_has "$root/journeys/agent-send.sh" 'story_session_land' 'agent-send canary proves session-first startup'

tapes=(
  09-product-e2e-flow.tape
  10-problem-ops-visibility.tape
  11-problem-plan-progress.tape
  12-problem-backlog-triage.tape
  13-problem-plan-loop-drive.tape
)

for tape in "${tapes[@]}"; do
  file="$root/tapes/$tape"
  for stage in HOOK ORIENTATION ACTION PROOF RESOLUTION; do
    assert_has "$file" "# STORY: $stage" "$tape mirrors $stage"
  done
  assert_has "$file" 'Type "Sessions"' "$tape opens Sessions from Go to"
  assert_has "$file" 'Type "n"' "$tape prefers new-session attach for reliability"
  assert_has "$file" '/Session ·|INSERT' "$tape proves Session Detail attach"
  assert_not_matches "$file" 'Wait\+Screen[^\n]*Failed to initialize' \
    "$tape does not accept failed initialization as Session Detail proof"
done

assert_matches "$root/tapes/13-problem-plan-loop-drive.tape" \
  '# STORY: PROOF - return home to Session Detail\nHide\nEscape\nShow\nWait\+Screen@[1-9][0-9]*s /Session ·\|INSERT/' \
  'plan-loop returns to Session Detail before resolution proof'
assert_has "$root/tapes/04-session-resume.tape" '/Session ·|INSERT/' \
  'resume tape requires Session Detail rather than matching Sessions picker text'
assert_has "$root/tapes/04-session-resume.tape" 'Ctrl+K' \
  'resume tape opens Go to from Session Detail'
assert_has "$root/tapes/04-session-resume.tape" 'Type "Sessions"' \
  'resume tape opens Sessions through Go to'
assert_not_matches "$root/tapes/04-session-resume.tape" 'Wait\+Screen[^\n]*/Lineage/' \
  'resume tape does not assume Dashboard startup'

assert_has "$root/journeys/product-e2e-flow.sh" 'SPUR_DEMO_ALLOW_AGENT_SEND' 'product send remains opt-in'
assert_has "$root/journeys/problem-plan-loop-drive.sh" 'SPUR_DEMO_ALLOW_PLAN_LOOP' 'plan-loop seed remains opt-in'
assert_has "$root/../geometry.env" ": \"\${SPUR_DEMO_STORY_PACE:=0}\"" 'story dwell stays off by default'
assert_matches "$root/tapes/09-product-e2e-flow.tape" 'Wait\+Screen@[1-9][0-9]*s /agent=/' 'product tape proves cascade agent='
assert_matches "$root/tapes/09-product-e2e-flow.tape" 'Wait\+Screen@[1-9][0-9]*s /model=/' 'product tape proves cascade model='
assert_matches "$root/tapes/09-product-e2e-flow.tape" 'Wait\+Screen@[1-9][0-9]*s /effort=/' 'product tape proves cascade effort='
assert_has "$root/tapes/11-problem-plan-progress.tape" '/Progress|No plans found/' 'plan-progress tape accepts campaign rows or honest empty state'
assert_has "$root/tapes/12-problem-backlog-triage.tape" 'Wait+Screen@12s /status: open|priority: P0|bd-|No issues/' 'backlog tape binds detail or empty'
assert_has "$root/tapes/13-problem-plan-loop-drive.tape" 'Alt+p' 'plan-loop tape tries session plan inspector'
assert_has "$root/tapes/10-problem-ops-visibility.tape" 'Alt+d' 'ops tape opens workers panel'
assert_has "$lib" '_lineage_select_worker_node' 'lineage selects EXEC worker not BRAIN root'
assert_has "$lib" '_lineage_wait_stream_panel' 'lineage waits for stream panel'
assert_has "$lib" '_lineage_wait_task_tab' 'lineage opens task tab for assigned work'
assert_has "$lib" 'press_key Ctrl+1' 'stream tab via Ctrl+1'
assert_has "$lib" 'press_key Ctrl+4' 'task tab via Ctrl+4'
# VHS has no Ctrl+digit — tapes cycle with `l` to task; shell-use uses Ctrl+1/4.
assert_matches "$root/tapes/10-problem-ops-visibility.tape" 'Wait\+Screen@[1-9][0-9]*s /stream/' 'ops tape waits stream panel'
assert_matches "$root/tapes/10-problem-ops-visibility.tape" 'Wait\+Screen@[1-9][0-9]*s /task/' 'ops tape opens task tab'
assert_matches "$root/tapes/13-problem-plan-loop-drive.tape" 'Wait\+Screen@[1-9][0-9]*s /stream/' 'plan-loop tape waits stream panel'
assert_matches "$root/tapes/13-problem-plan-loop-drive.tape" 'Wait\+Screen@[1-9][0-9]*s /task/' 'plan-loop tape opens task tab'
assert_has "$root/tapes/10-problem-ops-visibility.tape" 'Type "l"' 'ops tape cycles detail tabs with l'
assert_has "$root/tapes/13-problem-plan-loop-drive.tape" 'Type "l"' 'plan-loop tape cycles detail tabs with l'

hitl_capture="$root/capture-live-hitl.sh"
plan_loop="$root/journeys/problem-plan-loop-drive.sh"

assert_matches "$lib" '(?m)^require_hitl_loop_opt_in\(\) \{$' \
  'D4 defines its dedicated spend guard at top level'
assert_matches "$lib" '(?m)^require_hitl_loop_opt_in\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+fi[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+if \[\[ ! -d "\$project/\.beads" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?beads-backed project[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 2[[:blank:]]*$' \
  'D4 direct journey guard rejects a project without a beads backend before spend'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+require_hitl_loop_opt_in[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+local seed_task_id="demo-hitl-\$\$"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task id: \$\{seed_task_id\}[^"\n]*"[[:blank:]]*$' \
  'D4 helper invokes its guard before declaring and typing the correlation tag'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Alt\+p[[:blank:]]*\n^[[:blank:]]+story_hard_proof[[:blank:]]+\\[[:blank:]]*\n[[:blank:]]+"Plan Inspector is open on the selected task detail"[[:blank:]]+\\[[:blank:]]*\n[[:blank:]]+"Task detail"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"\$seed_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"awaiting_review"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"summary: D4 FINDING:"[^\n]*$' \
  'D4 proves the Plan Inspector task-detail view before transcript-derived review markers'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Alt\+p[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"\$seed_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"awaiting_review"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"summary: D4 FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key d[[:blank:]]*$' \
  'D4 first review hard-correlates task, state, and worker finding before reject'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*D4 FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*$' \
  'D4 initial FINDING prompt is read-only inside the HITL helper'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"Retry Task"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*$' \
  'D4 post-Retry Task prompt is read-only inside the HITL helper'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"Decision: Reject"[^\n]*$' \
  'D4 captures Reject as a hard proof'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"Retry Task"[^\n]*$' \
  'D4 captures Retry Task as a hard proof'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_slow "[^"\n]*SOURCE: <exact path>[^"\n]*"[[:blank:]]*$' \
  'D4 retry types an exact SOURCE path requirement'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "RECOMMENDATION: <one sentence>[^"\n]*"[[:blank:]]*$' \
  'D4 retry types a RECOMMENDATION requirement'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"Decision: Approve"[^\n]*$' \
  'D4 captures Approve as a hard proof'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"Decision: Approve"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return_to_session_detail[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_slow "D4 HITL COMPLETE\. Synthesize[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Do not call tools or delegate\."[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"D4 SYNTHESIS:"[^\n]*$' \
  'D4 synthesis follows approval and session return without redelegating'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Do not call tools or delegate\."[[:blank:]]*$' \
  'D4 synthesis types its explicit no-tools instruction inside the helper'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key d[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"Decision: Reject"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key R[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"Retry Task"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"Decision: Approve"[^\n]*$' \
  'D4 HITL helper performs Reject then Retry then Approve in order'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+"SOURCE:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Enter[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"awaiting_review"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"\$seed_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"summary: SOURCE:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*\n[[:blank:]]+"[^"\n]*"[^\n]*\n[[:blank:]]+"RECOMMENDATION:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*$' \
  'D4 retry hard-correlates state, task, source, and recommendation before approve'
assert_matches "$plan_loop" '(?m)^if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" == "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+trigger_submit_plan_hitl_review_and_synthesize[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^elif \[\[ "\$\{SPUR_DEMO_ALLOW_PLAN_LOOP:-0\}" == "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+trigger_submit_plan_one_task_and_observe[[:blank:]]*$' \
  'D4 executable branch precedes the existing minimal plan-loop branch'
assert_matches "$hitl_capture" '(?m)^export SPUR_DEMO_ALLOW_HITL_LOOP=1[[:blank:]]*\n(?:^.*\n)*?^export SPUR_DEMO_ALLOW_PLAN_LOOP=0[[:blank:]]*\n(?:^.*\n)*?^exec "\$ROOT/capture-live-seed\.sh"[[:blank:]]*$' \
  'D4 wrapper enables HITL, disables the minimal loop, then executes capture'
assert_matches "$hitl_capture" '(?m)^export SPUR_DEMO_CAPTURE_STEM_PREFIX=15-live-hitl-agent-loop[[:space:]]*$' \
  'D4 capture wrapper exports a distinct stable output stem'
assert_matches "$root/capture-live-seed.sh" '(?m)^export SPUR_DEMO_CAPTURE_STEM_PREFIX="\$\{SPUR_DEMO_CAPTURE_STEM_PREFIX:-14-live-plan-loop-seed\}"[[:blank:]]*\n(?:^.*\n)*?^stem_prefix="\$SPUR_DEMO_CAPTURE_STEM_PREFIX"[[:blank:]]*$' \
  'D4 shared capture derives its output stem from the explicit override'
assert_matches "$root/capture-live-seed.sh" '(?m)^if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n)*?^fi[[:blank:]]*\n^export SPUR_DEMO_ALLOW_PLAN_LOOP="\$\{SPUR_DEMO_ALLOW_PLAN_LOOP:-0\}"[[:blank:]]*$' \
  'D4 shared capture initializes the preserved plan-loop gate after HITL routing'
assert_matches "$root/capture-live-seed.sh" '(?m)^export SPUR_DEMO_ALLOW_HITL_LOOP="\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}"[[:blank:]]*\n(?:^.*\n)*?^if \[\[ -n "\$\{SPUR_DEMO_PROJECT:-\}" \]\]; then[[:blank:]]*\n^[[:blank:]]+capture_project="\$SPUR_DEMO_PROJECT"[[:blank:]]*\n^else[[:blank:]]*\n^[[:blank:]]+capture_project="\$\(git -C "\$E2E_ROOT/\.\./\.\." rev-parse --show-toplevel\)"[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^if \[\[ "\$SPUR_DEMO_ALLOW_HITL_LOOP" == "1" && ! -d "\$capture_project/\.beads" \]\]; then[[:blank:]]*\n(?:^.*\n)*?D4 requires a beads-backed project before TUI startup[^\n]*\n(?:^.*\n)*?SPUR_DEMO_PROJECT=/path/to/beads-project[^\n]*\n(?:^.*\n)*?^[[:blank:]]+exit 2[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^stamp=' \
  'D4 capture checks the effective project beads backend before launching the journey'
assert_matches "$root/capture-live-seed.sh" '(?m)^[[:space:]]*cp -p "\$log" "\$OUT/\$\{stem_prefix\}\.log"[[:space:]]*$' \
  'D4 shared capture executes the full stable audit-log copy command'

if [[ "$failures" -ne 0 ]]; then
  printf '\n%d story-contract check(s) failed\n' "$failures" >&2
  exit 1
fi

printf '\nAll story-contract checks passed\n'
