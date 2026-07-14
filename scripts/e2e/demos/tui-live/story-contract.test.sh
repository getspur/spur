#!/usr/bin/env bash
# Static contract for the five value films. The live UAT remains the runtime proof.
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
assert_has "$lib" 'story_dashboard_land() {' 'dashboard landing supports active and empty lineage'
assert_has "$lib" 'story_resolution() {' 'shared resolution helper'
assert_has "$lib" 'Type a task below' 'empty dashboard has a stable landing anchor'
assert_has "$lib" 'No plans found' 'empty plan browser has a labeled soft anchor'
assert_has "$lib" 'No sessions yet' 'empty session picker has a labeled soft anchor'
assert_count_at_least "$lib" 'local has_plan_rows=0' 2 'plan stories guard row proof behind non-empty state'
assert_has "$lib" 'for _ in 1 2 3 4; do' 'empty Mine filter cycles fully back to All'
assert_lacks "$lib" 'wait_text "Lineage"' 'shared story helpers never hard-fail on empty lineage'
assert_lacks "$lib" 'run_su wait text "Session ·"' 'recoverable session attach checks stay quiet'
assert_count_at_least "$lib" 'start_clean_session_for_draft' 2 'repeatable cascade starts from a clean local session'
assert_has "$lib" 'for candidate_row in 1 2 3 4; do' 'clean-session selection uses a bounded draft-safe scan'
assert_has "$lib" 'refusing to overwrite' 'clean-session selection fails safely when every composer has a draft'
assert_has "$lib" 'configured worker draft already exists' 'repeat runs reuse completed worker proof without clearing it'
assert_has "$lib" 'resume_prior_session_context() {' 'product continuity uses one provable prior-session resume'
assert_lacks "$lib" 'session A attached' 'product continuity never invents distinct session A'
assert_lacks "$lib" 'session B attached' 'product continuity never invents distinct session B'
assert_has "$lib" 'status: open' 'backlog detail binds proof to open status'
assert_has "$lib" 'priority: P0' 'backlog detail binds proof to P0 priority'
assert_has "$lib" 'error: backlog unavailable' 'backlog load failures never masquerade as empty queues'
assert_lacks "$lib" 'Brain transcript acknowledges plan submission' 'seed prompt text never claims brain-result proof'
assert_lacks "$lib" 'seeded campaign produced EXEC/Running' 'generic lineage never claims seed correlation'
# shellcheck disable=SC2016 # assert the literal per-run shell tag
assert_has "$lib" 'seed_task_id="demo-echo-$$"' 'plan-loop seed uses a per-run correlation tag'
assert_has "$lib" "type_slow \"@worker:\${worker}\"" 'worker cascade avoids paste burst'
assert_lacks "$lib" 'press_key 1' 'Agents focus never uses digit 1'

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
  assert_has "$file" 'story_dashboard_land' "$story accepts an empty-lineage dashboard"
  assert_has "$file" 'story_resolution' "$story resolves the opening problem"
done

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
  case "$tape" in
    10-problem-ops-visibility.tape | 13-problem-plan-loop-drive.tape)
      assert_has "$file" 'Wait+Screen@30s /Lineage/' "$tape stops safely without seeded lineage"
      ;;
    *)
      assert_has "$file" '/Lineage|Type a task below/' "$tape accepts active or empty-lineage landing"
      ;;
  esac
done

assert_has "$root/journeys/product-e2e-flow.sh" 'SPUR_DEMO_ALLOW_AGENT_SEND' 'product send remains opt-in'
assert_has "$root/journeys/problem-plan-loop-drive.sh" 'SPUR_DEMO_ALLOW_PLAN_LOOP' 'plan-loop seed remains opt-in'
assert_has "$root/../geometry.env" ": \"\${SPUR_DEMO_STORY_PACE:=0}\"" 'story dwell stays off by default'
assert_lacks "$root/tapes/10-problem-ops-visibility.tape" 'Type "1"' 'ops tape uses Tab for Agents'
assert_lacks "$root/tapes/13-problem-plan-loop-drive.tape" 'Type "1"' 'plan-loop tape uses Tab for Agents'
assert_has "$root/tapes/09-product-e2e-flow.tape" 'Wait+Screen@10s /TODAY/' 'product tape proves real session history'
assert_has "$root/tapes/09-product-e2e-flow.tape" '# resume the highlighted prior session' 'product tape demonstrates context continuity'
assert_has "$root/tapes/11-problem-plan-progress.tape" '/Progress|No plans found/' 'plan-progress tape accepts campaign rows or honest empty state'
assert_has "$root/tapes/12-problem-backlog-triage.tape" 'Wait+Screen@12s /status: open/' 'backlog tape proves selected detail is open'
assert_has "$root/tapes/12-problem-backlog-triage.tape" 'Wait+Screen@12s /priority: P0/' 'backlog tape proves selected detail is P0'
assert_has "$root/tapes/13-problem-plan-loop-drive.tape" '/Progress|No plans found/' 'plan-loop tape accepts campaign rows or honest empty state'
assert_has "$root/tapes/13-problem-plan-loop-drive.tape" 'Wait+Screen@10s /Activity/' 'plan-loop tape resolves on Activity proof'

if [[ "$failures" -ne 0 ]]; then
  printf '\n%d story-contract check(s) failed\n' "$failures" >&2
  exit 1
fi

printf '\nAll story-contract checks passed\n'
