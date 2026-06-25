#!/usr/bin/env bash
set -uo pipefail
WT="/Volumes/Projects/spur/.spur/worktrees/s3-verify"
LOG="/Volumes/Projects/spur/.spur/s3-buildrun.log"
cd "$WT" || { echo "FATAL cd" | tee -a "$LOG"; exit 99; }
: > "$LOG"
echo "## buildrun tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S)" | tee -a "$LOG"
step() {
  local name="$1"; shift
  echo "" | tee -a "$LOG"
  echo "######## START [$name] $(date -u +%H:%M:%S) ########" | tee -a "$LOG"
  local t0=$SECONDS
  ( CARGO_BUILD_JOBS=4 "$@" ) >>"$LOG" 2>&1
  local rc=$?
  if [ $rc -eq 0 ]; then echo "######## PASS  [$name] rc=0 ($((SECONDS-t0))s) ########" | tee -a "$LOG"
  else echo "######## FAIL  [$name] rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"; fi
  return $rc
}
step "build-workspace" ./scripts/spur-cargo build --workspace && \
step "run-cli-help"    ./scripts/spur-cargo run -p spur-cli -- --help
rc=$?
echo "" | tee -a "$LOG"
echo "@@@@@@@@ BUILDRUN DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
