#!/usr/bin/env bash
# Live resource consumption snapshot of the build VM.
#
# Usage:
#   ./monitor.sh                  # one-shot snapshot
#   ./monitor.sh --watch          # repeat every 10s (Ctrl-C to stop)
#   ./monitor.sh --watch=5        # custom interval
#   ./monitor.sh --short          # one-shot, terse (PSI + disk + sccache only)
#
# What it shows:
#   - PSI (cpu / mem / io pressure stall info)
#   - Memory usage + headroom
#   - Active rustc / cargo / rust-lld build process counts
#   - Top procs by CPU
#   - Disk usage breakdown (/mnt/cargo split by sub-dir + worktrees)
#   - sccache stats (hit/miss rate, errors)
#
# Read the output to spot:
#   - IO pressure rising under burst linking (rust-lld concurrent count high)
#   - Disk approaching the 85% watchdog threshold
#   - Memory pressure if too many concurrent links
#   - sccache hit rate trending up (cache warming) or stuck at 0% (something broken)

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=config.env
source "$SCRIPT_DIR/config.env"

WATCH_INTERVAL=0
SHORT=0
for arg in "$@"; do
    case "$arg" in
        --watch) WATCH_INTERVAL=10 ;;
        --watch=*) WATCH_INTERVAL="${arg#--watch=}" ;;
        --short) SHORT=1 ;;
        -h|--help) sed -n '3,21p' "$0"; exit 0 ;;
        *) echo "unknown arg: $arg"; exit 2 ;;
    esac
done

# Remote command body — heredoc-piped so we don't fight shell quoting.
# Set SHORT in the outer env so the inner script sees it.
remote_snapshot () {
    local short=$1
    gcloud compute ssh --project="$GCP_PROJECT" --zone="$GCP_ZONE" \
        --tunnel-through-iap --quiet "$VM_NAME" -- "SHORT=$short bash -s" <<'REMOTE'
set -u
echo "=== $(date -u +%FT%TZ) === uptime: $(uptime | awk -F'load average: ' '{print $2}')"
echo
echo "--- PSI (some/full, avg10/60/300) ---"
for kind in cpu memory io; do
    awk -v k="${kind:0:3}" '
        $1=="some" { gsub("avg10=","",$2); gsub("avg60=","",$3); gsub("avg300=","",$4);
                     printf "%s some %s / %s / %s\n", k, $2, $3, $4 }
        $1=="full" { gsub("avg10=","",$2); gsub("avg60=","",$3); gsub("avg300=","",$4);
                     printf "%s full %s / %s / %s\n", k, $2, $3, $4 }
    ' /proc/pressure/$kind
done
echo
echo "--- memory ---"
free -h | awk 'NR==1 || $1=="Mem:"'
echo
echo "--- builds in flight ---"
printf "rustc: %s   cargo: %s   rust-lld: %s\n" \
    "$(pgrep -c rustc)" "$(pgrep -c cargo)" "$(pgrep -c rust-lld)"

[[ "$SHORT" == "1" ]] || {
    echo
    echo "--- top 5 by CPU ---"
    ps -eo pid,user,%cpu,%mem,rss,comm --sort=-%cpu --no-headers \
        | grep -v -E "^\s+[0-9]+ [a-z]+\+\s+[0-9.]+\s+[0-9.]+\s+[0-9]+ (ps|sshd)\$" \
        | head -5

    echo
    echo "--- top 3 by RSS ---"
    ps -eo pid,user,%mem,rss,comm --sort=-rss --no-headers | head -3
}

echo
echo "--- disk /mnt/cargo ---"
df -h /mnt/cargo | awk 'NR==1 || NR==2'
USED_PCT=$(df --output=pcent /mnt/cargo | tail -1 | tr -dc 0-9)
if [[ "$USED_PCT" -ge 85 ]]; then
    echo "*** WARNING: /mnt/cargo at ${USED_PCT}% — watchdog will prune idle worktrees ***"
fi

[[ "$SHORT" == "1" ]] || {
    echo
    echo "--- /mnt/cargo breakdown ---"
    sudo du -sh /mnt/cargo/targets/main /mnt/cargo/cargo-home /mnt/cargo/rustup 2>/dev/null
    WT_COUNT=$(ls /mnt/cargo/targets/worktrees/ 2>/dev/null | wc -l)
    echo "worktrees ($WT_COUNT):"
    sudo du -sh /mnt/cargo/targets/worktrees/* 2>/dev/null | sort -h | head -10
    [[ "$WT_COUNT" -gt 10 ]] && echo "  ... ($((WT_COUNT - 10)) more)"
}

echo
echo "--- sccache ---"
sccache --show-stats 2>&1 \
    | grep -E "^(Compile requests( executed)?|Cache (hits|misses|hits rate|read errors|write errors)|Non-cacheable c)" \
    | head -12
REMOTE
}

if [[ "$WATCH_INTERVAL" -gt 0 ]]; then
    echo "watching every ${WATCH_INTERVAL}s — Ctrl-C to stop"
    while true; do
        clear
        remote_snapshot "$SHORT"
        sleep "$WATCH_INTERVAL"
    done
else
    remote_snapshot "$SHORT"
fi
