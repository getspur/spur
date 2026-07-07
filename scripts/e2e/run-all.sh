#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$script_dir/lib/spur-bin.sh"

only="${SPUR_E2E_ONLY:-all}"
case "$only" in
  all | behavioral | visual)
    ;;
  *)
    printf 'error: SPUR_E2E_ONLY must be behavioral or visual, got: %s\n' "$only" >&2
    exit 2
    ;;
esac

artifact_root="$script_dir/.artifacts"
rm -rf "$artifact_root"
mkdir -p "$artifact_root"

spur_bin="$(spur_e2e_resolve_spur_bin)"
export SPUR_BIN="$spur_bin"

status=0

copy_dir_if_present() {
  local src="$1"
  local dest="$2"

  if [[ -d "$src" ]]; then
    mkdir -p "$(dirname "$dest")"
    rm -rf "$dest"
    cp -a "$src" "$dest"
  fi
}

collect_shell_use_casts() {
  local dest="$artifact_root/behavioral/casts"
  local dir cast
  local cache_dirs=(
    "$HOME/Library/Caches/shell-use"
    "${XDG_CACHE_HOME:-"$HOME/.cache"}/shell-use"
  )

  mkdir -p "$dest"
  shopt -s nullglob
  for dir in "${cache_dirs[@]}"; do
    for cast in "$dir"/*.cast; do
      cp -p "$cast" "$dest/"
    done
  done
  shopt -u nullglob
}

collect_artifacts() {
  local suite="$1"

  case "$suite" in
    behavioral)
      collect_shell_use_casts
      ;;
    visual)
      copy_dir_if_present "$script_dir/vhs/actual" "$artifact_root/visual/actual"
      ;;
  esac
}

run_suite() {
  local suite="$1"
  shift

  printf '\n=== %s suite ===\n' "$suite"
  if "$@"; then
    printf 'PASS %s suite\n' "$suite"
  else
    local rc=$?
    printf 'FAIL %s suite (exit %d)\n' "$suite" "$rc" >&2
    collect_artifacts "$suite"
    status=1
  fi
}

case "$only" in
  all | behavioral)
    run_suite behavioral env SPUR_E2E_ARTIFACTS_DIR="$artifact_root/behavioral" "$script_dir/shell-use/run.sh"
    ;;
esac

case "$only" in
  all | visual)
    run_suite visual "$script_dir/vhs/run-vhs-suite.sh"
    ;;
esac

if [[ "$status" -ne 0 ]]; then
  printf '\nArtifacts: %s\n' "$artifact_root" >&2
  exit "$status"
fi
