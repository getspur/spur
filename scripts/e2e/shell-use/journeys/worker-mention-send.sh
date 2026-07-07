#!/usr/bin/env bash
set -euo pipefail

journey_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_E2E_FIXTURE="worker-mentions"
# shellcheck disable=SC1091
source "$journey_dir/../lib.sh"
# shellcheck disable=SC1091
source "$journey_dir/worker-mention-common.sh"

launch_spur_tui_with_catalog "worker-mention-send"
wait_text "Type a task below"
compose_worker_mention_cascade

type_text " fix the parser bug"
press_key Enter

# Send path: the composer prepends the [UI hint] block naming the worker
# tuple, and the message (hint + task text) must land in the session
# transcript backed by the fake ACP worker. The transcript wraps the
# hint + atom + task run at 80 cols, so assert fragments that stay on
# one line ("fix the parser bug" straddles the wrap).
wait_text "User-suggested workers"
expect_text "agent=rust-reviewer, model=gpt-5-codex, effort=high"
expect_text "parser bug"

quit_cleanly
