#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s4-remote.log
: > "$LOG"
echo "## s4-remote tip $(git rev-parse --short HEAD) start $(date -u +%H:%M:%S) — REMOTE(aws-my) test spur-core + spur-mcp" | tee -a "$LOG"
t0=$SECONDS
( SPUR_REMOTE=1 SPUR_CLOUD=aws-my SPUR_CLOUD_FALLBACK=aws SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 CARGO_BUILD_JOBS=4 \
    ./scripts/spur-cargo test -p spur-core -p spur-mcp ) >>"$LOG" 2>&1
rc=$?
echo "######## DONE rc=$rc ($((SECONDS-t0))s) ########" | tee -a "$LOG"
echo "@@@@@@@@ S4REMOTE DONE overall_rc=$rc $(date -u +%H:%M:%S) @@@@@@@@" | tee -a "$LOG"
exit $rc
