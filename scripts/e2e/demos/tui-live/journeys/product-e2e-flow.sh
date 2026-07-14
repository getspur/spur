#!/usr/bin/env bash
# PROBLEM: "I need a specialist persona + the right model/effort for this
#           task — without reinventing agents or losing session context."
#
# RESOLUTION (feature bond):
#   1) Recover / switch session context (multi-session work)
#   2) Explore: browse ecosystem skills+agents → gate → apply to pool
#      (answer: "where do specialists come from?")
#   3) Compose @worker cascade: worker → agent profile → model → effort
#      (answer: "how do I dispatch the right specialist?")
#   4) Optional send (SPUR_DEMO_ALLOW_AGENT_SEND=1)
#
# Persona: P1 solo dev "multiply myself" + explore design journey
# Grounded in:
#   - docs/superpowers/specs/2026-07-07-explore-command-design.md §2
#   - Mentions cascading worker→agent→model→effort design
#   - docs/rca/2026-04-17-persona-journey-review.md (P1 single-agent ceiling)
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

timeout_ms="${SHELL_USE_TIMEOUT_MS:-45000}"

start_live_tui "product-e2e-flow"
story_beat "HOOK" "A specialist should be configurable in seconds without throwing away session context."
story_dashboard_land "The current work remains anchored in the live dashboard" 2.5
expect_text "INSERT"

story_beat "ORIENTATION" "Session history preserves context while the operator chooses where to work."
resume_prior_session_context

story_beat "ACTION" "Adopt a trusted skill and agent from Explore into the local specialist pool."
explore_adopt_skill_and_agent

story_beat "PROOF" "Compose one @worker atom that names persona, model, and effort explicitly."
compose_live_worker_cascade

if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  story_beat "ACTION" "Opt-in only: send the configured delegated turn."
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

story_resolution "Context is preserved and the right specialist is ready to dispatch with explicit model/effort."
quit_live
