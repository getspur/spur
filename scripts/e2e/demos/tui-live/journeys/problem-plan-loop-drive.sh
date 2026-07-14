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

timeout_ms="${SHELL_USE_TIMEOUT_MS:-180000}"

start_live_tui "problem-plan-loop-drive"
wait_text "Lineage"
expect_text "Activity"
printf '== problem: drive submit_plan auto-loop from TUI control plane ==\n'

printf '\n== beat 1: ops orientation (what is running / how do I drive?) ==\n'
type_text "?"
sleep_ms 0.5
wait_text "Dashboard"
press_key Escape
sleep_ms 0.35

printf '\n== beat 2: plan browser (existing submit_plan campaigns) ==\n'
printf '== beat 3: lineage navigate brain ↔ worker + capture outputs ==\n'
story_plan_loop_control_plane

if [[ "${SPUR_DEMO_ALLOW_PLAN_LOOP:-0}" == "1" ]]; then
  printf '\n== beat 4: LIVE seed 1-task submit_plan + wait EXEC + re-walk lineage ==\n'
  trigger_submit_plan_one_task_and_observe
elif [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  printf '\n== beat 4: light brain kick (set SPUR_DEMO_ALLOW_PLAN_LOOP=1 for full seed) ==\n'
  trigger_brain_for_loop_observation
  navigate_lineage_brain_and_workers
else
  printf '\n== beat 4: skip live seed ==\n'
  printf '   SPUR_DEMO_ALLOW_PLAN_LOOP=1  → 1-task submit_plan + EXEC wait\n'
  printf '   SPUR_DEMO_ALLOW_AGENT_SEND=1 → light brain kick only\n'
  printf '   SPUR_DEMO_ALLOW_PLAN_START=1 → Start/Resume selected plan\n'
fi

printf '\n== problem resolved: plan campaigns + lineage loop are operable ==\n'
quit_live
