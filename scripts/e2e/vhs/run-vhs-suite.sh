#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
VHS_DIR="${ROOT_DIR}/scripts/e2e/vhs"
# shellcheck disable=SC1091
source "${ROOT_DIR}/scripts/e2e/lib/spur-bin.sh"

cd "$VHS_DIR"

./check-vhs.sh

mkdir -p actual goldens
mkdir -p actual/raw

if [[ "${SPUR_VHS_STANDIN:-0}" == "1" ]]; then
  export SPUR_BIN="${VHS_DIR}/bin/standin-spur"
fi

if ! SPUR_BIN="$(spur_e2e_resolve_spur_bin)"; then
  cat >&2 <<'EOF'

For the VHS framework-only stand-in probe:
  SPUR_VHS_STANDIN=1 scripts/e2e/vhs/run-vhs-suite.sh
EOF
  exit 1
fi
export SPUR_BIN

tapes=(
  "cold-launch"
  "help-overlay"
  "clean-quit"
  "explore-browser-open"
)

status=0

extract_screen() {
  local raw="$1"
  local pattern="$2"
  local label="$3"

  awk -v pat="$pattern" -v label="$label" '
    BEGIN {
      sep = "────────────────────────────────────────────────────────────────────────────────"
      n = 0
      text = ""
      selected = ""
    }
    function reset_segment() {
      delete lines
      n = 0
      text = ""
    }
    function capture_segment(    last, i, segment) {
      segment = "## " label "\n"
      last = n
      while (last > 0 && (lines[last] == "" || lines[last] ~ /^> ?$/)) {
        last--
      }
      for (i = 1; i <= last; i++) {
        segment = segment lines[i] "\n"
      }
      selected = segment "\n"
    }
    function maybe_capture() {
      if (text ~ pat) {
        capture_segment()
        found = 1
      }
    }
    $0 == sep {
      maybe_capture()
      reset_segment()
      next
    }
    {
      lines[++n] = $0
      text = text $0 "\n"
    }
    END {
      maybe_capture()
      if (!found) {
        exit 1
      }
      printf "%s", selected
    }
  ' "$raw"
}

normalize_help_overlay() {
  local raw="$1"

  extract_screen "$raw" "Dashboard — Modes" "help overlay" | awk '
    /│  j\/k, Up\/Down[[:space:]]+Scroll or navigate/ {
      print "       │  j/k, Up/Down       Scroll or navigate"
      exit 0
    }
    { print }
  '
}

normalize_output() {
  local name="$1"
  local raw="$2"
  local out="$3"
  local tmp="${out}.tmp"

  rm -f "$tmp"
  case "$name" in
    cold-launch)
      extract_screen "$raw" "No agents configured" "cold launch" >>"$tmp"
      ;;
    help-overlay)
      normalize_help_overlay "$raw" >>"$tmp"
      ;;
    clean-quit)
      extract_screen "$raw" "Quit spur[?]" "quit confirmation" >>"$tmp"
      extract_screen "$raw" "VHS_SPUR_EXITED status=0" "shell after exit" >>"$tmp"
      ;;
    explore-browser-open)
      extract_screen "$raw" "Esc back" "explore browser open" >>"$tmp"
      ;;
    *)
      echo "error: no normalizer configured for ${name}" >&2
      return 1
      ;;
  esac
  awk '{ lines[NR] = $0; if ($0 != "") last = NR } END { for (i = 1; i <= last; i++) print lines[i] }' "$tmp" >"$out"
  rm -f "$tmp"
}

for name in "${tapes[@]}"; do
  tape="tapes/${name}.tape"
  actual="actual/${name}.txt"
  raw="actual/raw/${name}.txt"
  golden="goldens/${name}.txt"

  rm -f "$actual" "$raw"
  vhs validate "$tape" >/dev/null

  started=$SECONDS
  if vhs -q "$tape"; then
    runtime=$((SECONDS - started))
  else
    rc=$?
    runtime=$((SECONDS - started))
    echo "FAIL ${name} runtime=${runtime}s vhs_exit=${rc}"
    status=1
    continue
  fi

  if [[ ! -f "$raw" ]]; then
    echo "FAIL ${name} runtime=${runtime}s missing_output=${raw}"
    status=1
    continue
  fi

  if ! normalize_output "$name" "$raw" "$actual"; then
    echo "FAIL ${name} runtime=${runtime}s normalize_output=${raw}"
    status=1
    continue
  fi

  if [[ "${SPUR_VHS_UPDATE:-0}" == "1" ]]; then
    cp "$actual" "$golden"
    echo "UPDATE ${name} runtime=${runtime}s golden=${golden}"
    continue
  fi

  if diff -u "$golden" "$actual"; then
    echo "PASS ${name} runtime=${runtime}s golden=stable"
  else
    echo "FAIL ${name} runtime=${runtime}s golden=mismatch"
    status=1
  fi
done

exit "$status"
