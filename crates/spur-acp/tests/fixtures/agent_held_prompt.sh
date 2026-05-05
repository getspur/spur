#!/bin/bash
# Mock ACP agent for the cancel-during-prompt regression
# (`cancel_during_in_flight_prompt_returns_within_250ms`).
#
# - initialize / session/new respond immediately.
# - session/prompt holds its response until $SPUR_TEST_RELEASE_FILE exists
#   on disk (or the loop times out at ~10 s as a safety net).
# - session/cancel writes "1" to $SPUR_TEST_CANCEL_SEEN so the test can
#   assert the notification reached us.
#
# The two paths are passed via environment variables so the test can place
# them in a tempdir without arg-quoting headaches.
set -u

: "${SPUR_TEST_RELEASE_FILE:?must be set by test harness}"
: "${SPUR_TEST_CANCEL_SEEN:?must be set by test harness}"

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
                    if [ -e "$SPUR_TEST_RELEASE_FILE" ]; then
                        break
                    fi
                    sleep 0.1
                done
                printf '%s\n' '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"stopReason":"end_turn"}}'
            ) &
            ;;
        session/cancel)
            printf '1' > "$SPUR_TEST_CANCEL_SEEN"
            ;;
    esac
done

# Wait for any backgrounded prompt-responder to flush before we exit.
wait
