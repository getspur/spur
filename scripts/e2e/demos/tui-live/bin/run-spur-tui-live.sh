#!/usr/bin/env bash
# Launch SPUR TUI against a *real* project workspace (no e2e fixture isolation).
#
# Safety defaults:
#   - lands with `tui --dashboard` (no auto-resume of last session)
#   - does NOT create a temp workspace and does NOT rm -rf anything
#   - does NOT send prompts / dispatch workers (tapes should stay navigation-only)
#
# Env:
#   SPUR_DEMO_PROJECT   absolute path to project root (default: monorepo root)
#   SPUR_DEMO_TUI_ARGS  override argv after spur bin (default: "tui --dashboard")
#   SPUR_BIN            spur binary (default: resolve via e2e lib)
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
e2e_root="$(cd "$script_dir/../../../" && pwd)"
# shellcheck disable=SC1091
source "$e2e_root/lib/spur-bin.sh"

if [[ -n "${SPUR_DEMO_PROJECT:-}" ]]; then
  project="$SPUR_DEMO_PROJECT"
else
  project="$(git -C "$e2e_root/../.." rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -z "$project" ]]; then
    project="$(cd "$e2e_root/../.." && pwd)"
  fi
fi

if [[ ! -d "$project" ]]; then
  printf 'error: SPUR_DEMO_PROJECT is not a directory: %s\n' "$project" >&2
  exit 2
fi
if [[ ! -d "$project/.spur" ]]; then
  printf 'error: %s has no .spur/ — not a SPUR project\n' "$project" >&2
  exit 2
fi

spur_bin="$(spur_e2e_resolve_spur_bin)"

export SPUR_NO_UPGRADE_CHECK="${SPUR_NO_UPGRADE_CHECK:-1}"
export SPUR_TUI_MOUSE_CAPTURE="${SPUR_TUI_MOUSE_CAPTURE:-0}"

# Keep mouse off for VHS; leave HOME/XDG alone so license + agent CLIs work.
cd "$project"

# shellcheck disable=SC2086
set +e
if [[ -n "${SPUR_DEMO_TUI_ARGS:-}" ]]; then
  "$spur_bin" $SPUR_DEMO_TUI_ARGS
else
  "$spur_bin" tui --dashboard
fi
status=$?
set -e

printf '\033[2J\033[H'
printf 'VHS_SPUR_EXITED status=%s project=%s\n' "$status" "$project"
exit "$status"
