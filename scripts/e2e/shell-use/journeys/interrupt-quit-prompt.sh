#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_with_catalog "interrupt-quit-prompt"
wait_text "Type a task below"
compose_worker_mention_cascade

type_text " check interrupt handling"
press_key Enter
press_key Ctrl+C
wait_text "Quit spur?"

# Decline the quit prompt; the in-flight fake worker turn should keep the
# session alive and render its canned reply.
press_key n
wait_text "e2e canned reply"

quit_cleanly
