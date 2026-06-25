#!/usr/bin/env bash
set -uo pipefail
cd /Volumes/Projects/spur/.spur/worktrees/s3-verify || exit 99
LOG=/Volumes/Projects/spur/.spur/s3-serial.log
: > "$LOG"
echo "## serial-all-4 start $(date -u +%H:%M:%S) — all pending_sweep tests, --test-threads=1" >> "$LOG"
timeout 360 ./scripts/spur-cargo test -p spur-core --test pending_sweep -- --test-threads=1 --nocapture >>"$LOG" 2>&1
echo "## SERIAL DONE rc=$? $(date -u +%H:%M:%S)" >> "$LOG"
