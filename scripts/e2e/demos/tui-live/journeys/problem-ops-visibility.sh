#!/usr/bin/env bash
# PROBLEM: Multi-agent work is opaque — user cannot see what is running
#          or how to drive the TUI from where they actually work.
# RESOLUTION: Session Detail is home. Help + workers + Go to teach control;
#             ReAct proves activity; dashboard lineage is optional overview.
#
# HOME SURFACE: crates/spur-tui/src/views/session_detail
# Persona: P5 evaluator / P1 solo dev
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-ops-visibility"
story_beat "HOOK" "Multi-agent work is risky when I cannot see it in my session."
story_beat "ORIENTATION" "Session Detail (composer + ReAct + workers) is the primary surface."
story_session_land "The operator lands in Session Detail, not a chrome tour of the dashboard" 3.0
story_beat "ACTION" "Use session help, workers panel, and Go to; optional lineage is only overview."
story_ops_visibility
story_beat "PROOF" "Home remains Session Detail; empty transcript or empty workers stay labeled."
story_soft_proof \
  "Composer is still the send surface" \
  "INSERT" 1500 2.5 \
  "session chrome differs under load banners"
story_resolution "The operator can see work in Session Detail, learn controls, and open ops overview only when needed."
quit_live
