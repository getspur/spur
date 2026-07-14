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
wait_text "Lineage"
expect_text "INSERT"
printf '\n== problem: specialist dispatch without context loss ==\n'
printf '== beat 1: land (ops surface) ==\n'

printf '\n== beat 2: recover/switch sessions (context continuity) ==\n'
switch_between_sessions

printf '\n== beat 3: explore adopt skill+agent → pool (specialist supply) ==\n'
explore_adopt_skill_and_agent

printf '\n== beat 4: @worker cascade profile/model/effort (dispatch precision) ==\n'
compose_live_worker_cascade

if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  printf '\n== beat 5: send delegated turn (opt-in spend) ==\n'
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
  printf '\n== beat 5: skip send (SPUR_DEMO_ALLOW_AGENT_SEND=1 to enable) ==\n'
fi

printf '\n== problem resolved: specialist ready to dispatch ==\n'
quit_live
