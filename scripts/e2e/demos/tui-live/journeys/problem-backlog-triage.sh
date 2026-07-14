#!/usr/bin/env bash
# PROBLEM: Backlog firehose — user cannot see what is P0 / open right now.
# RESOLUTION: Issues browser surfaces priority + status; Enter opens detail
#             (status/priority/labels) for triage without leaving the TUI.
#
# Persona: P2 tech lead / P4 OSS maintainer triage
#          (docs/rca/2026-04-17-persona-journey-review.md)
set -euo pipefail
journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"

# Used dynamically by wait_text/quit_live functions sourced above.
# shellcheck disable=SC2034
timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-backlog-triage"
story_beat "HOOK" "A backlog firehose hides the one issue that needs action now."
story_dashboard_land "The operator starts from the live control plane" 2.5
story_beat "ORIENTATION" "Open Issues to replace the firehose with priority and status."
story_beat "ACTION" "Find open P0 work, then open its decision context without leaving the TUI."
story_backlog_triage
story_beat "PROOF" "P0/open/ID and status/priority prove urgency when present; an empty P0 queue is explicit."
story_resolution "Urgent work is isolated when present; otherwise the operator has a trustworthy empty queue."
quit_live
