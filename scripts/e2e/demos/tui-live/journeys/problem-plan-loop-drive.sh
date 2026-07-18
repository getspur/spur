#!/usr/bin/env bash
# PROBLEM: "I have (or will have) a submit_plan multi-agent campaign, but I
#           can't see what's running, how to drive brain↔worker work, or
#           capture worker outputs that feed the brain again."
#
# HOME SURFACE: Session Detail (not dashboard)
#   crates/spur-tui/src/views/session_detail
#
# RESOLUTION (feature bond — Journey 3 brain/worker collaboration):
#   1) Session Detail — composer + ReAct transcript (YOU / THINK / DELEGATE)
#   2) Inline workers panel (Alt+d) — workers spawned from this session
#   3) Alt+p plan inspector / Plans hub — campaign progress
#   4) Opt-in seed: SPUR_DEMO_ALLOW_PLAN_LOOP=1 asks brain for a 1-task
#      submit_plan and watches the same session stream for DELEGATE / Done
#
# NOTE: submit_plan is MCP (brain), not a TUI button. The operator *lives*
# in Session Detail while the loop runs; dashboard lineage is optional ops.
#
# Docs: docs/spur-brain-worker-collaboration.md Journey 3
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# Used dynamically by wait_text/quit_live functions sourced above.
# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-180000}"

start_live_tui "problem-plan-loop-drive"
story_beat "HOOK" "A submit_plan campaign should not disappear outside the session I work in."
story_session_land "Session Detail is the loop's operating surface" 3.0

story_beat "ORIENTATION" "Learn session controls, then workers and plan surfaces from home."
story_session_help

story_beat "ACTION" "Inspect workers and plan state without abandoning Session Detail."
story_plan_loop_control_plane

if [[ "${SPUR_DEMO_ALLOW_HITL_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "Real Product Hunt audit: three deep dives, evidence retry, approvals, then brain synthesis."
  trigger_submit_plan_hitl_review_and_synthesize
elif [[ "${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in seed: submit one safe task and watch DELEGATE/Done in this session."
  trigger_submit_plan_one_task_and_observe
elif [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in light kick: wake the brain, then re-check workers in session."
  trigger_brain_for_loop_observation
else
  printf '+ safe default: observe only; no brain turn or worker spend\n'
  printf '  SPUR_DEMO_ALLOW_HITL_LOOP=1 → three-worker Product Hunt audit + evidence retry\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_LOOP=1 → 1-task submit_plan + session wait\n'
  printf '  SPUR_DEMO_ALLOW_AGENT_SEND=1 → light brain kick only\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_START=1 → Start/Resume selected plan\n'
fi

story_beat "PROOF" "Session transcript + workers prove the loop; missing history stays labeled."
story_soft_proof \
  "Composer remains the send surface" \
  "INSERT" 1500 2.5 \
  "session chrome differs under load banners"
story_resolution "The operator drives submit_plan loops from Session Detail: compose, watch ReAct/workers, open plan inspector."
quit_live
