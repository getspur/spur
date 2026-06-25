#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-serial-full.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## serial-full tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S) (--test-threads=1)" | tee -a "$LOG"
echo "######## START [spur-core --test-threads=1] $(date -u +%H:%M:%S) ########" | tee -a "$LOG"
t0=$SECONDS
( SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 ./scripts/spur-cargo test -p spur-core -- --test-threads=1 ) >>"$LOG" 2>&1
rc=$?
echo "######## DONE rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"
echo "@@@@@@@@ SERIALFULL DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
