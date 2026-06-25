#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-retest2.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## retest2 tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S)" | tee -a "$LOG"
step() {
  local name="$1"; shift
  echo "" | tee -a "$LOG"
  echo "######## START [$name] $(date -u +%H:%M:%S) ########" | tee -a "$LOG"
  local t0=$SECONDS
  ( "$@" ) >>"$LOG" 2>&1
  local rc=$?
  local dt=$((SECONDS - t0))
  if [ $rc -eq 0 ]; then echo "######## PASS  [$name] rc=0 (${dt}s) ########" | tee -a "$LOG"
  else echo "######## FAIL  [$name] rc=$rc (${dt}s) ########" | tee -a "$LOG"
       grep -nE "over 60 seconds|test result: FAILED|panicked|--- .* (stdout|FAILED)" "$LOG" | tail -25; fi
  return $rc
}
step "test-spur-core-1"  ./scripts/spur-cargo test -p spur-core && \
step "test-spur-core-2"  ./scripts/spur-cargo test -p spur-core
rc=$?
echo "" | tee -a "$LOG"
echo "@@@@@@@@ RETEST2 DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
