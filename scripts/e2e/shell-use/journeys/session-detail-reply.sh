#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_with_catalog "session-detail-reply"
wait_text "Type a task below"
compose_worker_mention_cascade

type_text " summarize this e2e reply"
press_key Enter

wait_text "e2e canned reply from fake worker"

quit_cleanly
