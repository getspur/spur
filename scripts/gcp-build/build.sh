#!/usr/bin/env bash
# Sync the current worktree to the build VM and run cargo remotely.
#
# Multi-worktree aware: detects whether we're in the main repo or a
# .spur/worktrees/<UUID> worktree, syncs to a matching remote dir, and uses
# per-worktree CARGO_TARGET_DIR + SCCACHE_BASEDIRS so all worktrees share the
# GCS sccache cache through path normalization.
#
# Spot-preemption resilient: the build VM is a spot instance
# (--instance-termination-action=DELETE), so it can vanish mid-build. We detect
# that case after any remote step fails and recover once by re-spinning +
# re-syncing + re-running — see "orchestrate" near the bottom.
#
# Usage:
#   ./build.sh                              # cargo build --release --workspace
#   ./build.sh -- check                     # cargo check
#   ./build.sh --auto-spin -- test ...      # auto-create VM if missing
#   SPUR_BUILD_JOBS=22 ./build.sh -- build  # override default -j 8
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck disable=SC1091
# shellcheck source=scripts/gcp-build/config.env
source "$SCRIPT_DIR/config.env"

log() { echo "[build] $*" >&2; }

# Exit code reserved for "VM/infrastructure unavailable" — distinct from any
# cargo exit code (cargo uses 1/100/101). spur-cargo uses this to decide
# whether to fall back to local cargo. Anything else is propagated as-is.
INFRA_UNAVAILABLE=200

# Flags may appear in any order before the optional `--` cargo-args separator.
#   --auto-spin       create/start the VM if it is not already RUNNING
#   --frontend-test   skip cargo; run the notebook frontend (vitest) suite on
#                     the VM instead. vitest is a per-project devDependency, so
#                     this installs node_modules (when stale) then runs the
#                     `test` npm script. Override the script via
#                     SPUR_FRONTEND_TEST_CMD (default: `npm test` -> vitest run).
#   --pnpm            skip cargo; run pnpm in the notebook frontend on the VM
#                     with a shared pnpm store and per-worktree node_modules on
#                     /mnt/cargo so installs can hard-link from the store.
AUTO_SPIN=0
FRONTEND_TEST=0
PNPM=0
while [[ "${1:-}" == --* ]]; do
    case "$1" in
        --auto-spin)     AUTO_SPIN=1; shift ;;
        --frontend-test) FRONTEND_TEST=1; shift ;;
        --pnpm)          PNPM=1; shift ;;
        --)              shift; break ;;
        *)               break ;;
    esac
done
CARGO_ARGS="${CARGO_ARGS:-${*:-build --release --workspace}}"
PNPM_ARGS=("$@")
if [[ $PNPM -eq 1 && "${PNPM_ARGS[0]:-}" == "--" ]]; then
    PNPM_ARGS=("${PNPM_ARGS[@]:1}")
