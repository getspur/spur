#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s3-final.log
: > "$LOG"
echo "## final tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S) — spur-cargo test -p spur-core (report_signal active)" | tee -a "$LOG"
t0=$SECONDS
( SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 ./scripts/spur-cargo test -p spur-core ) >>"$LOG" 2>&1
rc=$?
echo "######## DONE rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"
echo "@@@@@@@@ FINAL DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
