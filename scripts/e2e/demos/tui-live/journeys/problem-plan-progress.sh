#!/usr/bin/env bash
# PROBLEM: Multi-task campaigns are invisible — user cannot answer
#          "what's running / awaiting review / done?"
# RESOLUTION: Plan browser lists plans with Progress + summary pane
#             (Work item, Tasks) so the user can triage campaign state.
#
# Persona: P3 platform eng / plan-based delegation
#          (docs/spur-brain-worker-collaboration.md Journey 3)
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-plan-progress"
wait_text "Lineage"
printf '== beat: land, then open plan control surface ==\n'
story_plan_progress
printf '== problem-plan-progress complete ==\n'
quit_live
