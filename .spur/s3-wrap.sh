#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-wrap.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## wrap-e2e tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S) — bare 'spur-cargo test -p spur-core'" | tee -a "$LOG"
t0=$SECONDS
( SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 ./scripts/spur-cargo test -p spur-core ) >>"$LOG" 2>&1
rc=$?
echo "######## DONE rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"
echo "@@@@@@@@ WRAP DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