fi
PNPM_ARGS_ESCAPED=""
if [[ ${#PNPM_ARGS[@]} -gt 0 ]]; then
    PNPM_ARGS_ESCAPED="$(printf ' %q' "${PNPM_ARGS[@]}")"
fi
NOTEBOOK_FRONTEND_DIR="crates/spur-notebook/jute-notebook"
NOTEBOOK_FRONTEND_INSTALL_CMD="npm ci"
NOTEBOOK_FRONTEND_BUILD_CMD="npm run build"
NOTEBOOK_FRONTEND_TEST_CMD="${SPUR_FRONTEND_TEST_CMD:-npm test}"
NOTEBOOK_FRONTEND_PNPM_STORE="/mnt/cargo/pnpm-store"
PNPM_VERSION="${SPUR_PNPM_VERSION:-10.28.2}"
PNPM_VERSION_ESCAPED="$(printf '%q' "$PNPM_VERSION")"

is_notebook_production_build() {
    local args="$1"
    [[ "$args" == *spur-notebook* && "$args" == *custom-protocol* ]]
}

# ---- worktree detection ----------------------------------------------------
# Resolve toplevel from the *current* directory so workers invoking us from
# .spur/worktrees/<uuid>/ pick up that worktree's path.
GIT_TOPLEVEL=$(git rev-parse --show-toplevel 2>/dev/null || echo "")
if [[ -z "$GIT_TOPLEVEL" ]]; then
    log "Not inside a git repo. Aborting."
    exit 2
fi

if [[ "$GIT_TOPLEVEL" == *"/.spur/worktrees/"* ]]; then
    WORKTREE_UUID=$(basename "$GIT_TOPLEVEL")
    WORKTREE_KEY="worktrees/$WORKTREE_UUID"
else
    WORKTREE_KEY="main"
fi
WORKTREE_FILE_KEY="${WORKTREE_KEY//\//_}"
REMOTE_DIR="spur/$WORKTREE_KEY"                       # e.g. spur/worktrees/UUID
REMOTE_TARGET="/mnt/cargo/targets/$WORKTREE_KEY"
REMOTE_PNPM_NODE_MODULES="/mnt/cargo/pnpm-nm/$WORKTREE_KEY"
JOBS="${SPUR_BUILD_JOBS:-8}"
NOTEBOOK_FRONTEND_HAS_PNPM_LOCK=0
if [[ -f "$GIT_TOPLEVEL/$NOTEBOOK_FRONTEND_DIR/pnpm-lock.yaml" ]]; then
    NOTEBOOK_FRONTEND_HAS_PNPM_LOCK=1
fi

log "Worktree: $WORKTREE_KEY  (local=$GIT_TOPLEVEL)"
log "Remote:   ~/$REMOTE_DIR   target=$REMOTE_TARGET   -j$JOBS"

# ---- VM lifecycle ----------------------------------------------------------
# Distinguish three states:
#   RUNNING                       — proceed
#   TERMINATED / STOPPED / etc.   — `instances start` (cheap, ~30s)
#   MISSING                       — `spin.sh` (creates from scratch)

wait_for_ssh() {
    log "Waiting for SSH on $VM_NAME..."
    for _ in $(seq 1 30); do
        if gcloud compute ssh "$VM_NAME" \
                --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
                --tunnel-through-iap --quiet \
                --command='true' >/dev/null 2>&1; then
            return 0
        fi
        sleep 5
    done
    return 1
}

# Echo the VM's current GCE status, or MISSING if it does not exist / describe
# fails. A deleted spot instance (the preemption outcome with
# --instance-termination-action=DELETE) reports MISSING here.
vm_status() {
    gcloud compute instances describe "$VM_NAME" \
        --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
        --format='value(status)' 2>/dev/null || echo "MISSING"
}

# Bring the VM to RUNNING, honoring AUTO_SPIN. On any unrecoverable condition
# this exits INFRA_UNAVAILABLE so spur-cargo falls back to local cargo. A fresh
# spin.sh re-attaches the persistent cache disk, so target/, the cargo registry,
# the rustup toolchain, and the GCS sccache all survive a preemption — only the
# boot disk (and thus the synced source tree under ~/spur) is lost and re-synced.
ensure_vm_up() {
    local status
    status=$(vm_status)
    case "$status" in
        RUNNING)
            ;;
        TERMINATED|STOPPED|STOPPING|SUSPENDED|SUSPENDING)
            if [[ $AUTO_SPIN -eq 1 ]]; then
                log "VM $VM_NAME is $status — starting..."
                gcloud compute instances start "$VM_NAME" \
                    --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
                    --quiet || { log "start failed"; exit $INFRA_UNAVAILABLE; }
                wait_for_ssh || { log "SSH never came up"; exit $INFRA_UNAVAILABLE; }
            else
                log "VM $VM_NAME is $status. Pass --auto-spin to start it."
                exit $INFRA_UNAVAILABLE
            fi
            ;;
        MISSING)
            if [[ $AUTO_SPIN -eq 1 ]]; then
                log "VM $VM_NAME missing — spinning a fresh one..."
                "$SCRIPT_DIR/spin.sh" || { log "spin.sh failed"; exit $INFRA_UNAVAILABLE; }
            else
                log "VM $VM_NAME missing. Pass --auto-spin to create it."
                exit $INFRA_UNAVAILABLE
            fi
            ;;
        *)
            log "VM $VM_NAME in unexpected state: $status"
            exit $INFRA_UNAVAILABLE
            ;;
    esac
}

