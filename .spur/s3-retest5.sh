#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-retest5.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## retest5 tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S) (remote, 1 pass, local-fallback cap=4)" | tee -a "$LOG"
echo "######## START [test-spur-core-1] $(date -u +%H:%M:%S) ########" | tee -a "$LOG"
t0=$SECONDS
( SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 ./scripts/spur-cargo test -p spur-core ) >>"$LOG" 2>&1
rc=$?
echo "######## DONE [test-spur-core-1] rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"
echo "@@@@@@@@ RETEST5 DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
