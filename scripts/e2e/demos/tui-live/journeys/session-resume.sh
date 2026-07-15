#!/usr/bin/env bash
# Live UAT: resume a real session and load message history / transcript.
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "session-resume"
story_session_land "Session Detail is ready before choosing history"
resume_session_skip_held
# History-loaded session detail
story_session_land "Resuming history returns to Session Detail"
# Soft: transcript chrome varies by session content
set +e
run_su expect text "following" --no-strict --timeout 3000
set -e
quit_live
