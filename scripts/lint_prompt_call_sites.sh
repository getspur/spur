#!/usr/bin/env bash
# INV-C1: only `run_interactive` in orchestrator.rs (or its notification_drain.rs
# helper, which is pub(crate) and only called from orchestrator) may call `.prompt(`
# on AgentConnection.
set -euo pipefail

# Check for violations outside the two allowed files.
OFFENDERS=$(
  git grep -nE '\bconnection\.prompt\(' -- 'crates/spur-core/src/' \
  | grep -v 'crates/spur-core/src/orchestrator.rs' \
  | grep -v 'crates/spur-core/src/notification_drain.rs' \
  || true
)

# Inside orchestrator.rs, verify each hit is inside run_interactive (best-effort).
ORCH_OFFENDERS=$(
  git grep -nE '\bconnection\.prompt\(' -- 'crates/spur-core/src/orchestrator.rs' \
  || true
)

if [[ -n "$OFFENDERS" ]]; then
  echo "INV-C1 violation: .prompt() called outside orchestrator.rs / notification_drain.rs"
  echo "$OFFENDERS"
  exit 1
fi

if [[ -n "$ORCH_OFFENDERS" ]]; then
  # Soft-verify each hit is inside run_interactive.
  while IFS= read -r line; do
    FILE=$(echo "$line" | cut -d: -f1)
    LINENO=$(echo "$line" | cut -d: -f2)
    FN=$(awk -v ln="$LINENO" 'NR<=ln && /pub async fn |pub fn |async fn |fn / {last=$0} END{print last}' "$FILE")
    if ! echo "$FN" | grep -q "run_interactive"; then
      echo "INV-C1 violation at $FILE:$LINENO — .prompt() called outside run_interactive"
      echo "  enclosing fn candidate: $FN"
      exit 1
    fi
  done <<<"$ORCH_OFFENDERS"
fi

echo "INV-C1: OK"
