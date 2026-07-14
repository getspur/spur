#!/usr/bin/env bash
# PROBLEM: Multi-agent work is opaque — user cannot see what is running
#          or how to drive the TUI.
# RESOLUTION: Lineage + Activity prove live work; Help teaches controls;
#             Palette is the hub to every other problem surface.
#
# Persona: P5 evaluator / P1 solo dev (docs/rca/2026-04-17-persona-journey-review.md)
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-ops-visibility"
story_ops_visibility
printf '== problem-ops-visibility complete ==\n'
quit_live
