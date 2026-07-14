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

timeout_ms="${SHELL_USE_TIMEOUT_MS:-30000}"

start_live_tui "problem-backlog-triage"
wait_text "Lineage"
printf '== beat: land, then triage open P0 issues ==\n'
story_backlog_triage
printf '== problem-backlog-triage complete ==\n'
quit_live
