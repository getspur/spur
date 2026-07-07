#!/usr/bin/env bash
set -euo pipefail

spur_e2e_bin_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
spur_e2e_bin_repo_root="$(git -C "$spur_e2e_bin_lib_dir" rev-parse --show-toplevel)"

spur_e2e_print_build_hint() {
  cat >&2 <<'EOF'
Build the real binary with:
  SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli

In GitHub Actions:
  cargo build -p spur-cli --locked
EOF
}

spur_e2e_resolve_spur_bin() {
  local candidate

  if [[ -n "${SPUR_BIN:-}" ]]; then
    candidate="$SPUR_BIN"
  else
    candidate="$spur_e2e_bin_repo_root/target/debug/spur"
  fi

  if [[ ! -x "$candidate" ]]; then
    printf 'error: spur binary is not executable: %s\n\n' "$candidate" >&2
    spur_e2e_print_build_hint
    return 2
  fi

  printf '%s\n' "$candidate"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  spur_e2e_resolve_spur_bin
fi
