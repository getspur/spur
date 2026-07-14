#!/usr/bin/env bash
# PROBLEM: Multi-agent work is opaque — user cannot see what is running
#          or how to drive the TUI.
# RESOLUTION: The dashboard distinguishes no work from hidden work; when
#             lineage exists, Activity proves it. Help and Palette teach control.
#
# Persona: P5 evaluator / P1 solo dev (docs/rca/2026-04-17-persona-journey-review.md)
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# Used dynamically by wait_text/quit_live functions sourced above.
# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-ops-visibility"
story_beat "HOOK" "Multi-agent work is risky when the operator cannot see it or drive it."
story_beat "ORIENTATION" "Lineage and Activity answer what is running before any action is taken."
story_dashboard_land "The dashboard reveals live work or labels that no work exists yet" 3.0
story_beat "ACTION" "Use Help, Go to, and the Agents tree to move from overview to worker output."
story_ops_visibility
story_beat "PROOF" "The dashboard distinguishes no work from hidden work; populated lineage adds agent output and Activity."
story_soft_proof \
  "Activity preserves the operating timeline" \
  "Activity" 1200 2.5 \
  "no lineage exists, so worker output and Activity remain explicit soft beats"
story_resolution "The operator can see whether work exists, learn the controls, and inspect it when present."
quit_live