# ---- choose SSH transport: direct (default) -> IAP -> local ----------------
# Direct SSH connects to the VM's external IP and skips the IAP tunnel, which
# roughly doubles upload throughput when the uplink has headroom (measured
# ~1 MB/s over IAP vs ~2.5 MB/s direct from Vietnam->asia-southeast1). It does
# depend on the firewall keeping tcp:22 reachable; IAP does not. We probe the
# preferred transport with a cheap `true` (bounded by ConnectTimeout so a
# filtered :22 fails fast) and fall back in order:
#   direct -> IAP -> exit INFRA_UNAVAILABLE (caller spur-cargo builds locally)
#
# Both modes run through `gcloud compute ssh`, so OS Login, key management, and
# host-key validation are identical — only the transport path differs (omit vs
# pass --tunnel-through-iap). Set SPUR_DIRECT_SSH=0 to force IAP-only.
probe_transport() {
    # $1: transport flag ("" for direct, "--tunnel-through-iap" for IAP).
    local connect_timeout="${SPUR_SSH_CONNECT_TIMEOUT:-3}"
    if [[ -n "$1" ]]; then
        gcloud compute ssh "$VM_NAME" \
            --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
            "$1" --quiet --command='true' \
            -- -o ConnectTimeout="$connect_timeout" >/dev/null 2>&1
    else
        gcloud compute ssh "$VM_NAME" \
            --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
            --quiet --command='true' \
            -- -o ConnectTimeout="$connect_timeout" >/dev/null 2>&1
    fi
}

# Pick a working SSH transport and publish it via the IAP_FLAG / TRANSPORT
# globals (re-probed on every dispatch attempt — a fresh post-preemption VM has
# a new external IP). Exits INFRA_UNAVAILABLE when neither transport is
# reachable. Sets the env that the rsync transport (_gcloud-ssh.sh) reads.
choose_transport() {
    local direct="${SPUR_DIRECT_SSH:-1}"
    if [[ "$direct" != "0" ]] && probe_transport ""; then
        IAP_FLAG=""; TRANSPORT_MODE="direct (external IP)"
    elif probe_transport "--tunnel-through-iap"; then
        IAP_FLAG="--tunnel-through-iap"; TRANSPORT_MODE="IAP tunnel"
    else
        log "No SSH transport reachable (direct + IAP both failed) — falling back to local build."
        exit $INFRA_UNAVAILABLE
    fi
    log "SSH transport: $TRANSPORT_MODE"

    TRANSPORT="$SCRIPT_DIR/_gcloud-ssh.sh"
    export GCP_PROJECT GCP_ZONE
    # _gcloud-ssh.sh (the rsync transport) reads this to match the chosen mode.
    export SPUR_SSH_IAP_FLAG="$IAP_FLAG"
}

# Every remote command goes through here so it uses the chosen transport.
# $IAP_FLAG is intentionally unquoted: empty -> no arg (direct), else one flag.
remote_ssh() {
    gcloud compute ssh "$VM_NAME" \
        --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
        $IAP_FLAG --quiet "$@"
}

