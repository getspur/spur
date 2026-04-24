#!/usr/bin/env python3
"""
Simulation script to test kimi ACP protocol behavior.

Spawns `kimi acp` as a subprocess, performs the ACP handshake,
creates a session, sends a simple prompt, and captures all
session/update notifications to analyze what kimi sends back.

Key question: does kimi send `agent_message_chunk` or only
`agent_thought_chunk`? This determines whether SPUR's TUI
can display kimi's responses.
"""

import json
import subprocess
import sys
import threading
import time
import uuid
from collections import defaultdict


def make_jsonrpc(id_, method, params=None):
    msg = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def read_responses(proc, responses, notifications):
    """Thread worker: read JSON-RPC responses/notifications from stdout."""
    buf = ""
    while True:
        try:
            chunk = proc.stdout.read(1)
            if not chunk:
                break
        except Exception as e:
            print(f"[READER] Error reading stdout: {e}", file=sys.stderr)
            break
        buf += chunk
        if chunk == "\n":
            line = buf.strip()
            buf = ""
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                print(f"[READER] JSON parse error: {line[:200]}", file=sys.stderr)
                continue

            if "method" in obj and "id" not in obj:
                notifications.append(obj)
            else:
                responses.append(obj)


def send(proc, obj):
    line = json.dumps(obj, separators=(",", ":"))
    proc.stdin.write(line + "\n")
    proc.stdin.flush()
    print(f"[SEND] {line[:200]}")


def wait_for_response(responses, timeout=30):
    start = time.time()
    while time.time() - start < timeout:
        if responses:
            return responses.pop(0)
        time.sleep(0.05)
    return None


def main():
    print("=" * 70)
    print("KIMI ACP PROTOCOL SIMULATOR")
    print("=" * 70)

    cmd = ["kimi", "acp"]
    print(f"[INFO] Spawning: {' '.join(cmd)}")

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )

    # Start stderr printer
    def print_stderr():
        for line in proc.stderr:
            print(f"[STDERR] {line.rstrip()}")

    stderr_thread = threading.Thread(target=print_stderr, daemon=True)
    stderr_thread.start()

    responses = []
    notifications = []
    reader = threading.Thread(
        target=read_responses, args=(proc, responses, notifications), daemon=True
    )
    reader.start()

    # ---- 1. Initialize ----
    init_req = make_jsonrpc(
        0,
        "initialize",
        {
            "protocolVersion": 1,
            "clientInfo": {"name": "spur-sim", "version": "0.1.0"},
        },
    )
    send(proc, init_req)
    init_resp = wait_for_response(responses, timeout=10)
    if init_resp is None:
        print("[FAIL] Initialize timed out")
        proc.kill()
        return 1
    print(f"[RECV] Initialize response received")
    print(json.dumps(init_resp, indent=2)[:800])

    # ---- 2. Create session ----
    session_id = str(uuid.uuid4())
    new_session_req = make_jsonrpc(
        1,
        "session/new",
        {"sessionId": session_id, "cwd": "/Volumes/Projects/spur", "mcpServers": []},
    )
    send(proc, new_session_req)
    session_resp = wait_for_response(responses, timeout=10)
    if session_resp is None:
        print("[FAIL] New session timed out")
        proc.kill()
        return 1
    print(f"[RECV] New session response received")
    print(json.dumps(session_resp, indent=2)[:800])

    # Kimi may return a different sessionId than requested; use the returned one
    actual_session_id = session_resp.get("result", {}).get("sessionId", session_id)
    if actual_session_id != session_id:
        print(f"[INFO] Server assigned different sessionId: {actual_session_id}")
        session_id = actual_session_id

    # Drain any pending notifications (available_commands_update etc)
    time.sleep(0.5)
    while notifications:
        notif = notifications.pop(0)
        print(f"[DRAIN] {notif.get('method')} during session setup")

    # ---- 3. Send a simple prompt ----
    prompt_req = make_jsonrpc(
        2,
        "session/prompt",
        {
            "sessionId": session_id,
            "prompt": [
                {"type": "text", "text": "Say hello in exactly 5 words."}
            ],
        },
    )
    send(proc, prompt_req)

    # ---- 4. Collect notifications until prompt completes ----
    print("\n[INFO] Collecting notifications until prompt completes...")
    start = time.time()
    prompt_done = False
    prompt_timeout = 120
    all_notifs = []

    while time.time() - start < prompt_timeout and not prompt_done:
        # Check for prompt response
        for i, resp in enumerate(responses):
            if resp.get("id") == 2:
                print(f"\n[RECV] Prompt response received")
                print(json.dumps(resp, indent=2)[:800])
                responses.pop(i)
                prompt_done = True
                break

        # Process notifications
        while notifications:
            notif = notifications.pop(0)
            all_notifs.append(notif)
            method = notif.get("method", "")
            params = notif.get("params", {})
            update = params.get("update", {})
            session_update = update.get("sessionUpdate", "")

            if method == "session/update":
                content = update.get("content", {})
                if isinstance(content, list):
                    content = content[0] if content else {}
                text = ""
                if isinstance(content, dict):
                    text = content.get("text", "")
                elif isinstance(content, str):
                    text = content

                print(
                    f"[NOTIF] {session_update:25s} | text_len={len(text):4d} | text={text[:80]!r}"
                )
            elif method == "session/request_permission":
                print(f"[NOTIF] PERMISSION REQUEST: {json.dumps(params, indent=2)[:300]}")
            else:
                print(f"[NOTIF] method={method} | {json.dumps(params, indent=2)[:200]}")

        time.sleep(0.1)

    if not prompt_done:
        print("[WARN] Prompt did not complete within timeout")

    # ---- 5. Summary ----
    print("\n" + "=" * 70)
    print("SUMMARY")
    print("=" * 70)

    counts = defaultdict(int)
    total_text_len = defaultdict(int)
    sample_texts = defaultdict(list)

    for notif in all_notifs:
        if notif.get("method") != "session/update":
            continue
        update = notif.get("params", {}).get("update", {})
        variant = update.get("sessionUpdate", "unknown")
        counts[variant] += 1

        content = update.get("content", {})
        if isinstance(content, list):
            content = content[0] if content else {}
        text = ""
        if isinstance(content, dict):
            text = content.get("text", "")
        elif isinstance(content, str):
            text = content
        total_text_len[variant] += len(text)

        if len(sample_texts[variant]) < 3:
            sample_texts[variant].append(text[:120])

    print(f"\nTotal notifications collected: {len(all_notifs)}")
    print("\nBreakdown by sessionUpdate variant:")
    print("-" * 70)
    for variant in sorted(counts.keys()):
        print(f"  {variant:25s}: {counts[variant]:5d} events, {total_text_len[variant]:6d} chars total")
        for i, sample in enumerate(sample_texts[variant]):
            print(f"    sample {i+1}: {sample!r}")
        print()

    has_agent_message = counts.get("agent_message_chunk", 0) > 0
    has_agent_thought = counts.get("agent_thought_chunk", 0) > 0

    print("-" * 70)
    print("VERDICT:")
    if has_agent_message and has_agent_thought:
        print("  ✅ kimi sends BOTH agent_message_chunk and agent_thought_chunk")
    elif has_agent_message:
        print("  ⚠️  kimi sends ONLY agent_message_chunk (no thinking)")
    elif has_agent_thought:
        print("  ⚠️  kimi sends ONLY agent_thought_chunk (response hidden in thinking)")
    else:
        print("  ❌ kimi sends NEITHER — no displayable content!")
    print("-" * 70)

    # Cleanup
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    return 0


if __name__ == "__main__":
    sys.exit(main())
