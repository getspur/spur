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
capture_seed="$root/capture-live-seed.sh"
assert_matches "$lib" '(?m)^require_hitl_loop_opt_in\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+local [^\n]*\n)*^[[:blank:]]+if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*$' \
  'PH audit spend guard checks opt-in before any non-local action'
assert_matches "$lib" '(?m)^require_hitl_loop_opt_in\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+local [^\n]*\n)*^[[:blank:]]+if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 2[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n(?:^[[:blank:]]*#[^\n]*\n|^[[:blank:]]*\n)*^[[:blank:]]+if \[\[ ! -d "\$project/\.beads" \]\]; then[[:blank:]]*$' \
  'PH audit opt-out returns 2 then hands directly to the beads preflight'
assert_matches "$lib" '(?m)^require_hitl_loop_opt_in\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+if \[\[ ! -d "\$project/\.beads" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^beads-backed project[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 2[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit missing-beads preflight emits its error and returns 2'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n^[[:blank:]]+require_hitl_loop_opt_in[[:blank:]]*\n^[[:blank:]]+local positioning_task_id="ph-acp-positioning-\$\$"[[:blank:]]*\n^[[:blank:]]+local proof_task_id="ph-tui-proof-\$\$"[[:blank:]]*\n^[[:blank:]]+local readiness_task_id="ph-launch-readiness-\$\$"[[:blank:]]*\n^[[:blank:]]+local handoff_task_id="ph-media-handoff-\$\$"[[:blank:]]*$' \
  'PH audit campaign guard is the first action before all four exact task ids'
assert_matches "$lib" '(?m)^start_fresh_session_detail\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+open_sessions_picker[[:blank:]]*\n^[[:blank:]]+wait_text "Start new session"[[:blank:]]*\n^[[:blank:]]+press_key n[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+if ! _live_wait_session_detail [1-9][0-9]*; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+printf [^\n]*fatal: fresh Session Detail attach failed[^\n]*\n^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^[[:blank:]]+expect_text "INSERT"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^\}[[:blank:]]*$' \
  'PH audit fresh-session helper fails closed unless Start new session attaches'
