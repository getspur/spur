#!/bin/bash
# Mock ACP agent: responds to initialize + session/load, then — AFTER the
# session/load response — sleeps 50 ms and emits a delayed
# `session/update{available_commands_update}` notification.
#
# Used to reproduce the claude-code-acp dropped-notification race:
# the ACP SDK schedules `session_notification` callbacks on its LocalSet
# AFTER the `session/load` response frame has been consumed.
# `NativeAcpConnection` swaps `notification_tx` → `dead_tx` at that point,
# so the delayed `available_commands_update` is silently dropped.
#
# The FIX (Task 2+) will add a connection-scoped broadcast channel and a
# `subscribe_session_notifications()` method on `AgentConnection` so callers
# can receive session notifications that arrive outside a prompt stream.

set -u

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id_json=$(printf '%s' "$line" | sed -E -n 's/.*"id"[[:space:]]*:[[:space:]]*("[^"]*"|[0-9]+).*/\1/p')

    case "$method" in
        initialize)
            # Advertise loadSession: true so NativeAcpConnection uses load_session.
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/load)
            # Return a minimal load response acknowledging the session.
            echo '{"jsonrpc":"2.0","id":'"$id_json"',"result":{"sessionId":"test-session"}}'
            # Sleep 50 ms, then emit the delayed available_commands_update.
            # At this point NativeAcpConnection has already consumed the response
            # and (in the buggy code path) swapped notification_tx → dead_tx,
            # so this notification is dropped before it reaches any subscriber.
            sleep 0.05
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"test-session","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"test-cmd","description":"t"}]}}}'
            ;;
        session/cancel)
            # Notifications have no id; nothing to reply with.
            ;;
    esac
done
