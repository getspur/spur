#!/bin/bash
# Mock ACP agent that accepts initialization and session creation, then
# returns a JSON-RPC error for every session/prompt request.

set -u

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"prompt-error-session"}}'
            ;;
        session/prompt)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"error":{"code":-32000,"message":"prompt exploded"}}'
            ;;
        session/cancel)
            # Notifications have no id and require no response.
            ;;
    esac
done
