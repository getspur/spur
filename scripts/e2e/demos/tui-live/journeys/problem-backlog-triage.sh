#!/usr/bin/env bash
# PROBLEM: Backlog firehose — user cannot see what is P0 / open right now
#          without leaving the session they work in.
# RESOLUTION: From Session Detail → Issues via Go to; Enter opens detail.
#
# HOME SURFACE: Session Detail
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-backlog-triage"
story_beat "HOOK" "A backlog firehose hides the one issue that needs action now."
story_session_land "Triage starts from Session Detail, not a separate app mode" 2.5
story_beat "ORIENTATION" "Open Issues from Go to without abandoning the workspace model."
story_beat "ACTION" "Find open P0 work, then open its decision context."
story_backlog_triage
story_beat "PROOF" "P0/open/ID and status/priority prove urgency when present; empty P0 is explicit."
story_resolution "Urgent work is isolated when present; the operator still has a Session Detail home to return to."
return_to_session_detail
quit_live
