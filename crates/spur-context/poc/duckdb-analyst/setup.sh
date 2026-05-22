#!/usr/bin/env bash
# Builds the SPUR code-graph analyst DuckDB at .spur/analyst.duckdb.
# Idempotent: drops + rebuilds the DB on each run. Safe to wire into pre-commit
# hooks or a make target. Run from the repo root.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

CURRENT_LINK="${SPUR_GRAPH_CURRENT:-$REPO_ROOT/.spur/graph/CURRENT}"
DB_PATH="${SPUR_ANALYST_DB:-$REPO_ROOT/.spur/analyst.duckdb}"
INIT_SQL="$REPO_ROOT/crates/spur-context/poc/duckdb-analyst/init.sql"

resolve_path() {
  perl -MCwd=abs_path -e '
    my $path = abs_path($ARGV[0]);
    die "failed to resolve $ARGV[0]\n" unless defined $path;
    print "$path\n";
  ' "$1"
}

if [ -n "${SPUR_GRAPH_ARTIFACT_DIR:-}" ]; then
  ARTIFACT_DIR="$(resolve_path "$SPUR_GRAPH_ARTIFACT_DIR")"
elif [ -e "$CURRENT_LINK" ]; then
  ARTIFACT_DIR="$(resolve_path "$CURRENT_LINK")"
else
  echo "error: spur-graph CURRENT pointer not found at $CURRENT_LINK" >&2
  echo "       run: scripts/spur-cargo run -p spur-cli -- graph build --workspace" >&2
  echo "       or set SPUR_GRAPH_ARTIFACT_DIR to a Parquet artifact directory" >&2
  exit 1
fi

if [ ! -d "$ARTIFACT_DIR" ]; then
  echo "error: spur-graph artifact is not a directory: $ARTIFACT_DIR" >&2
  exit 1
fi

for required in \
  nodes.parquet \
  edges.parquet \
  edges_by_dst.parquet \
  edges_unresolved.parquet \
  files.parquet \
  file_manifests.parquet \
  tombstones.parquet \
  manifest.json
do
  if [ ! -r "$ARTIFACT_DIR/$required" ]; then
    echo "error: missing required graph artifact file: $ARTIFACT_DIR/$required" >&2
    exit 1
  fi
done

if ! command -v duckdb >/dev/null 2>&1; then
  echo "error: 'duckdb' CLI not in PATH (try: brew install duckdb)" >&2
  exit 1
fi

mkdir -p "$(dirname "$DB_PATH")"
# Start fresh — guarantees DROP semantics for tables/views.
rm -f "$DB_PATH"

ARTIFACT_DIR_SQL="${ARTIFACT_DIR//\'/\'\'}"

{
  SPUR_GRAPH_ARTIFACT_DIR_SQL="$ARTIFACT_DIR_SQL" perl -0pe \
    's#__SPUR_GRAPH_ARTIFACT_DIR__#$ENV{SPUR_GRAPH_ARTIFACT_DIR_SQL}#g' \
    "$INIT_SQL"
  printf "\nSELECT 'analyst db built:' AS step, * FROM _meta;\n"
} | duckdb "$DB_PATH"

echo
echo "DB ready at: $DB_PATH"
echo "Artifact dir: $ARTIFACT_DIR"
echo "Inspect interactively: duckdb $DB_PATH"
echo "Run worked examples:   duckdb $DB_PATH < $REPO_ROOT/crates/spur-context/poc/duckdb-analyst/examples.sql"
