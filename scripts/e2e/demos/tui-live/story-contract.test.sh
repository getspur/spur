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

if [[ "$failures" -ne 0 ]]; then
  printf '\n%d story-contract check(s) failed\n' "$failures" >&2
  exit 1
fi

printf '\nAll story-contract checks passed\n'
