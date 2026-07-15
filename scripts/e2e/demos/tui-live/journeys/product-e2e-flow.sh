#!/usr/bin/env bash
# PROBLEM: "I need a specialist persona + the right model/effort for this
#           task — without reinventing agents or losing session context."
#
# HOME SURFACE: Session Detail
# RESOLUTION (feature bond):
#   1) Land / resume in Session Detail (context continuity)
#   2) Explore: adopt skill+agent into pool (from session via Go to)
#   3) Compose @worker cascade in the session composer
#   4) Optional send (SPUR_DEMO_ALLOW_AGENT_SEND=1)
#
# Persona: P1 solo dev "multiply myself" + explore design journey
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

timeout_ms="${SHELL_USE_TIMEOUT_MS:-45000}"

start_live_tui "product-e2e-flow"
story_beat "HOOK" "A specialist should be configurable in the session I already work in."
story_session_land "Session Detail preserves the operator's workspace" 2.5

story_beat "ORIENTATION" "Session history / attach proves context continuity before reconfiguration."
# start_live_tui already lands a session; re-assert and optionally hop history
if soft_has_text "Session ·" 1500; then
  printf '+ proof: already home in Session Detail\n'
  story_dwell 2.0
else
  resume_prior_session_context
fi

story_beat "ACTION" "Adopt a trusted skill and agent from Explore into the local specialist pool."
explore_adopt_skill_and_agent
return_to_session_detail

story_beat "PROOF" "Compose one @worker atom that names persona, model, and effort in the session composer."
compose_live_worker_cascade

if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in only: send the configured delegated turn from Session Detail."
  type_text " reply with only the word ok"
  expect_text "reply with only the word ok"
  sleep_ms 0.5
  press_key Enter
  wait_text "YOU"
  set +e
  run_su wait text "THINK" --timeout "$timeout_ms"
  run_su wait text "ok" --timeout "$timeout_ms"
  set -e
else
  printf '+ safe default: configured specialist remains a draft; no model spend\n'
fi

story_resolution "Context is preserved in Session Detail and the right specialist is ready to dispatch with explicit model/effort."
quit_live
