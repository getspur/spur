#!/usr/bin/env bash
# Demo script: spur context workstation setup (CLI surface).
# Safe for VHS / marketing capture — no secrets printed.
set -euo pipefail

SPUR_BIN="${SPUR_BIN:-spur}"
if [[ -n "${SPUR_DEMO_SPUR_BIN:-}" ]]; then
  SPUR_BIN="$SPUR_DEMO_SPUR_BIN"
fi

bold=$'\033[1m'
cyan=$'\033[36m'
dim=$'\033[2m'
reset=$'\033[0m'
green=$'\033[32m'

banner() {
  printf '\n%s%s%s\n' "${bold}${cyan}" "$1" "${reset}"
  printf '%s%s%s\n' "${dim}" "--------------------------------------------------------" "${reset}"
}

pause() {
  # Visual breathing room for VHS; override with SPUR_DEMO_PAUSE=0 for tests.
  sleep "${SPUR_DEMO_PAUSE:-0.6}"
}

clear 2>/dev/null || true
printf '%sSPUR Context Service — CLI setup demo%s\n' "${bold}" "${reset}"
printf '%sCloud code context for third-party packages%s\n' "${dim}" "${reset}"
pause

banner "1) Discover the command surface"
printf '%s$ %s context --help%s\n' "${dim}" "$SPUR_BIN" "${reset}"
"$SPUR_BIN" context --help
pause

banner "2) Personal keys (management vs day-to-day MCP)"
printf '%s$ %s context key --help%s\n' "${dim}" "$SPUR_BIN" "${reset}"
"$SPUR_BIN" context key --help
pause

banner "3) Start the external_* MCP server (stdio)"
printf '%s$ %s context mcp --help%s\n' "${dim}" "$SPUR_BIN" "${reset}"
"$SPUR_BIN" context mcp --help
pause

banner "4) First-time workstation checklist (from PRODUCT_AND_USAGE)"
cat <<'EOF'
  # Sign in once (browser OAuth — management only)
  spur context auth login --profile workstation

  # Create a personal API key for routine external_* traffic
  spur context key create \
    --name workstation \
    --scope external.read \
    --profile workstation

  # Select the key, then serve MCP
  spur context key use <PUBLIC_KEY_ID>
  spur context mcp --profile <PUBLIC_KEY_ID>
EOF
pause

printf '\n%s✓ Setup surface ready — next: external_knowledge_context → external_code_read%s\n' \
  "${green}" "${reset}"
printf '%sOrigin default: https://context.getspur.dev%s\n\n' "${dim}" "${reset}"
