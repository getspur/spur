#!/bin/bash
# Mock native ACP agent that emits one notification 50 ms after each
# session/load or session/prompt terminal response.

set -u

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"sequencing-session"}}'
            ;;
        session/load)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"sequencing-session"}}'
            sleep 0.05
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sequencing-session","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"wire-history"}}}}'
            ;;
        session/prompt)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"stopReason":"end_turn"}}'
            sleep 0.05
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sequencing-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"prompt-tail"}}}}'
            ;;
        session/cancel)
            ;;
    esac
done
