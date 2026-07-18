#!/usr/bin/env bash
# Capture the higher-spend Product Hunt human-in-the-loop audit story.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SPUR_DEMO_ALLOW_HITL_LOOP=1
export SPUR_DEMO_ALLOW_PLAN_LOOP=0
export SPUR_DEMO_CAPTURE_STEM_PREFIX=16-live-product-hunt-audit-loop
export SPUR_DEMO_PLAN_LOOP_WAIT_S="${SPUR_DEMO_PLAN_LOOP_WAIT_S:-420}"
export SPUR_CAPTURE_FULL_FIDELITY=1
export SPUR_AGG_IDLE_LIMIT="${SPUR_AGG_IDLE_LIMIT:-6.0}"
exec "$ROOT/capture-live-seed.sh"
