#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s3-repro.log
: > "$LOG"
echo "## repro start $(date -u +%H:%M:%S) — single test, alone, 1 thread" >> "$LOG"
# local timeout 360s bounds our wait; remote cargo runs on VM (compile cached)
timeout 360 ./scripts/spur-cargo test -p spur-core --test pending_sweep -- --exact startup_sweep_quarantines_all_plan_children_with_comments --nocapture --test-threads=1 >>"$LOG" 2>&1
rc=$?
echo "## REPRO DONE rc=$rc $(date -u +%H:%M:%S) (124=our-timeout=>hang-in-isolation)" >> "$LOG"
exit $rc
