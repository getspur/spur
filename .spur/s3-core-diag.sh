#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s3-core-diag.log
: > "$LOG"
echo "## diag start $(date -u +%H:%M:%S)" >> "$LOG"
SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 ./scripts/spur-cargo test -p spur-core >>"$LOG" 2>&1
rc=$?
echo "## DIAG DONE rc=$rc $(date -u +%H:%M:%S)" >> "$LOG"
exit $rc
