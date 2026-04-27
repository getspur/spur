#!/bin/bash
# Mock ACP agent: responds to initialize + session/new + session/prompt,
# emits a `session/update` notification BEFORE the prompt_response, then
# sleeps 200ms and emits a trailing `session/update` AFTER the response.
#
# Used to reproduce the H5 dead_tx race in `NativeAcpConnection`:
# when `connection.prompt().await` returns (because the prompt_response
# frame was consumed), the ACP thread immediately swaps `notification_tx`
# for a throwaway `dead_tx`. Any `session_notification` callback
# scheduled on the LocalSet after that swap is routed to the dead channel
# and silently dropped — e.g., the trailing chunk emitted here.

set -u

# Disable stdout buffering for each `echo` (line-by-line is the contract).
# Bash's default is already line-buffered on pipes, but we're paranoid.

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"test-session"}}'
            ;;
        session/prompt)
            # 1. Emit the leading chunk FIRST so it is clearly in-band.
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"test-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first"}}}}'
            # 2. Send the prompt_response — this unblocks connection.prompt().await
            #    on the orchestrator side, triggering the notification_tx -> dead_tx swap.
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"stopReason":"end_turn"}}'
            # 3. Sleep, then emit the trailing chunk. Because the dead_tx swap
            #    has already happened, this notification is dropped in the buggy
            #    code path. A correct implementation (grace window / buffer
            #    pattern) keeps the live sender alive long enough to forward it.
            sleep 0.2
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"test-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second"}}}}'
            ;;
        session/cancel)
            # Notifications have no id; nothing to reply with.
            ;;
    esac
done
