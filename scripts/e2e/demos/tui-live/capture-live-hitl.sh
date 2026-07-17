#!/usr/bin/env bash
# Capture the higher-spend D4 human-in-the-loop plan story.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_DEMO_ALLOW_HITL_LOOP=1
export SPUR_DEMO_ALLOW_PLAN_LOOP=0
export SPUR_DEMO_CAPTURE_STEM_PREFIX=15-live-hitl-agent-loop
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-300}"
exec "$ROOT/capture-live-seed.sh"
