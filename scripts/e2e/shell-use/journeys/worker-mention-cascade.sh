#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_with_catalog "worker-mention-cascade"
wait_text "Type a task below"
compose_worker_mention_cascade

# Clear the composed atom (atom + trailing space) so the quit flow starts
# from an empty composer.
press_key Backspace
press_key Backspace
press_key Backspace
press_key Escape
quit_cleanly
