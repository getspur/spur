#!/usr/bin/env bash
# PROBLEM: "I have (or will have) a submit_plan multi-agent campaign, but I
#           can't see what's running, how to drive brain↔worker navigation,
#           or capture worker outputs that feed the brain again."
#
# RESOLUTION (feature bond — Journey 3 brain/worker collaboration):
#   1) Plan browser — campaigns created via brain submit_plan (progress,
#      awaiting review, start/resume chrome)
#   2) Lineage Agents tree — Navigate mode: Tab→Agents, j/k select brain vs
#      EXEC workers, Enter focus, l cycle stream/artifacts/attempts/task/review
#   3) Activity log — live events as auto-loop progresses
#   4) Optional: SPUR_DEMO_ALLOW_AGENT_SEND=1 kicks a brain turn to observe
#      the loop under load; SPUR_DEMO_ALLOW_PLAN_START=1 presses Start/Resume
#
# NOTE: submit_plan is an MCP tool called *by the brain*, not a TUI button.
# This journey is the *operator control plane* over that auto loop.
#
# Personas: P3 platform eng (plan opacity), P1/P2 multi-agent visibility
# Docs: docs/spur-brain-worker-collaboration.md Journey 3
#       docs/rca/2026-04-23-submit-plan-end-to-end-map-territory-review.md
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

timeout_ms="${SHELL_USE_TIMEOUT_MS:-90000}"

start_live_tui "problem-plan-loop-drive"
wait_text "Lineage"
expect_text "Activity"
printf '== problem: drive submit_plan auto-loop from TUI control plane ==\n'

printf '\n== beat 1: ops orientation (what is running / how do I drive?) ==\n'
# Help is part of the same problem statement the user called out
type_text "?"
sleep_ms 0.5
wait_text "Dashboard"
press_key Escape
sleep_ms 0.35

printf '\n== beat 2: plan browser (submit_plan campaigns + progress) ==\n'
printf '== beat 3: lineage navigate brain ↔ worker + capture outputs ==\n'
story_plan_loop_control_plane

if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  printf '\n== beat 4: opt-in brain kick → re-observe lineage loop ==\n'
  trigger_brain_for_loop_observation
  navigate_lineage_brain_and_workers
else
  printf '\n== beat 4: skip brain kick (SPUR_DEMO_ALLOW_AGENT_SEND=1 to enable) ==\n'
fi

printf '\n== problem resolved: plan campaigns + lineage loop are operable ==\n'
quit_live
