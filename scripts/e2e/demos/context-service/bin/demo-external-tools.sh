#!/usr/bin/env bash
# Demo script: external_knowledge_context → external_code_read multi-round flow.
#
# Modes:
#   fixture (default) — pretty-print recorded responses (reproducible marketing capture)
#   live              — call context service via spur context mcp (needs API key)
#
# Usage:
#   ./bin/demo-external-tools.sh
#   SPUR_DEMO_MODE=live SPUR_CONTEXT_SERVICE_API_KEY=… ./bin/demo-external-tools.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${SPUR_DEMO_MODE:-fixture}"
PACKAGE="${SPUR_DEMO_PACKAGE:-serde}"
REVISION="${SPUR_DEMO_REVISION:-1.0.197}"
SOURCE="${SPUR_DEMO_SOURCE:-registry:crates-io}"
QUERY="${SPUR_DEMO_QUERY:-how does Deserialize deserialize work}"
PAUSE="${SPUR_DEMO_PAUSE:-0.5}"

bold=$'\033[1m'
cyan=$'\033[36m'
dim=$'\033[2m'
reset=$'\033[0m'
green=$'\033[32m'
yellow=$'\033[33m'
magenta=$'\033[35m'

pause() { sleep "$PAUSE"; }

banner() {
  printf '\n%s%s%s\n' "${bold}${cyan}" "$1" "${reset}"
  printf '%s%s%s\n' "${dim}" "--------------------------------------------------------" "${reset}"
}

json_tool() {
  # Pretty-print JSON if jq is available.
  if command -v jq >/dev/null 2>&1; then
    jq -C "${2:-.}" <<<"$1"
  else
    printf '%s\n' "$1"
  fi
}

call_live_tool() {
  local tool="$1"
  local args_json="$2"
  # Minimal MCP stdio client: initialize + tools/call against spur context mcp.
  # Requires SPUR_CONTEXT_SERVICE_API_KEY or a stored key profile.
  python3 - "$tool" "$args_json" <<'PY'
import json, os, subprocess, sys

tool, args_raw = sys.argv[1], sys.argv[2]
args = json.loads(args_raw)
spur = os.environ.get("SPUR_DEMO_SPUR_BIN") or os.environ.get("SPUR_BIN") or "spur"
profile = os.environ.get("SPUR_DEMO_PROFILE")
cmd = [spur, "context", "mcp"]
if profile:
    cmd.extend(["--profile", profile])

proc = subprocess.Popen(
    cmd,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)

def send(msg: dict) -> None:
    line = json.dumps(msg)
    assert proc.stdin is not None
    proc.stdin.write(line + "\n")
    proc.stdin.flush()

def recv() -> dict:
    assert proc.stdout is not None
    while True:
        line = proc.stdout.readline()
        if not line:
            err = proc.stderr.read() if proc.stderr else ""
            raise SystemExit(f"MCP closed unexpectedly. stderr={err!r}")
        line = line.strip()
        if not line:
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue

send({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "spur-context-demo", "version": "0.1.0"},
    },
})
init = recv()
if "error" in init:
    raise SystemExit(json.dumps(init, indent=2))

send({"jsonrpc": "2.0", "method": "notifications/initialized"})

send({
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {"name": tool, "arguments": args},
})
result = recv()
proc.terminate()
try:
    proc.wait(timeout=2)
except Exception:
    proc.kill()

if "error" in result:
    raise SystemExit(json.dumps(result, indent=2))

content = result.get("result", {}).get("content") or []
# Prefer structured text payload; fall back to whole result.
for block in content:
    if block.get("type") == "text" and block.get("text"):
        text = block["text"]
        try:
            print(json.dumps(json.loads(text), indent=2))
        except Exception:
            print(text)
        break
else:
    print(json.dumps(result.get("result", result), indent=2))
PY
}

print_fixture_knowledge() {
  local path="$ROOT/fixtures/external_knowledge_context.serde.json"
  json_tool "$(cat "$path")"
}

print_fixture_read() {
  local path="$ROOT/fixtures/external_code_read.serde.json"
  json_tool "$(cat "$path")"
}

clear 2>/dev/null || true
printf '%sSPUR Context Service — external_* multi-round demo%s\n' "${bold}" "${reset}"
printf '%smode=%s  package=%s@%s  source=%s%s\n' \
  "${dim}" "$MODE" "$PACKAGE" "$REVISION" "$SOURCE" "${reset}"
pause

banner "Plane check"
cat <<EOF
  Question about THIS worktree?     → knowledge_context_pack_2 / code_*
  Question about a dependency?      → external_*  (this demo)
  Revision not indexed yet?         → external_index → external_index_status
EOF
pause

banner "Round 1 — Orient: external_knowledge_context"
printf '%s→ tool: external_knowledge_context%s\n' "${magenta}" "${reset}"
printf '%sargs:%s\n' "${dim}" "${reset}"
args_knowledge=$(cat <<EOF
{
  "package": "$PACKAGE",
  "query": "$QUERY",
  "revision": "$REVISION",
  "source": "$SOURCE",
  "scope": "all",
  "limit": 8
}
EOF
)
json_tool "$args_knowledge"
pause

printf '\n%s← evidence pack%s\n' "${green}" "${reset}"
if [[ "$MODE" == "live" ]]; then
  call_live_tool "external_knowledge_context" "$args_knowledge"
else
  printf '%s(fixture mode — recorded warm hit for serde@1.0.197)%s\n' "${yellow}" "${reset}"
  print_fixture_knowledge
fi
pause

SELECTOR="${SPUR_DEMO_SELECTOR:-pkg:serde@1.0.197::Deserialize}"

banner "Round 2 — Precision: external_code_read"
printf '%sCarry the returned selector → external_code_read%s\n' "${dim}" "${reset}"
printf '%s→ tool: external_code_read%s\n' "${magenta}" "${reset}"
args_read=$(cat <<EOF
{
  "selector": "$SELECTOR",
  "context_lines": 3
}
EOF
)
json_tool "$args_read"
pause

printf '\n%s← pinned source%s\n' "${green}" "${reset}"
if [[ "$MODE" == "live" ]]; then
  call_live_tool "external_code_read" "$args_read"
else
  printf '%s(fixture mode — Deserialize trait body)%s\n' "${yellow}" "${reset}"
  print_fixture_read
fi
pause

banner "Recommended next (from PRODUCT_AND_USAGE)"
cat <<EOF
  external_code_callers  { "selector": "$SELECTOR", "include_unresolved": true }
  external_code_callees  { "selector": "$SELECTOR", "include_unresolved": false }
  external_code_search   { "package": "$PACKAGE", "query": "Deserialize", "revision": "$REVISION" }
EOF

printf '\n%s✓ Version-precise dependency context without cloning the crate%s\n\n' \
  "${green}" "${reset}"