# ---- sync the worktree to the VM -------------------------------------------
# Returns nonzero (without aborting the script) if any sync step fails, so the
# orchestrator can tell a spot preemption mid-sync from a genuine error by
# re-checking VM state. FILE_LIST / XFER_LIST are created once by the caller and
# reused across attempts.
sync_workspace() {
    log "Enumerating git-tracked files..."
    cd "$GIT_TOPLEVEL"
    git ls-files -z --cached --others --exclude-standard >"$FILE_LIST" || return $?
    local count
    count=$(tr -cd '\0' <"$FILE_LIST" | wc -c | tr -d ' ')
    log "  $count files"

    # Ensure remote parent dir exists (idempotent).
    # Note we symlink the in-source target/ -> $REMOTE_TARGET instead of setting
    # CARGO_TARGET_DIR env — sccache hashes CARGO_TARGET_DIR (it's a CARGO_* var)
    # which would defeat cross-worktree cache sharing.
    #
    # Only (re)create the symlink when it is missing or points elsewhere. An
    # unconditional `rm -rf target && ln -sf` momentarily deletes the link; a
    # concurrent build sharing this worktree key (e.g. another session running
    # cargo against ~/spur/main) then races into the gap and rustc fails with
    # "os error 2" creating a temp dir under target/release/deps. When the link is
    # already correct — the steady state for repeated builds — we touch nothing and
    # the window never opens.
    remote_ssh \
        --command="mkdir -p ~/$REMOTE_DIR $REMOTE_TARGET /mnt/cargo/cargo-home /mnt/cargo/rustup && link=\"\$HOME/$REMOTE_DIR/target\" && if [ \"\$(readlink \"\$link\" 2>/dev/null)\" != \"$REMOTE_TARGET\" ]; then rm -rf \"\$link\"; ln -s \"$REMOTE_TARGET\" \"\$link\"; fi" >/dev/null || return $?

    local remote_xfer_list="/tmp/spur-sync-xfer.$WORKTREE_FILE_KEY"
    SYNC_TRANSFER_COUNT=0
    log "Syncing to $VM_NAME:~/$REMOTE_DIR ..."
    # Capture the exact set of files rsync transfers. --checksum is intentional:
    # the VM-side restamp below makes changed sources newer than any cached cargo
    # artifact, but it also means remote mtimes no longer match local mtimes. If
    # rsync used its default size+mtime quick-check, every warm run would treat
    # restamped files as changed, re-transfer thousands of paths, restamp them
    # again, and force cargo to rebuild workspace crates. Checksum mode makes the
    # delta content-based, and --omit-dir-times keeps directory metadata churn out
    # of $XFER_LIST so only content changes are restamped.
    rsync -azcO --delete -0 --files-from="$FILE_LIST" --out-format='%n' \
        -e "$TRANSPORT" \
        "$GIT_TOPLEVEL/" "$VM_NAME:$REMOTE_DIR/" >"$XFER_LIST" || return $?

    # ---- normalize mtimes of just-synced files to the VM clock -------------
    # Cargo decides whether to rebuild a path dependency (e.g. jute) by comparing
    # source-file mtimes against the cached artifact's mtime — not by content hash.
    # rsync -a (-> -t) preserves the dev machine's mtimes on the VM copy. When the
    # dev clock lags the VM clock even slightly, a freshly edited file lands with an
    # mtime OLDER than an artifact the VM built moments earlier, so cargo declares
    # the crate fresh and silently skips the edit (observed: a newly added method
    # never compiled in despite a clean source sync). Touching exactly the
    # transferred files to the VM's "now" guarantees changed sources are newer than
    # any prior artifact (correct rebuild) while leaving unchanged files untouched
    # (incremental cache intact). touch -c skips anything rsync reported but that no
    # longer exists (e.g. directory entries that were pruned).
    # See: docs/rca/2026-05-31-remote-cargo-stale-fingerprint.md
    if [[ -s "$XFER_LIST" ]]; then
        SYNC_TRANSFER_COUNT=$(wc -l <"$XFER_LIST" | tr -d ' ')
        log "Re-stamping $SYNC_TRANSFER_COUNT synced path(s) to VM clock..."
        rsync -az -e "$TRANSPORT" "$XFER_LIST" "$VM_NAME:$remote_xfer_list" || return $?
        remote_ssh \
            --command="cd \"\$HOME/$REMOTE_DIR\" && while IFS= read -r f; do [ -n \"\$f\" ] && touch -c -- \"\$f\"; done < \"$remote_xfer_list\"" || return $?
    fi

    # ---- reconcile: prune files deleted locally since the last sync --------
    # rsync's --delete is INERT when combined with --files-from (it transfers only
    # the listed files and never recurses the destination, so it cannot prune
    # extraneous files). A file removed/renamed locally therefore lingers on the VM
    # and silently desyncs the remote build from local (stale modules, removed
    # tests). We ship the current manifest and let a VM-side helper delete exactly
    # (previous manifest - current manifest): only ever rsync-managed source that is
    # now gone, never VM-generated artifacts (node_modules/, dist/, target/) which
    # were never in any manifest. The baseline manifest persists on the cache disk
    # (/mnt/cargo) keyed by worktree, so it survives across builds and VM restarts.
    local remote_manifest_cur="/tmp/spur-sync-manifest.$WORKTREE_FILE_KEY"
    local stored_manifest="/mnt/cargo/sync-manifests/$WORKTREE_KEY.manifest"
    log "Reconciling remote workspace (pruning locally-deleted files)..."
    rsync -az -e "$TRANSPORT" "$FILE_LIST" "$VM_NAME:$remote_manifest_cur" || return $?
    remote_ssh \
        --command="bash \"\$HOME/$REMOTE_DIR/scripts/gcp-build/_prune-remote.sh\" \"$REMOTE_DIR\" \"$remote_manifest_cur\" \"$stored_manifest\"" || return $?
}

