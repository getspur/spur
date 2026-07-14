#!/usr/bin/env bash
# Live UAT: send a minimal brain turn and wait for reply (REAL model spend).
# Requires: SPUR_DEMO_ALLOW_AGENT_SEND=1
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

require_agent_send_opt_in

# Longer waits for model round-trip
timeout_ms="${SHELL_USE_TIMEOUT_MS:-60000}"

start_live_tui "agent-send"
wait_text "Lineage"

# Attach a free session (or N=new) — submit is more reliable in session detail.
attach_session_for_send
sleep_ms 0.8

# Type ASCII prompt and submit (Enter sends from session composer).
type_text "demo capture ping - reply with only the word ok"
expect_text "demo capture ping"
sleep_ms 0.5
press_key Enter

# Session detail with YOU turn + model reply
wait_text "YOU"
expect_text "demo capture ping"
set +e
run_su wait text "THINK" --timeout "$timeout_ms"
think_rc=$?
set -e
if [[ "$think_rc" -ne 0 ]]; then
  wait_text "ok"
else
  expect_text "ok"
fi
quit_live
