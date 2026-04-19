#!/usr/bin/env bash
# INV-C2: only the TUI translation task may construct InteractiveInput::Message.
set -euo pipefail

ALLOWED_FILES=(
  'crates/spur-tui/src/components/input_bar.rs'
  'crates/spur-cli/src/main.rs'                     # TUI→core translation task
  'crates/spur-core/src/orchestrator.rs'            # test modules only
  'crates/spur-core/tests/'
  'crates/spur-core/src/continuation_bridge.rs'     # test modules only
  'crates/spur-core/src/scheduler.rs'               # test modules only
)

HITS=$(git grep -nE 'InteractiveInput::Message' -- 'crates/**/*.rs' || true)
if [[ -z "$HITS" ]]; then
  echo "INV-C2: no construction sites found (suspicious)"; exit 0
fi

VIOLATIONS=""
while IFS= read -r line; do
  FILE=$(echo "$line" | cut -d: -f1)
  OK=0
  for allowed in "${ALLOWED_FILES[@]}"; do
    if [[ "$FILE" == "$allowed"* ]]; then OK=1; break; fi
  done
  if [[ $OK -eq 0 ]]; then
    VIOLATIONS+="$line"$'\n'
  fi
done <<<"$HITS"

if [[ -n "$VIOLATIONS" ]]; then
  echo "INV-C2 violation: InteractiveInput::Message constructed outside allowed sites:"
  echo "$VIOLATIONS"
  exit 1
fi

echo "INV-C2: OK"