# ---- run the requested payload on the VM -----------------------------------
# Returns the remote command's exit code (without aborting the script) so the
# orchestrator can distinguish a genuine build/test failure from a preemption.
run_payload() {
    # ---- run pnpm in the notebook frontend on the VM ----------------------
    # pnpm hard-links node_modules entries from its content-addressable store.
    # Hard-links only work within one filesystem, so both the shared store and this
    # worktree's node_modules live under /mnt/cargo. The in-source node_modules
    # path is only a symlink, mirroring the target/ symlink used for cargo.
    if [[ $PNPM -eq 1 ]]; then
        log "Running pnpm on VM: $NOTEBOOK_FRONTEND_DIR (pnpm$PNPM_ARGS_ESCAPED)"
        remote_ssh \
            --command="bash -lc '
                set -e
                cd ~/$REMOTE_DIR
                source /etc/profile.d/spur-build.sh 2>/dev/null || true
                frontend_dir=\"\$HOME/$REMOTE_DIR/$NOTEBOOK_FRONTEND_DIR\"
                pnpm_store=\"$NOTEBOOK_FRONTEND_PNPM_STORE\"
                pnpm_node_modules=\"$REMOTE_PNPM_NODE_MODULES\"
                mkdir -p \"\$pnpm_store\" \"\$pnpm_node_modules\"
                link=\"\$frontend_dir/node_modules\"
                cd \"\$frontend_dir\"
                store_dev=\$(stat -Lc %d \"\$pnpm_store\")
                nm_dev=\$(stat -Lc %d \"\$pnpm_node_modules\")
                if [ \"\$store_dev\" != \"\$nm_dev\" ]; then
                    echo \"[build] pnpm store and node_modules are on different filesystems\" >&2
                    exit 1
                fi
                pnpm_version=$PNPM_VERSION_ESCAPED
                ensure_node_modules_link() {
                    if [ \"\$(readlink \"\$link\" 2>/dev/null)\" != \"\$pnpm_node_modules\" ]; then
                        rm -rf \"\$link\"
                        ln -s \"\$pnpm_node_modules\" \"\$link\"
                    fi
                }
                corepack prepare pnpm@\"\$pnpm_version\" --activate
                lockfile=\"\"
                install_flags=(--prefer-offline)
                version_marker=\"\$pnpm_node_modules/.spur-pnpm-version\"
                if [[ $NOTEBOOK_FRONTEND_HAS_PNPM_LOCK -eq 0 ]]; then
                    rm -f pnpm-lock.yaml
                fi
                if [[ $NOTEBOOK_FRONTEND_HAS_PNPM_LOCK -eq 1 && -f pnpm-lock.yaml ]]; then
                    lockfile=\"pnpm-lock.yaml\"
                    install_flags+=(--frozen-lockfile)
                elif [ -f package-lock.json ]; then
                    lockfile=\"package-lock.json\"
                fi
                lock_hash=\"\"
                if [ -n \"\$lockfile\" ]; then
                    lock_hash=\$(sha256sum \"\$lockfile\" | awk \"{ print \\\$1 }\")
                fi
                expected_marker=\"\$pnpm_version \$lockfile \$lock_hash\"
                if [ ! -d \"\$pnpm_node_modules/.pnpm\" ] || [ ! -f \"\$version_marker\" ] || [ \"\$(cat \"\$version_marker\")\" != \"\$expected_marker\" ]; then
                    echo \"[build] Installing frontend deps: pnpm install \${install_flags[*]}\"
                    rm -rf \"\$link\"
                    rm -rf \"\$pnpm_node_modules\"
                    mkdir -p \"\$pnpm_node_modules\"
                    ensure_node_modules_link
                    pnpm --dir \"\$frontend_dir\" --store-dir \"\$pnpm_store\" install \"\${install_flags[@]}\"
                    if [[ $NOTEBOOK_FRONTEND_HAS_PNPM_LOCK -eq 0 ]]; then
                        rm -f pnpm-lock.yaml
                    fi
                    printf \"%s\n\" \"\$expected_marker\" >\"\$version_marker\"
                    touch \"\$pnpm_node_modules\"
                else
                    echo \"[build] node_modules current; skipping install\"
                fi
                ensure_node_modules_link
                echo \"[build] pnpm --dir $NOTEBOOK_FRONTEND_DIR$PNPM_ARGS_ESCAPED\"
                pnpm --dir \"\$frontend_dir\"$PNPM_ARGS_ESCAPED
            '" || return $?
        log "pnpm done."
        return 0
    fi

    # ---- run notebook frontend (vitest) tests on the VM --------------------
    # vitest is not a system tool — it lives in the worktree's node_modules, which
    # is gitignored and therefore never synced. We install it on the VM (npm ci),
    # reusing the just-synced sources, then run the project's test script. Install
    # is skipped when node_modules is already present and no newer than the lockfile
    # (node_modules persists on the VM across syncs, so repeat TDD runs are fast).
    if [[ $FRONTEND_TEST -eq 1 ]]; then
        log "Running notebook frontend tests on VM: $NOTEBOOK_FRONTEND_DIR ($NOTEBOOK_FRONTEND_TEST_CMD)"
        remote_ssh \
            --command="bash -lc '
                set -e
                cd ~/$REMOTE_DIR/$NOTEBOOK_FRONTEND_DIR
                source /etc/profile.d/spur-build.sh 2>/dev/null || true
                if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
                    echo \"[build] Installing frontend deps: $NOTEBOOK_FRONTEND_INSTALL_CMD\"
                    $NOTEBOOK_FRONTEND_INSTALL_CMD
                else
                    echo \"[build] node_modules current; skipping install\"
                fi
                echo \"[build] $NOTEBOOK_FRONTEND_TEST_CMD\"
                $NOTEBOOK_FRONTEND_TEST_CMD
            '" || return $?
        log "Frontend tests done."
        return 0
    fi

    # ---- run cargo on the VM ----------------------------------------------
    # Forward caller's $RUSTFLAGS so cfg/lint flags survive the SSH hop. The
    # remote bash re-parses `cargo $CARGO_ARGS` and would strip quotes from a
    # `--config build.rustflags=[...]` arg, so we propagate via env instead and
    # escape with %q to survive both the local→gcloud quoting and the remote
    # bash -lc re-parse.
    #
    # Only emit the export when the caller actually set RUSTFLAGS. A set-but-empty
    # RUSTFLAGS is NOT inert: cargo treats it as "no flags" and IGNORES the synced
    # .cargo/config.toml `build.rustflags`, silently dropping
    # `-C force-frame-pointers=yes`. That both diverges from local builds (where
    # config applies) and changes rustc args, fragmenting the shared sccache cache.
    # Verified: `RUSTFLAGS='' cargo build -v` omits force-frame-pointers; unset
    # preserves it. So when unset we emit no export at all and let config win.
    local remote_rustflags_export=""
    if [[ -n "${RUSTFLAGS:-}" ]]; then
        remote_rustflags_export="export RUSTFLAGS=$(printf '%q' "$RUSTFLAGS")"
    fi
    log "Running: cargo $CARGO_ARGS  -j$JOBS${RUSTFLAGS:+  RUSTFLAGS=$RUSTFLAGS}"
    local notebook_production_build=0
    if is_notebook_production_build "$CARGO_ARGS"; then
        notebook_production_build=1
        log "Notebook production build detected; will run frontend build on VM first."
    fi
    local capture_cargo_output=0
    if [[ "${SPUR_CAPTURE_FRESH_CARGO_OUTPUT:-1}" != "0" && "${SYNC_TRANSFER_COUNT:-0}" == "0" ]]; then
        capture_cargo_output=1
        log "No source content delta; capturing cargo output on VM (set SPUR_CAPTURE_FRESH_CARGO_OUTPUT=0 to stream)."
    fi
    remote_ssh \
        --command="bash -lc '
            set -e
            cd ~/$REMOTE_DIR
            source /etc/profile.d/spur-build.sh 2>/dev/null || true
            # Big-disk TMPDIR. Nested builds that use std::env::temp_dir() — e.g. the
            # rest-table-gateway-ext load/action harnesses, which point
            # CARGO_TARGET_DIR at a ~6G DuckDB build under a temp dir — would
            # otherwise land on the small ~49G root disk that holds /tmp and exhaust
            # it. /mnt/cargo is the ~295G build disk. TMPDIR is not a CARGO_* var, so
            # this does NOT affect sccache cross-worktree hashing.
            mkdir -p /mnt/cargo/tmp
            export TMPDIR=/mnt/cargo/tmp
            # Do NOT set CARGO_TARGET_DIR — sccache hashes CARGO_* env vars, which
            # would defeat cross-worktree sharing. We symlinked target/ to
            # /mnt/cargo/targets/\$WORKTREE_KEY at sync time instead. Likewise
            # SCCACHE_BASEDIRS is set per rustc invocation by the
            # sccache-worktree wrapper (RUSTC_WRAPPER in profile.d).
            export CARGO_BUILD_JOBS=$JOBS
            $remote_rustflags_export
            sccache --start-server >/dev/null 2>&1 || true
            if [[ $notebook_production_build -eq 1 ]]; then
                echo \"[build] Building notebook frontend: $NOTEBOOK_FRONTEND_DIR\"
                (cd $NOTEBOOK_FRONTEND_DIR && $NOTEBOOK_FRONTEND_INSTALL_CMD && $NOTEBOOK_FRONTEND_BUILD_CMD)
            fi
            cargo_log=/tmp/spur-cargo-output.$WORKTREE_FILE_KEY.log
            if [[ $capture_cargo_output -eq 1 ]]; then
                set +e
                cargo $CARGO_ARGS >\"\$cargo_log\" 2>&1
                cargo_rc=\$?
                set -e
                if [[ \$cargo_rc -ne 0 ]]; then
                    cat \"\$cargo_log\"
                    exit \$cargo_rc
                fi
                tail -30 \"\$cargo_log\"
            else
                cargo $CARGO_ARGS
            fi
            echo
            echo \"--- sccache stats ($WORKTREE_KEY) ---\"
            sccache --show-stats > /tmp/spur-sccache-stats.$WORKTREE_FILE_KEY
            sed -n '1,20p' /tmp/spur-sccache-stats.$WORKTREE_FILE_KEY
        '" || return $?

    log "Done. target lives at $VM_NAME:$REMOTE_TARGET — use scripts/gcp-build/fetch.sh to pull artifacts."
    return 0
}

# ---- orchestrate: ensure VM, sync, run — with one preemption retry ----------
# The build VM is a spot instance created with
# --instance-termination-action=DELETE, so a preemption mid-build deletes the VM
# and drops our SSH session. The failing remote step then exits nonzero
# (typically 255 for a dropped connection) — a code that is neither 0 nor
# INFRA_UNAVAILABLE, so without this handling spur-cargo would misread it as a
# genuine build/test failure and propagate it (and the CLAUDE.md contract tells
# agents NOT to re-run a "real" red remotely).
#
# So after any nonzero step we re-check the VM's state:
#   * vanished (not RUNNING) -> spot preemption. The persistent cache disk and
#     GCS sccache survived, so we re-spin (fresh boot disk -> source tree gone),
#     re-sync, and run once more. The retry is warm: cargo re-fingerprints
#     everything dirty but sccache serves the objects from GCS. A second vanish,
#     or a VM we can't bring up, yields INFRA_UNAVAILABLE so spur-cargo falls
#     back to local cargo.
#   * still RUNNING -> a genuine build/test failure; propagate the exit code
#     unchanged ("a red remote test is a real failure").
FILE_LIST=$(mktemp)
XFER_LIST=$(mktemp)
trap 'rm -f "$FILE_LIST" "$XFER_LIST"' EXIT

ensure_vm_up
choose_transport

attempt=1
MAX_ATTEMPTS=2
while : ; do
    rc=0
    sync_workspace || rc=$?
    if [[ $rc -eq 0 ]]; then
        run_payload || rc=$?
    fi
    if [[ $rc -eq 0 ]]; then
        exit 0
    fi

    # Nonzero. Disambiguate spot preemption from a genuine failure by VM state.
    status_after=$(vm_status)
    if [[ "$status_after" == "RUNNING" ]]; then
        # VM is alive — this is a real build/test failure. Propagate as-is.
        exit "$rc"
    fi

    if [[ $attempt -lt $MAX_ATTEMPTS ]]; then
        log "Remote step exited $rc and VM $VM_NAME is now $status_after — likely spot preemption mid-build. Re-spinning + re-syncing and retrying (attempt $((attempt + 1))/$MAX_ATTEMPTS)..."
        attempt=$((attempt + 1))
        AUTO_SPIN=1                 # a recovery spin is always warranted
        ensure_vm_up                # exits INFRA_UNAVAILABLE if it can't spin
        choose_transport            # re-probe — fresh VM has a new external IP
        continue
    fi

    log "VM $VM_NAME unavailable ($status_after) after $MAX_ATTEMPTS attempt(s) — signaling INFRA_UNAVAILABLE ($INFRA_UNAVAILABLE) for local fallback."
    exit $INFRA_UNAVAILABLE
done
