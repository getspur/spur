#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
sync_helper="$script_dir/_sync-manifest.sh"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

[[ -f "$sync_helper" ]] || fail "sync manifest helper is missing"

# shellcheck disable=SC1090
source "$sync_helper"
declare -F spur_rsync_manifest >/dev/null \
    || fail "spur_rsync_manifest is missing"

scratch_root=$(mktemp -d)
trap 'rm -rf "$scratch_root"' EXIT

source_root="$scratch_root/source"
destination_root="$scratch_root/destination"
manifest="$scratch_root/manifest"
transfer_log="$scratch_root/transferred"
link_path=".claude/skills/marketing-ab-testing"
link_target="../../marketing/marketingskills/skills/ab-testing"

mkdir -p "$source_root/.claude/skills" "$destination_root"
printf 'quality gate fixture\n' >"$source_root/regular.txt"
ln -s "$link_target" "$source_root/$link_path"
printf 'regular.txt\0%s\0' "$link_path" >"$manifest"

spur_rsync_manifest \
    "$source_root" \
    "$manifest" \
    "$destination_root" \
    "$transfer_log"

[[ "$(cat "$destination_root/regular.txt")" == "quality gate fixture" ]] \
    || fail "regular manifest entry was not copied"
[[ -L "$destination_root/$link_path" ]] \
    || fail "dangling symlink was not preserved"
[[ "$(readlink "$destination_root/$link_path")" == "$link_target" ]] \
    || fail "dangling symlink target changed"
grep -Fqx 'regular.txt' "$transfer_log" \
    || fail "regular transfer was not reported"
grep -Fqx "$link_path" "$transfer_log" \
    || fail "symlink transfer was not reported"

printf 'ok: sync manifest preserves tracked dangling symlinks\n'
