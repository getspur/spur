#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-retest.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## retest tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S)" | tee -a "$LOG"
overall=0
step() {
  local name="$1"; shift
  echo "" | tee -a "$LOG"
  echo "######## START [$name] $(date -u +%H:%M:%S) ########" | tee -a "$LOG"
  echo "+ $*" | tee -a "$LOG"
  local t0=$SECONDS
  ( "$@" ) >>"$LOG" 2>&1
  local rc=$?
  local dt=$((SECONDS - t0))
  if [ $rc -eq 0 ]; then echo "######## PASS  [$name] rc=0 (${dt}s) ########" | tee -a "$LOG"
  else echo "######## FAIL  [$name] rc=$rc (${dt}s) ########" | tee -a "$LOG"; tail -45 "$LOG"; overall=$rc; fi
  return $rc
}
step "clippy-core-tests" env SPUR_REMOTE=1 ./scripts/spur-cargo clippy -p spur-core --tests -- -D warnings && \
step "test-spur-core-1"  ./scripts/spur-cargo test -p spur-core && \
step "test-spur-core-2"  ./scripts/spur-cargo test -p spur-core
rc=$?
echo "" | tee -a "$LOG"
echo "@@@@@@@@ RETEST DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
