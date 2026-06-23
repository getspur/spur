#!/usr/bin/env bash
#
# Index a crates.io package into the spur-context DuckLake catalog on S3.
#
# Usage:   scripts/index-package.sh <package> <version>
# Example: scripts/index-package.sh tokio 1.38.0
#
# Requires: spur (graph build), duckdb CLI, aws CLI, curl, tar
#
set -euo pipefail

PACKAGE="${1:?Usage: $0 <package> <version>}"
VERSION="${2:?}"
SOURCE="registry:crates-io"
BUCKET="${SPUR_CONTEXT_BUCKET:-spur-context}"
REGION="${AWS_REGION:-ap-southeast-5}"
SPUR_BIN="${SPUR_BIN:-spur}"
WORKDIR="/tmp/index-packages/$PACKAGE"
CATALOG="/tmp/spur-catalog.ducklake"

IFS='.' read -r MAJOR MINOR PATCH <<< "$VERSION"

# ── 1. Download ──────────────────────────────────────────────────────────
echo "[1/5] Download $PACKAGE@$VERSION ..."
rm -rf "$WORKDIR" && mkdir -p "$WORKDIR"
curl -sL "https://crates.io/api/v1/crates/$PACKAGE/$VERSION/download" \
  | tar xz -C "$WORKDIR" --strip-components=1

# ── 2. Build graph ───────────────────────────────────────────────────────
echo "[2/5] Build graph ..."
git -C "$WORKDIR" init -q && git -C "$WORKDIR" add -A && git -C "$WORKDIR" commit -q -m init
"$SPUR_BIN" graph build \
  --root "$WORKDIR" --output "$WORKDIR/.spur/graph" \
  --quiet --no-analyst --no-section-embeddings
ARTIFACT_DIR=$(find "$WORKDIR/.spur/graph/artifacts" -name "nodes.parquet" -exec dirname {} \; | head -1)
if [ -z "$ARTIFACT_DIR" ]; then
  echo "ERROR: no graph artifacts found"; exit 1
fi
NODES=$(duckdb -c "SELECT count(*) FROM read_parquet('$ARTIFACT_DIR/nodes.parquet')" -noheader 2>/dev/null)
EDGES=$(duckdb -c "SELECT count(*) FROM read_parquet('$ARTIFACT_DIR/edges.parquet')" -noheader 2>/dev/null)
echo "      $NODES nodes, $EDGES edges"

# ── 3. Fetch existing catalog ────────────────────────────────────────────
echo "[3/5] Fetch catalog ..."
rm -f "$CATALOG"
if aws s3 cp "s3://$BUCKET/catalog/catalog.ducklake" "$CATALOG" --region "$REGION" 2>/dev/null; then
  echo "      merging into existing catalog"
else
  echo "      new catalog"
fi

# S3 credentials for DuckDB
eval "$(aws configure export-credentials --format env)"
S3_TOKEN="${AWS_SESSION_TOKEN:+SET s3_session_token='${AWS_SESSION_TOKEN}';}"

# ── 4. Translate to DuckLake ─────────────────────────────────────────────
echo "[4/5] Translate ..."

duckdb <<SQL
INSTALL ducklake; INSTALL httpfs;
LOAD ducklake; LOAD httpfs;
SET s3_region='$REGION';
SET s3_access_key_id='$AWS_ACCESS_KEY_ID';
SET s3_secret_access_key='$AWS_SECRET_ACCESS_KEY';
$S3_TOKEN
ATTACH '$CATALOG' AS dl (TYPE ducklake, DATA_PATH 's3://$BUCKET/data/');

