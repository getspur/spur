#!/usr/bin/env bash
# Live UAT: focus composer and type a draft (does NOT send — no model spend).
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

start_live_tui "composer-draft"
story_session_land "Session Detail exposes the spend-free draft surface"
press_key Enter
sleep_ms 0.25
type_text "draft only - do not send (demo capture)"
expect_text "draft only"

# Prove session switching protects an unsent draft. `n` targets a new session;
# the second `n` cancels the confirmation without sending or mutating metadata.
open_sessions_picker
press_key n
wait_text "has an unsent draft"
expect_text "save and start a new session"
press_key n
wait_text "Sessions"
quit_live
