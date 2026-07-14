#!/usr/bin/env bash
# PROBLEM: "I have (or will have) a submit_plan multi-agent campaign, but I
#           can't see what's running, how to drive brain↔worker navigation,
#           or capture worker outputs that feed the brain again."
#
# RESOLUTION (feature bond — Journey 3 brain/worker collaboration):
#   1) Plan browser — campaigns created via brain submit_plan
#   2) Lineage Agents tree — Tab→Agents, j/k brain vs EXEC, Enter + detail tabs
#   3) Activity log — live auto-loop events
#   4) Opt-in seed: SPUR_DEMO_ALLOW_PLAN_LOOP=1 asks brain for a 1-task
#      submit_plan, waits for lineage EXEC/Running, re-walks brain↔worker
#
# NOTE: submit_plan is MCP (brain), not a TUI button. This journey is the
# operator control plane + optional live seed.
#
# Docs: docs/spur-brain-worker-collaboration.md Journey 3
#       docs/rca/2026-04-23-submit-plan-end-to-end-map-territory-review.md
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# Used dynamically by wait_text/quit_live functions sourced above.
# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-180000}"

start_live_tui "problem-plan-loop-drive"
story_beat "HOOK" "A submit_plan campaign should not disappear into a brain↔worker black box."
story_dashboard_land "The live dashboard is the loop's operating surface" 3.0
story_soft_proof \
  "Activity exposes existing loop events" \
  "Activity" 1200 2.0 \
  "no loop events exist yet; the optional seed is the cause→effect path"

story_beat "ORIENTATION" "Learn the controls, then locate campaigns created by the brain."
type_text "?"
story_hop 1.0 0.5
story_hard_proof "Dashboard help explains how to drive the control plane" "Dashboard" 2.5
press_key Escape
story_hop 0.8 0.35

story_beat "ACTION" "Inspect Plan progress, then follow BRAIN → EXEC → output → review → Activity."
story_plan_loop_control_plane

if [[ "${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in seed: submit one safe task and watch its EXEC row materialize."
  trigger_submit_plan_one_task_and_observe
elif [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in light kick: wake the brain, then re-walk existing lineage."
  trigger_brain_for_loop_observation
  navigate_lineage_brain_and_workers
else
  printf '+ safe default: observe only; no brain turn or worker spend\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_LOOP=1  → 1-task submit_plan + EXEC wait\n'
  printf '  SPUR_DEMO_ALLOW_AGENT_SEND=1 → light brain kick only\n'
  printf '  SPUR_DEMO_ALLOW_PLAN_START=1 → Start/Resume selected plan\n'
fi

story_beat "PROOF" "Existing history forms one trace; absent roles or output stay labeled behind the opt-in seed path."
story_soft_proof \
  "Activity closes the campaign-to-worker trace" \
  "Activity" 1200 2.5 \
  "no lineage was available; every unavailable proof was labeled instead"
story_resolution "The operator has a safe control path: inspect existing loops or opt in to seed cause-to-effect proof."
quit_live