assert_matches "$lib" '(?m)^start_fresh_session_detail\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key n[[:blank:]]*\n^[[:blank:]]+_live_confirm_draft_if_needed \|\| true[[:blank:]]*\n^[[:blank:]]+if ! _live_wait_session_detail [1-9][0-9]*; then[[:blank:]]*$' \
  'PH audit saves any existing draft before waiting for the fresh paid session'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n^[[:blank:]]+require_hitl_loop_opt_in[[:blank:]]*\n^[[:blank:]]+local positioning_task_id="ph-acp-positioning-\$\$"[[:blank:]]*\n^[[:blank:]]+local proof_task_id="ph-tui-proof-\$\$"[[:blank:]]*\n^[[:blank:]]+local readiness_task_id="ph-launch-readiness-\$\$"[[:blank:]]*\n^[[:blank:]]+local handoff_task_id="ph-media-handoff-\$\$"[[:blank:]]*\n(?:^\n)*^[[:blank:]]+start_fresh_session_detail[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_slow "PRODUCT HUNT LIVE CAPTURE\. "[[:blank:]]*$' \
  'PH audit starts the paid campaign in an explicit fresh Session Detail'
assert_matches "$lib" '(?m)^land_plan_inspector_for_task\(\) \{[[:blank:]]*\n^[[:blank:]]+local task_id="\$1"[[:blank:]]*\n^[[:blank:]]+local timeout_s="\$\{2:-\$\{SPUR_DEMO_PLAN_LOOP_WAIT_S:-180\}\}"[[:blank:]]*\n^[[:blank:]]+local deadline=\$\(\(SECONDS \+ timeout_s\)\)[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+while \(\( SECONDS < deadline \)\); do[[:blank:]]*$' \
  'PH audit Plan Inspector inherits the campaign budget with a 180s fallback'
assert_matches "$lib" '(?m)^land_plan_inspector_for_task\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+while \(\( SECONDS < deadline \)\); do[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Alt\+p[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+if soft_has_text "Task detail"[^\n]*&& soft_has_text "\$task_id"[^\n]*; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 0[[:blank:]]*$' \
  'PH audit Plan Inspector loop pins both task detail and requested task id'
assert_matches "$lib" '(?m)^land_plan_inspector_for_task\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+done[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+printf [^\n]*fatal: Plan Inspector never pinned[^\n]*\n^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit Plan Inspector timeout is fatal and nonzero'
assert_matches "$lib" '(?m)^plan_inspector_output_text\(\) \{[[:blank:]]*\n^[[:blank:]]+"\$shell_use_bin" --session "\$session_name" text --full \| awk '\''[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+\$0 ~ /[^\n]*Output[^\n]*/ \{[[:blank:]]*\n^[[:blank:]]+buffer = \$0 ORS[[:blank:]]*\n^[[:blank:]]+in_output = 1[[:blank:]]*\n^[[:blank:]]+next[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+in_output \{[[:blank:]]*\n^[[:blank:]]+buffer = buffer \$0 ORS[[:blank:]]*\n^[[:blank:]]+if \(index\(\$0, "next: review"\) > 0\) \{[[:blank:]]*\n^[[:blank:]]+printf "%s", buffer[[:blank:]]*\n^[[:blank:]]+exit[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+'\''[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit extracts only a complete current Plan Inspector Output block'
assert_matches "$lib" '(?m)^story_plan_inspector_result_hard_proof\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+local [^\n]*\n)+(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+while \(\( SECONDS < deadline \)\); do[[:blank:]]*\n^[[:blank:]]+output="\$\(plan_inspector_output_text \|\| true\)"[[:blank:]]*\n^[[:blank:]]+if \[\[ "\$output" == \*"summary:"\* && "\$output" == \*"\$marker"\* \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 0[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+done[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+printf [^\n]*fatal: Plan Inspector Output never exposed[^\n]*\n^[[:blank:]]+"\$shell_use_bin" --session "\$session_name" text --full >&2 \|\| true[[:blank:]]*\n^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit hard-waits normalized summaries only inside bounded Plan Inspector output'
assert_not_matches <(awk '/^trigger_submit_plan_hitl_review_and_synthesize\(\) \{/ { in_hitl=1 } in_hitl { print } in_hitl && /^}/ { exit }' "$lib") '(?m)^[[:blank:]]+story_hard_proof "The (?:positioning result exposes|proof result exposes|retry exposes|readiness result exposes|handoff result exposes)' \
  'PH audit worker result gates cannot match outgoing prompts or issue bodies globally'
assert_has "$lib" 'Call submit_plan with exactly FOUR independent read-only tasks.' \
  'PH audit requests a populated four-task plan'
assert_count_at_least "$lib" 'effort: medium.' 4 \
  'PH audit requests visible resolved effort for every worker'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task 1 id: \$\{positioning_task_id\}\. Worker: claude-code\. effort: medium\.[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*PH POSITIONING FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*\n^[[:blank:]]+type_text "Task 2 id: \$\{proof_task_id\}[^"\n]*"[[:blank:]]*$' \
  'PH audit positioning prompt is read-only and precedes proof routing'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task 2 id: \$\{proof_task_id\}\. Worker: grok\. effort: medium\.[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*PH PROOF FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*\n^[[:blank:]]+type_text "Task 3 id: \$\{readiness_task_id\}[^"\n]*"[[:blank:]]*$' \
  'PH audit proof prompt routes read-only work to Grok before readiness'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task 3 id: \$\{readiness_task_id\}\. Worker: codex\. effort: medium\.[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*PH READINESS FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*$' \
  'PH audit readiness prompt is read-only with explicit routing and effort'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task 3 id: \$\{readiness_task_id\}\. Worker: codex\. effort: medium\.[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*PH READINESS FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*\n^[[:blank:]]+type_text "Task 4 id: \$\{handoff_task_id\}[^"\n]*"[[:blank:]]*$' \
  'PH audit readiness prompt precedes handoff routing'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Task 4 id: \$\{handoff_task_id\}\. Worker: opencode\. effort: medium\.[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "[^"\n]*PH HANDOFF FINDING:[^"\n]*Make no file changes\.[^"\n]*"[[:blank:]]*$' \
  'PH audit handoff prompt routes read-only work to OpenCode'
assert_lacks <(awk '/^trigger_submit_plan_hitl_review_and_synthesize\(\) \{/ { in_hitl=1 } in_hitl { print } in_hitl && /^}/ { exit }' "$lib") 'Worker: gemini.' \
  'PH audit removes Gemini from the four-agent campaign'
assert_lacks <(awk '/^trigger_submit_plan_hitl_review_and_synthesize\(\) \{/ { in_hitl=1 } in_hitl { print } in_hitl && /^}/ { exit }' "$lib") 'Workers (3)' \
  'PH audit removes the obsolete three-worker panel proof'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type_text "Leave every completed task awaiting_review for the operator\."[[:blank:]]*\n(?:^[[:blank:]]+sleep_ms[^\n]*\n)?^[[:blank:]]+press_key Enter[[:blank:]]*$' \
  'PH audit submits the operator review hold with the campaign prompt'
assert_matches "$lib" '(?m)^workers_panel_text\(\) \{[[:blank:]]*\n^[[:blank:]]+"\$shell_use_bin" --session "\$session_name" text \| awk '\''[[:blank:]]*\n^[[:blank:]]+index\(\$0, "Workers \("\) > 0 \{[[:blank:]]*\n^[[:blank:]]+buffer = \$0 ORS[[:blank:]]*\n^[[:blank:]]+in_workers = 1[[:blank:]]*\n^[[:blank:]]+next[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+in_workers \{[[:blank:]]*\n^[[:blank:]]+buffer = buffer \$0 ORS[[:blank:]]*\n^[[:blank:]]+if \(index\(\$0, "Alt\+D collapse"\) > 0\) \{[[:blank:]]*\n^[[:blank:]]+printf "%s", buffer[[:blank:]]*\n^[[:blank:]]+exit[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+\}[[:blank:]]*\n^[[:blank:]]+'\''[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit emits Workers text only after the expanded bottom boundary'
assert_matches "$lib" '(?m)^story_workers_panel_hard_proof\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+local [^\n]*\n)+(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+while \(\( SECONDS < deadline \)\); do[[:blank:]]*\n^[[:blank:]]+panel="\$\(workers_panel_text \|\| true\)"[[:blank:]]*\n^[[:blank:]]+if \[\[ "\$panel" == \*"\$anchor"\* \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return 0[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+done[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+printf [^\n]*fatal: Workers panel never exposed[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit hard-waits anchors only inside the extracted Workers panel'
assert_not_matches <(awk '/^trigger_submit_plan_hitl_review_and_synthesize\(\) \{/ { in_hitl=1 } in_hitl { print } in_hitl && /^}/ { exit }' "$lib") '(?m)^[[:blank:]]+story_hard_proof[^\n]*"(?:Workers \(4\)|claude-code|grok|codex|opencode)"' \
  'PH audit worker routing never uses global terminal proofs'
assert_not_matches <(awk '/^[[:blank:]]+start_fresh_session_detail$/ { in_fresh=1 } in_fresh { print } in_fresh && /story_workers_panel_hard_proof[^\n]*"opencode"/ { exit }' "$lib") '(?m)^[[:blank:]]+press_key Alt\+d[[:blank:]]*$' \
  'PH audit leaves the fresh Workers panel expanded through all routing proofs'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*"THINK"[^\n]*\n(?:^\n)*^[[:blank:]]+story_workers_panel_hard_proof[^\n]*"Workers \(4\)"[^\n]*\n^[[:blank:]]+story_workers_panel_hard_proof[^\n]*"claude-code"[^\n]*\n^[[:blank:]]+story_workers_panel_hard_proof[^\n]*"grok"[^\n]*\n^[[:blank:]]+story_workers_panel_hard_proof[^\n]*"codex"[^\n]*\n^[[:blank:]]+story_workers_panel_hard_proof[^\n]*"opencode"[^\n]*\n^[[:blank:]]+story_dwell[^\n]*\n^[[:blank:]]+press_key Alt\+d[[:blank:]]*$' \
  'PH audit proves the expanded Workers panel before collapsing it'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+land_plan_inspector_for_task "\$positioning_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*"awaiting_review"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH POSITIONING FINDING:"[^\n]*\n^[[:blank:]]+press_key a[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Decision: Approve"[^\n]*\n^[[:blank:]]+press_key Enter[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"approved"[^\n]*$' \
  'PH audit correlates positioning review through confirmed approval'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key j[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"\$proof_task_id"[^\n]*\n^[[:blank:]]+story_hard_proof[^\n]*"awaiting_review"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH PROOF FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key d[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Decision: Reject"[^\n]*\n^[[:blank:]]+press_key Enter[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"rejected"[^\n]*$' \
  'PH audit correlates proof review through confirmed rejection'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key R[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Retry Task"[^\n]*\n^[[:blank:]]+type_slow "READ ONLY\.[^"\n]*"[[:blank:]]*\n^[[:blank:]]+type_text "SOURCE: <exact path>; WINDOW: <exact seconds or line range>;[^"\n]*"[[:blank:]]*\n^[[:blank:]]+type_text "RECOMMENDATION: <one sentence>\. Make no file changes\."[[:blank:]]*\n(?:^[[:blank:]]+story_hard_proof[^\n]*"(?:SOURCE:|WINDOW:)"[^\n]*\n){2}^[[:blank:]]+press_key Enter[[:blank:]]*$' \
  'PH audit retry submits read-only source window and recommendation requirements'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key R[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Enter[[:blank:]]*\n(?:^\n)*^[[:blank:]]+story_hard_proof[^\n]*"awaiting_review"[^\n]*\n^[[:blank:]]+story_hard_proof[^\n]*"\$proof_task_id"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"SOURCE:"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"WINDOW:"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"RECOMMENDATION:"[^\n]*$' \
  'PH audit retry remains on proof task and exposes all evidence markers'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"RECOMMENDATION:"[^\n]*\n^[[:blank:]]+press_key a[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Decision: Approve"[^\n]*\n^[[:blank:]]+press_key Enter[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"approved"[^\n]*$' \
  'PH audit retry approval is explicitly confirmed'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key j[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"\$readiness_task_id"[^\n]*\n^[[:blank:]]+story_hard_proof[^\n]*"awaiting_review"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH READINESS FINDING:"[^\n]*\n^[[:blank:]]+press_key a[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Decision: Approve"[^\n]*\n^[[:blank:]]+press_key Enter[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"approved"[^\n]*$' \
  'PH audit correlates readiness review through confirmed approval'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key j[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"\$handoff_task_id"[^\n]*\n^[[:blank:]]+story_hard_proof[^\n]*"awaiting_review"[^\n]*\n^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH HANDOFF FINDING:"[^\n]*\n^[[:blank:]]+press_key a[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"Decision: Approve"[^\n]*\n^[[:blank:]]+press_key Enter[[:blank:]]*\n^[[:blank:]]+story_hard_proof[^\n]*"approved"[^\n]*$' \
  'PH audit selects and approves the bounded handoff result'
assert_matches "$lib" '(?m)^return_to_campaign_session_detail\(\) \{[[:blank:]]*\n^[[:blank:]]+if session_detail_is_visible; then[[:blank:]]*\n^[[:blank:]]+return 0[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n(?:^\n)*^[[:blank:]]+press_key Escape[[:blank:]]*\n^[[:blank:]]+if "\$shell_use_bin" --session "\$session_name" wait text "Session ·\|INSERT" --regex --timeout "\$timeout_ms"[^\n]*; then[[:blank:]]*\n^[[:blank:]]+expect_text "INSERT"[[:blank:]]*\n^[[:blank:]]+return 0[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n(?:^\n)*^[[:blank:]]+"\$shell_use_bin" --session "\$session_name" text --full >&2 \|\| true[[:blank:]]*\n^[[:blank:]]+printf [^\n]*fatal: could not return directly to the campaign Session Detail[^\n]*\n^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^\}[[:blank:]]*$' \
  'PH audit synthesis return accepts only the current Session Detail or one direct Escape'
assert_not_matches <(awk '/^return_to_campaign_session_detail\(\) \{/ { in_return=1 } in_return { print } in_return && /^}/ { exit }' "$lib") '(?:land_session_detail|attach_session_for_send|open_sessions_picker|resume_session|start_fresh_session_detail|press_key n)' \
  'PH audit synthesis return has no attach, resume, picker, or new-session fallback'
assert_not_matches "$lib" '(?m)^[[:blank:]]+type(?:_text|_slow)[[:blank:]]+"[^"\n]*PH AUDIT SYNTHESIS:' \
  'PH audit outgoing prompt cannot self-satisfy the synthesis proof'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return_to_campaign_session_detail[[:blank:]]*\n^[[:blank:]]+story_session_land[^\n]*\n^[[:blank:]]+type(?:_text|_slow) "[^"\n]*Synthesize approved evidence from \$\{positioning_task_id\}, \$\{proof_task_id\}, \$\{readiness_task_id\}, and \$\{handoff_task_id\}[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+type(?:_text|_slow) "[^"\n]*words PH AUDIT SYNTHESIS, then a colon, one space, and the proof task id \$\{proof_task_id\}[^"\n]*"[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key Enter[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*"PH AUDIT SYNTHESIS: \$\{proof_task_id\}"[^\n]*$' \
  'PH audit synthesis prompt names all four ids and proof is proof-task-correlated'
assert_matches "$lib" '(?m)^trigger_submit_plan_hitl_review_and_synthesize\(\) \{[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+land_plan_inspector_for_task "\$positioning_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH POSITIONING FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+[^\n]*"Decision: Approve"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH PROOF FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key d[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+[^\n]*"Decision: Reject"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key R[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"SOURCE:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"WINDOW:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"RECOMMENDATION:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+[^\n]*"Decision: Approve"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH READINESS FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+[^\n]*"Decision: Approve"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*"\$handoff_task_id"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_plan_inspector_result_hard_proof[^\n]*"PH HANDOFF FINDING:"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+press_key a[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+[^\n]*"Decision: Approve"[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+return_to_campaign_session_detail[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+story_hard_proof[^\n]*"PH AUDIT SYNTHESIS: \$\{proof_task_id\}"[^\n]*$' \
  'PH audit campaign order reaches four approvals, one proof retry, and proof-correlated synthesis'
assert_has "$hitl_capture" 'SPUR_DEMO_CAPTURE_STEM_PREFIX=17-live-product-hunt-four-agent-loop' \
  'PH capture wrapper uses the four-agent versioned stem'
assert_matches "$hitl_capture" '(?m)^export SPUR_DEMO_PLAN_LOOP_WAIT_S="\$\{SPUR_DEMO_PLAN_LOOP_WAIT_S:-420\}"[[:blank:]]*\n^export SHELL_USE_TIMEOUT_MS="\$\{SHELL_USE_TIMEOUT_MS:-\$\(\(SPUR_DEMO_PLAN_LOOP_WAIT_S \* 1000\)\)\}"[[:blank:]]*\n(?:^export [^\n]*\n)*?^exec "\$ROOT/capture-live-seed\.sh"[[:blank:]]*$' \
  'PH capture derives hard-proof milliseconds from the 420s campaign budget before exec'
assert_has "$hitl_capture" 'SPUR_CAPTURE_FULL_FIDELITY=1' \
  'PH capture requests the full-duration 2560x1600 encode path'
assert_has "$hitl_capture" 'SPUR_AGG_IDLE_LIMIT="${SPUR_AGG_IDLE_LIMIT:-6.0}"' \
  'PH capture preserves proof dwells instead of truncating them to 1.5 seconds'
assert_has "$capture_seed" 'local full_fidelity="${SPUR_CAPTURE_FULL_FIDELITY:-0}"' \
  'capture seed keeps full-fidelity encoding opt-in'
assert_matches "$capture_seed" '(?m)^[[:blank:]]*if \[\[ -f "\$gif_out" && "\$full_fidelity" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+command -v ffmpeg[^\n]*\|\| return 1[[:blank:]]*$' \
  'full-fidelity encode is an active opt-in branch with ffmpeg preflight'
assert_matches "$capture_seed" '(?m)^[[:blank:]]*if \[\[ -f "\$gif_out" && "\$full_fidelity" == "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+ffmpeg [^\n]*-i "\$gif_out"[^\n]*\n^[[:blank:]]+-vf '\''fps=30,scale=2560:1600:force_original_aspect_ratio=decrease,pad=2560:1600:[^'\'']*'\''[^\n]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+-movflags [^\n]*"\$mp4_out" \|\| return 1[[:blank:]]*$' \
  'full-fidelity branch directly encodes complete GIF at 30 fps and 2560x1600'
assert_matches "$capture_seed" '(?m)^[[:blank:]]*if \[\[ -f "\$gif_out" && "\$full_fidelity" == "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]*elif \[\[ -f "\$gif_out" \]\] && command -v ffmpeg[^\n]*&& command -v python3[^\n]*; then[[:blank:]]*$' \
  'full-fidelity branch falls through to sampled preview only via elif'
assert_matches "$capture_seed" '(?m)^if \[\[ "\$\{SPUR_CAPTURE_FULL_FIDELITY:-0\}" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+if ! command -v ffmpeg[^\n]*; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+exit [1-9][0-9]*[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^[[:blank:]]+if ! command -v agg[^\n]*&& ! command -v docker[^\n]*; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+exit [1-9][0-9]*[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^if ! SPUR_BIN=' \
  'full-fidelity capture preflights ffmpeg and agg-or-Docker before resolving the journey binary'
assert_matches "$capture_seed" '(?m)^[[:blank:]]+if docker run[^\n]*; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+elif \[\[ "\$full_fidelity" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+return [1-9][0-9]*[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*$' \
  'full-fidelity Docker conversion failure is not masked'
assert_lacks <(awk '/if \[\[ -f "\$gif_out" && "\$full_fidelity" == "1" \]\]; then/ { in_full=1 } in_full && /elif \[\[ -f "\$gif_out" \]\]/ { exit } in_full { print }' "$capture_seed") 'python3' \
  'full-fidelity branch never invokes the Python sampler'
assert_lacks <(awk '/if \[\[ -f "\$gif_out" && "\$full_fidelity" == "1" \]\]; then/ { in_full=1 } in_full && /elif \[\[ -f "\$gif_out" \]\]/ { exit } in_full { print }' "$capture_seed") 'n // 120' \
  'full-fidelity branch never uses n // 120 frame sampling'
assert_matches "$capture_seed" '(?m)^[[:blank:]]*elif \[\[ -f "\$gif_out" \]\] && command -v ffmpeg[^\n]*&& command -v python3[^\n]*; then[[:blank:]]*\n^[[:blank:]]+echo "==> ffmpeg mp4 via sampled frames[^\n]*\n(?:^.*\n)*?^step = max\(1, n // 120\)[[:blank:]]*$' \
  'default path preserves the existing sampled-preview encoder'
assert_matches "$capture_seed" '(?m)^conversion_rc=0[[:blank:]]*\n^if \[\[ -f "\$cast_dest" \]\]; then[[:blank:]]*\n^[[:blank:]]+if \[\[ "\$\{SPUR_CAPTURE_FULL_FIDELITY:-0\}" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+convert_cast "\$cast_dest" \|\| conversion_rc=\$\?[[:blank:]]*\n^[[:blank:]]+else[[:blank:]]*\n^[[:blank:]]+convert_cast "\$cast_dest" \|\| true[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^elif \[\[ "\$\{SPUR_CAPTURE_FULL_FIDELITY:-0\}" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+conversion_rc=1[[:blank:]]*\n^fi[[:blank:]]*$' \
  'full-fidelity conversion propagates failure while preview conversion stays tolerant'
assert_matches "$capture_seed" '(?m)^[[:blank:]]+# Fallback: newest matching cast by mtime[[:blank:]]*\n^[[:blank:]]+if \[\[ -z "\$cast_src" && "\$\{SPUR_CAPTURE_FULL_FIDELITY:-0\}" != "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+cast_src="\$\(find [^\n]*\n^[[:blank:]]+\| xargs [^\n]*\| head -1 \|\| true\)"[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*$' \
  'full-fidelity capture rejects archival cast fallback provenance'
assert_matches "$capture_seed" '(?m)^if \[\[ -f "\$mp4_out" \]\]; then[[:blank:]]*\n^[[:blank:]]+cp -p "\$mp4_out" "\$OUT/\$\{stem_prefix\}\.mp4"[[:blank:]]*\n^fi[[:blank:]]*\n(?:^\n)*^if \[\[ "\$\{SPUR_CAPTURE_FULL_FIDELITY:-0\}" == "1" \]\]; then[[:blank:]]*\n^[[:blank:]]+if \[\[ "\$conversion_rc" -ne 0 \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+rc=1[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^[[:blank:]]+for artifact in \\[[:blank:]]*\n^[[:blank:]]+"\$cast_dest" "\$gif_out" "\$mp4_out" \\[[:blank:]]*\n^[[:blank:]]+"\$OUT/\$\{stem_prefix\}\.cast" "\$OUT/\$\{stem_prefix\}\.gif" "\$OUT/\$\{stem_prefix\}\.mp4"; do[[:blank:]]*\n^[[:blank:]]+if \[\[ ! -s "\$artifact" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n|^\n)*?^[[:blank:]]+rc=1[[:blank:]]*\n^[[:blank:]]+fi[[:blank:]]*\n^[[:blank:]]+done[[:blank:]]*\n^fi[[:blank:]]*$' \
  'full-fidelity capture requires nonempty run and stable cast gif and mp4 after copies'
assert_has "$plan_loop" 'trigger_submit_plan_hitl_review_and_synthesize' \
  'PH journey still invokes the guarded campaign helper'
assert_matches "$root/capture-live-seed.sh" '(?m)^export SPUR_DEMO_CAPTURE_STEM_PREFIX="\$\{SPUR_DEMO_CAPTURE_STEM_PREFIX:-14-live-plan-loop-seed\}"[[:blank:]]*\n(?:^.*\n)*?^stem_prefix="\$SPUR_DEMO_CAPTURE_STEM_PREFIX"[[:blank:]]*$' \
  'D4 shared capture derives its output stem from the explicit override'
assert_matches "$root/capture-live-seed.sh" '(?m)^if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*\n(?:^[[:blank:]]+.*\n)*?^fi[[:blank:]]*\n^export SPUR_DEMO_ALLOW_PLAN_LOOP="\$\{SPUR_DEMO_ALLOW_PLAN_LOOP:-0\}"[[:blank:]]*$' \
  'D4 shared capture initializes the preserved plan-loop gate after HITL routing'
assert_matches "$root/capture-live-seed.sh" '(?m)^export SPUR_DEMO_ALLOW_HITL_LOOP="\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}"[[:blank:]]*\n(?:^.*\n)*?^if \[\[ -n "\$\{SPUR_DEMO_PROJECT:-\}" \]\]; then[[:blank:]]*\n^[[:blank:]]+capture_project="\$SPUR_DEMO_PROJECT"[[:blank:]]*\n^else[[:blank:]]*\n^[[:blank:]]+capture_project="\$\(git -C "\$E2E_ROOT/\.\./\.\." rev-parse --show-toplevel\)"[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^if \[\[ "\$SPUR_DEMO_ALLOW_HITL_LOOP" == "1" && ! -d "\$capture_project/\.beads" \]\]; then[[:blank:]]*\n(?:^.*\n)*?D4 requires a beads-backed project before TUI startup[^\n]*\n(?:^.*\n)*?SPUR_DEMO_PROJECT=/path/to/beads-project[^\n]*\n(?:^.*\n)*?^[[:blank:]]+exit 2[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^stamp=' \
  'D4 capture checks the effective project beads backend before launching the journey'
assert_matches "$root/capture-live-seed.sh" '(?m)^if \[\[ "\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}" != "1" \]\]; then[[:blank:]]*\n(?:^.*\n)*?^export SPUR_DEMO_ALLOW_PLAN_LOOP="\$\{SPUR_DEMO_ALLOW_PLAN_LOOP:-0\}"[[:blank:]]*\n^export SPUR_DEMO_ALLOW_HITL_LOOP="\$\{SPUR_DEMO_ALLOW_HITL_LOOP:-0\}"[[:blank:]]*\n(?:^.*\n)*?^if \[\[ "\$SPUR_DEMO_ALLOW_HITL_LOOP" == "1" && ! -d "\$capture_project/\.beads" \]\]; then[[:blank:]]*\n(?:^.*\n)*?D4 requires a beads-backed project before TUI startup[^\n]*\n(?:^.*\n)*?^[[:blank:]]+exit 2[[:blank:]]*\n^fi[[:blank:]]*\n(?:^.*\n)*?^if ! SPUR_BIN="\$\(spur_e2e_resolve_spur_bin\)"; then[[:blank:]]*$' \
  'D4 capture rejects a missing beads backend before resolving the binary'
assert_matches "$root/capture-live-seed.sh" '(?m)^[[:space:]]*cp -p "\$log" "\$OUT/\$\{stem_prefix\}\.log"[[:space:]]*$' \
  'D4 shared capture executes the full stable audit-log copy command'

if [[ "$failures" -ne 0 ]]; then
  printf '\n%d story-contract check(s) failed\n' "$failures" >&2
  exit 1
fi

printf '\nAll story-contract checks passed\n'