-- Tables (no-op if already exist from a prior package)
CREATE TABLE IF NOT EXISTS dl.nodes (
    stable_symbol_id VARCHAR, package VARCHAR, source VARCHAR,
    revision VARCHAR, revision_kind VARCHAR,
    semver_major INTEGER, semver_minor INTEGER, semver_patch INTEGER,
    file_path VARCHAR, byte_range_start INTEGER, byte_range_end INTEGER,
    line_start INTEGER, line_end INTEGER,
    entity_name VARCHAR, qualified_name VARCHAR, symbol_kind VARCHAR,
    anchor_hash VARCHAR, enclosing_scope VARCHAR
);
CREATE TABLE IF NOT EXISTS dl.edges (
    source_stable_id VARCHAR, target_stable_id VARCHAR,
    target_package VARCHAR, target_label VARCHAR,
    package VARCHAR, source VARCHAR,
    revision VARCHAR, revision_kind VARCHAR,
    semver_major INTEGER, semver_minor INTEGER, semver_patch INTEGER,
    relation VARCHAR, edge_kind VARCHAR, confidence VARCHAR, confidence_score DOUBLE,
    bind_method VARCHAR, receiver_text VARCHAR, scope_text VARCHAR
);
CREATE TABLE IF NOT EXISTS dl.files (
    stable_file_id VARCHAR, file_path VARCHAR, source_text VARCHAR,
    package VARCHAR, source VARCHAR,
    revision VARCHAR, revision_kind VARCHAR,
    semver_major INTEGER, semver_minor INTEGER, semver_patch INTEGER
);
CREATE TABLE IF NOT EXISTS dl.package_catalog (
    source VARCHAR, package VARCHAR, revision VARCHAR, revision_kind VARCHAR,
    semver_major INTEGER, semver_minor INTEGER, semver_patch INTEGER,
    snapshot_id BIGINT, indexed_at TIMESTAMP, index_status VARCHAR,
    embeddings_status VARCHAR, row_counts JSON
);
CREATE TABLE IF NOT EXISTS dl.refs (
    source VARCHAR, package VARCHAR, ref_name VARCHAR,
    revision VARCHAR, updated_at TIMESTAMP
);
-- Set partitioning (errors on already-partitioned tables are harmless)
BEGIN TRANSACTION; ALTER TABLE dl.nodes SET PARTITIONED BY (source, package); COMMIT;
BEGIN TRANSACTION; ALTER TABLE dl.edges SET PARTITIONED BY (source, package); COMMIT;
BEGIN TRANSACTION; ALTER TABLE dl.files SET PARTITIONED BY (source, package); COMMIT;
BEGIN TRANSACTION; ALTER TABLE dl.package_catalog SET PARTITIONED BY (source, package); COMMIT;
BEGIN TRANSACTION; ALTER TABLE dl.refs SET PARTITIONED BY (source, package); COMMIT;

-- Remove old data for this package (idempotent re-index)
DELETE FROM dl.nodes  WHERE package='$PACKAGE' AND source='$SOURCE';
DELETE FROM dl.edges  WHERE package='$PACKAGE' AND source='$SOURCE';
DELETE FROM dl.files  WHERE package='$PACKAGE' AND source='$SOURCE';
DELETE FROM dl.package_catalog WHERE package='$PACKAGE' AND source='$SOURCE';
DELETE FROM dl.refs   WHERE package='$PACKAGE' AND source='$SOURCE';

-- Insert
INSERT INTO dl.nodes BY NAME
SELECT stable_symbol_id, '$PACKAGE' AS package, '$SOURCE' AS source,
       '$VERSION' AS revision, 'semver' AS revision_kind,
       $MAJOR AS semver_major, $MINOR AS semver_minor, $PATCH AS semver_patch,
       file_path, byte_range_start, byte_range_end,
       line_start, line_end, entity_name, qualified_name,
       symbol_kind, anchor_hash, enclosing_scope
FROM read_parquet('$ARTIFACT_DIR/nodes.parquet');

INSERT INTO dl.edges BY NAME
SELECT source_stable_id, target_stable_id,
       CAST(NULL AS VARCHAR) AS target_package, target_label,
       '$PACKAGE' AS package, '$SOURCE' AS source,
       '$VERSION' AS revision, 'semver' AS revision_kind,
       $MAJOR AS semver_major, $MINOR AS semver_minor, $PATCH AS semver_patch,
       relation, edge_kind, confidence, confidence_score,
       bind_method, receiver_text, scope_text
FROM read_parquet('$ARTIFACT_DIR/edges.parquet');

INSERT INTO dl.files BY NAME
SELECT stable_file_id, file_path, CAST(NULL AS VARCHAR) AS source_text,
       '$PACKAGE' AS package, '$SOURCE' AS source,
       '$VERSION' AS revision, 'semver' AS revision_kind,
       $MAJOR AS semver_major, $MINOR AS semver_minor, $PATCH AS semver_patch
FROM read_parquet('$ARTIFACT_DIR/files.parquet');

INSERT INTO dl.package_catalog BY NAME
SELECT '$SOURCE' AS source, '$PACKAGE' AS package, '$VERSION' AS revision,
       'semver' AS revision_kind,
       $MAJOR AS semver_major, $MINOR AS semver_minor, $PATCH AS semver_patch,
       $PATCH AS snapshot_id, NOW() AS indexed_at,
       'complete' AS index_status, 'none' AS embeddings_status,
       json_object('nodes', $NODES, 'edges', $EDGES) AS row_counts;

INSERT INTO dl.refs BY NAME
SELECT '$SOURCE' AS source, '$PACKAGE' AS package, 'latest' AS ref_name,
       '$VERSION' AS revision, NOW() AS updated_at;

DETACH dl;
SQL

# ── 5. Upload ────────────────────────────────────────────────────────────
echo "[5/5] Upload catalog ..."
aws s3 cp "$CATALOG" "s3://$BUCKET/catalog/catalog.ducklake" --region "$REGION"
rm -f "$CATALOG"

echo "Done: $PACKAGE@$VERSION ($NODES nodes, $EDGES edges)"
