#!/usr/bin/env bash
# PROBLEM: Multi-task campaigns are invisible — user cannot answer
#          "what's running / awaiting review / done?" from their session.
# RESOLUTION: From Session Detail → Alt+p plan inspector and/or Plans hub
#             with Progress + Work item summary.
#
# HOME SURFACE: Session Detail
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-plan-progress"
story_beat "HOOK" "A multi-task campaign cannot be managed when its state is outside my session."
story_session_land "Campaign triage starts from the operator's session" 2.5
story_beat "ORIENTATION" "Open plan inspector (Alt+p) or Plans via Go to from Session Detail."
story_beat "ACTION" "Inspect lifecycle state, objective, tasks, and filtered ownership."
story_plan_progress
story_beat "PROOF" "Campaign rows expose Progress, Work item, and Tasks; No plans found is an explicit next-step state."
expect_text "Plans"
story_resolution "Campaign state is visible as one decision surface reached from Session Detail."
return_to_session_detail
quit_live
