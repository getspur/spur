#!/bin/sh
# Compare sanitized production plan JSON and inventory snapshots captured before
# and after the POC. This script performs no AWS or Terraform operations.
set -eu

if [ "$#" -ne 4 ]; then
  echo "usage: $0 BEFORE_PLAN_JSON AFTER_PLAN_JSON BEFORE_INVENTORY_JSON AFTER_INVENTORY_JSON" >&2
  exit 2
fi

command -v jq >/dev/null 2>&1 || {
  echo "jq is required" >&2
  exit 2
}

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

compare_pair() {
  label=$1
  before=$2
  after=$3
  jq -S . "$before" >"$tmp_dir/${label}-before.json"
  jq -S . "$after" >"$tmp_dir/${label}-after.json"
  if ! cmp -s "$tmp_dir/${label}-before.json" "$tmp_dir/${label}-after.json"; then
    echo "${label} changed during the POC" >&2
    diff -u "$tmp_dir/${label}-before.json" "$tmp_dir/${label}-after.json" >&2 || true
    return 1
  fi
  echo "${label}: unchanged"
}

compare_pair production-plan "$1" "$2"
compare_pair production-inventory "$3" "$4"
