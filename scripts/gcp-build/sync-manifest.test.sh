#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
sync_helper="$script_dir/_sync-dangling-symlinks.sh"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

[[ -x "$sync_helper" ]] || fail "dangling symlink sync helper is missing"

scratch_root=$(mktemp -d)
trap 'rm -rf "$scratch_root"' EXIT

source_root="$scratch_root/source"
remote_home="$scratch_root/home"
remote_dir="remote-worktree"
destination_root="$remote_home/$remote_dir"
manifest="$scratch_root/manifest"
regular_manifest="$scratch_root/regular-manifest"
symlink_manifest="$scratch_root/symlink-manifest"
transfer_log="$scratch_root/transferred"
link_path=".claude/skills/marketing-ab-testing"
link_target="../../marketing/marketingskills/skills/ab-testing"

mkdir -p "$source_root/.claude/skills" "$destination_root"
printf 'quality gate fixture\n' >"$source_root/regular.txt"
ln -s "$link_target" "$source_root/$link_path"
printf 'regular.txt\0%s\0' "$link_path" >"$manifest"

"$sync_helper" partition \
    "$source_root" \
    "$manifest" \
    "$regular_manifest" \
    "$symlink_manifest"

rsync -azcO --delete -0 --files-from="$regular_manifest" --out-format='%n' \
    "$source_root/" "$destination_root/" >"$transfer_log"
HOME="$remote_home" "$sync_helper" restore \
    "$remote_dir" \
    "$symlink_manifest" >>"$transfer_log"

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
