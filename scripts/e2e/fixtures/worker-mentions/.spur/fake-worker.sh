#!/bin/bash
# Minimal ACP stub for worker-mention e2e journeys. Answers the three
# requests the TUI needs (initialize, session/new, session/prompt) and
# emits a canned agent reply during prompt handling. Advertises one model
# and one effort via session config options, mirroring the seeded agent-model catalog.
set -u

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    if [[ -z "$id_json" ]]; then
        continue
    fi

    case "$method" in
        initialize)
            if [[ -f .spur/session-list-enabled ]]; then
                echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{},"sessionCapabilities":{"list":{}}},"authMethods":[]}}'
            else
                echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            fi
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"e2e-session","configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"gpt-5-codex","options":[{"value":"gpt-5-codex","name":"GPT-5 Codex","description":"e2e frontier model"}]},{"id":"reasoning_effort","name":"Reasoning Effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"high","name":"High","description":"e2e deep reasoning"}]}]}}'
            ;;
        session/list)
            cwd_json=$(json_escape "${PWD:-/tmp}")
            updated_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessions":[{"sessionId":"e2e-populated-session","cwd":"'"$cwd_json"'","title":"E2E populated picker","updatedAt":"'"$updated_at"'"}],"nextCursor":null}}'
            ;;
        session/prompt)
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"e2e-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"e2e canned reply from fake worker"}}}}'
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"stopReason":"end_turn"}}'
            ;;
        *)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"error":{"code":-32601,"message":"method not supported by e2e stub"}}'
            ;;
    esac
done
