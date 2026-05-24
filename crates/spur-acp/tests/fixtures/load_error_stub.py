#!/usr/bin/env python3
"""Stub ACP agent that errors on session/load."""

import json
import sys


def write_response(response):
    sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        continue

    method = req.get("method")
    if method == "initialize":
        write_response(
            {
                "jsonrpc": "2.0",
                "id": req.get("id"),
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {},
                    },
                    "authMethods": [],
                },
            }
        )
    elif method == "session/load":
        params = req.get("params") or {}
        session_id = params.get("sessionId") or ""
        write_response(
            {
                "jsonrpc": "2.0",
                "id": req.get("id"),
                "error": {
                    "code": -32002,
                    "message": "Resource not found: " + session_id,
                    "data": {"uri": session_id},
                },
            }
        )
