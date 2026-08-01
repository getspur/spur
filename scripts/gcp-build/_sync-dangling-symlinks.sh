#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
usage:
  _sync-dangling-symlinks.sh partition SOURCE_ROOT FULL_MANIFEST REGULAR_MANIFEST SYMLINK_MANIFEST
  _sync-dangling-symlinks.sh restore REMOTE_DIR SYMLINK_MANIFEST
EOF
    exit 2
}

partition_manifest() {
    local source_root="${1:?source root required}"
    local full_manifest="${2:?full manifest required}"
    local regular_manifest="${3:?regular manifest required}"
    local symlink_manifest="${4:?symlink manifest required}"
    local manifest_entry link_target

    : >"$regular_manifest"
    : >"$symlink_manifest"
    while IFS= read -r -d '' manifest_entry; do
        if [[ -L "$source_root/$manifest_entry" && ! -e "$source_root/$manifest_entry" ]]; then
            link_target=$(readlink "$source_root/$manifest_entry")
            printf '%s\0%s\0' "$manifest_entry" "$link_target" >>"$symlink_manifest"
        else
            printf '%s\0' "$manifest_entry" >>"$regular_manifest"
        fi
    done <"$full_manifest"
}

restore_symlinks() {
    local remote_dir="${1:?remote dir required}"
    local symlink_manifest="${2:?symlink manifest required}"
    local worktree_root="$HOME/$remote_dir"
    local link_path link_target destination parent_dir current_target

    while IFS= read -r -d '' link_path \
        && IFS= read -r -d '' link_target; do
        case "$link_path" in
            /* | *..* | target | target/*)
                printf '[sync] unsafe symlink path: %s\n' "$link_path" >&2
                return 2
                ;;
        esac

        destination="$worktree_root/$link_path"
        if [[ -L "$destination" ]]; then
            current_target=$(readlink "$destination")
            if [[ "$current_target" == "$link_target" ]]; then
                continue
            fi
            rm -f -- "$destination"
        elif [[ -d "$destination" ]]; then
            if ! rmdir -- "$destination" 2>/dev/null; then
                printf '[sync] refusing to replace non-empty directory: %s\n' "$link_path" >&2
                return 2
            fi
        elif [[ -e "$destination" ]]; then
            rm -f -- "$destination"
        fi

        parent_dir=$(dirname -- "$destination")
        mkdir -p -- "$parent_dir"
        ln -s -- "$link_target" "$destination"
        printf '%s\n' "$link_path"
    done <"$symlink_manifest"
}

[[ $# -ge 1 ]] || usage
command_name="$1"
shift
case "$command_name" in
    partition)
        [[ $# -eq 4 ]] || usage
        partition_manifest "$@"
        ;;
    restore)
        [[ $# -eq 2 ]] || usage
        restore_symlinks "$@"
        ;;
    *)
        usage
        ;;
esac
