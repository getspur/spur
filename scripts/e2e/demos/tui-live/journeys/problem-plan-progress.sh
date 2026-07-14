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

# Used dynamically by wait_text/quit_live functions sourced above.
# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-plan-progress"
story_beat "HOOK" "A multi-task campaign cannot be managed when its state is scattered across workers."
story_dashboard_land "The dashboard anchors the operator before campaign triage" 2.5
story_beat "ORIENTATION" "Open Plans to turn campaign history into one progress surface."
story_beat "ACTION" "Inspect lifecycle state, objective, tasks, and filtered ownership."
story_plan_progress
story_beat "PROOF" "Campaign rows expose Progress, Work item, and Tasks; No plans found is an explicit next-step state."
expect_text "Plans"
story_resolution "Campaign state is visible as one decision surface, including an honest empty path."
quit_live
