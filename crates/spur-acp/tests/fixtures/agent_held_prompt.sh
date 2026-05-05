#!/bin/bash
# Mock ACP agent for the cancel-during-prompt regression
# (`cancel_during_in_flight_prompt_returns_within_250ms`).
#
# Args (positional, NOT env vars — env vars would race across parallel
# `cargo test` workers because `std::env::set_var` mutates process-global
# state):
#   $1  release-prompt file path. The session/prompt response is held
#       until this path exists on disk.
#   $2  cancel-seen file path. Written when session/cancel arrives so
#       the test can assert the notification reached the agent.
#
# - initialize / session/new respond immediately.
# - session/prompt holds its response until $1 exists (or the loop times
#   out at ~10 s as a safety net).
# - session/cancel writes "1" to $2.
set -u

if [ "$#" -lt 2 ]; then
    echo "agent_held_prompt.sh: missing args: <release_file> <cancel_seen_file>" >&2
    exit 64
fi

RELEASE_FILE="$1"
CANCEL_SEEN_FILE="$2"

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"held-session"}}'
            ;;
        session/prompt)
            # Background a process that waits for the release file, then sends
            # the prompt response. This way the bash main loop continues to
            # read stdin and can react to a session/cancel notification.
            (
                for _ in $(seq 1 100); do
                    if [ -e "$RELEASE_FILE" ]; then
                        break
                    fi
                    sleep 0.1
                done
                printf '%s\n' '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"stopReason":"end_turn"}}'
            ) &
            ;;
        session/cancel)
            printf '1' > "$CANCEL_SEEN_FILE"
            ;;
    esac
done

# Wait for any backgrounded prompt-responder to flush before we exit.
wait
