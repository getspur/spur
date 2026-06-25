#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s3-rsig.log
: > "$LOG"
echo "## rsig start $(date -u +%H:%M:%S) — report_signal_tool ALONE" >> "$LOG"
SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 timeout 420 ./scripts/spur-cargo test -p spur-core --test report_signal_tool -- --nocapture >>"$LOG" 2>&1
echo "## RSIG DONE rc=$? $(date -u +%H:%M:%S) (124=our-timeout=>hang-alone)" >> "$LOG"
