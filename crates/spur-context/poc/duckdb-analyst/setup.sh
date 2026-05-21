#!/usr/bin/env bash
# Builds the SPUR code-graph analyst DuckDB at .spur/analyst.duckdb.
# Idempotent: drops + rebuilds tables on each run. Safe to wire into pre-commit
# hooks or a make target. Run from the repo root.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

ARTIFACT="${SPUR_GRAPH_ARTIFACT:-$REPO_ROOT/.spur/graph-index.json}"
DB_PATH="${SPUR_ANALYST_DB:-$REPO_ROOT/.spur/analyst.duckdb}"
INIT_SQL="$REPO_ROOT/crates/spur-context/poc/duckdb-analyst/init.sql"

if [ ! -r "$ARTIFACT" ]; then
  echo "error: spur-graph artifact not found at $ARTIFACT" >&2
  echo "       set SPUR_GRAPH_ARTIFACT to an existing graph-index.json" >&2
  exit 1
fi

if ! command -v duckdb >/dev/null 2>&1; then
  echo "error: 'duckdb' CLI not in PATH (try: brew install duckdb)" >&2
  exit 1
fi

mkdir -p "$(dirname "$DB_PATH")"
# Start fresh — guarantees DROP semantics for tables/views.
rm -f "$DB_PATH"

duckdb "$DB_PATH" <<SQL
SET variable artifact_path = '$ARTIFACT';
.read $INIT_SQL
SELECT 'analyst db built:' AS step, * FROM _meta;
SQL

echo
echo "DB ready at: $DB_PATH"
echo "Inspect interactively: duckdb $DB_PATH"
echo "Run worked examples:   duckdb $DB_PATH < $REPO_ROOT/crates/spur-context/poc/duckdb-analyst/examples.sql"
