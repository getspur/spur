#!/bin/bash
# Mock ACP agent: responds to initialize, then sends an unsolicited
# `logout` request back to the client and records the client's response.

set -u

response_file=${1:?response file required}
rm -f "$response_file"
sent_logout=0

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    if printf '%s' "$line" | grep -q '"id":"agent-logout-1"'; then
        printf '%s\n' "$line" > "$response_file"
        continue
    fi

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false,"promptCapabilities":{}},"authMethods":[]}}'
            if [ "$sent_logout" -eq 0 ]; then
                sent_logout=1
                echo '{"jsonrpc":"2.0","id":"agent-logout-1","method":"logout","params":{}}'
            fi
            ;;
        session/cancel)
            ;;
    esac
done
