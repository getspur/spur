#!/usr/bin/env bash
# Long product E2E on a real project (single continuous shell-use session):
#
#   1) Land on lineage dashboard
#   2) Switch between sessions (multi-session UX)
#   3) Open Explore → star Skills + Agents → gate accept → apply to pool
#   4) Attach a free session and compose @worker cascade
#        worker → agent profile → model → effort
#   5) Optional: send a short turn (SPUR_DEMO_ALLOW_AGENT_SEND=1)
#
# Grounded in:
#   - docs/superpowers/specs/2026-07-07-explore-command-design.md §2 journey
#   - ExploreBrowserView browse/gate keys (space, Tab, Enter, c, m)
#   - Mentions cascading worker→agent→model→effort design
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# Long waits for gate apply + cascade
timeout_ms="${SHELL_USE_TIMEOUT_MS:-45000}"

start_live_tui "product-e2e-flow"
wait_text "Lineage"
expect_text "INSERT"
printf '\n== beat 1: lineage landing ==\n'

printf '\n== beat 2: switch between sessions ==\n'
switch_between_sessions

printf '\n== beat 3: explore adopt skill + agent → pool ==\n'
explore_adopt_skill_and_agent

printf '\n== beat 4: delegate with worker cascade (profile/model/effort) ==\n'
compose_live_worker_cascade

if [[ "${SPUR_DEMO_ALLOW_AGENT_SEND:-0}" == "1" ]]; then
  printf '\n== beat 5: send delegated turn (opt-in) ==\n'
  # Append a short task after the mention atom + space
  type_text " reply with only the word ok"
  expect_text "reply with only the word ok"
  sleep_ms 0.5
  press_key Enter
  wait_text "YOU"
  set +e
  run_su wait text "THINK" --timeout "$timeout_ms"
  set -e
  set +e
  run_su wait text "ok" --timeout "$timeout_ms"
  set -e
else
  printf '\n== beat 5: skip send (set SPUR_DEMO_ALLOW_AGENT_SEND=1 to enable) ==\n'
fi

printf '\n== product e2e flow complete ==\n'
quit_live
