#!/usr/bin/env python3
import time
import unittest
from argparse import Namespace
from pathlib import Path

import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

import probe_acp_subagents as probe


class SlowStream:
    def __init__(self, chunks):
        self.chunks = list(chunks)

    def read(self, _size):
        time.sleep(0.02)
        if self.chunks:
            return self.chunks.pop(0)
        return ""


class FakeProc:
    def __init__(self):
        self.stdout = SlowStream(["stdout-tail"])
        self.stderr = SlowStream(["stderr-tail"])

    def poll(self):
        return 0

    def wait(self):
        return 0

    def kill(self):
        pass


class AcpSubagentProbeTests(unittest.TestCase):
    def test_codex_command_defaults_to_codex_acp_package(self):
        args = Namespace(
            agent="codex",
            codex_package="@zed-industries/codex-acp@0.12.0",
            codex_config=[],
        )

        self.assertEqual(
            probe.agent_command(args),
            ["npx", "--yes", "@zed-industries/codex-acp@0.12.0"],
        )

    def test_kimi_command_accepts_yolo_and_afk(self):
        args = Namespace(agent="kimi", yolo=True, afk=True)

        self.assertEqual(probe.agent_command(args), ["kimi", "-y", "--afk", "acp"])

    def test_gemini_command_uses_repo_seed_acp_args(self):
        args = Namespace(
            agent="gemini",
            gemini_model="deep-thinker",
            gemini_skip_trust=False,
            gemini_no_sandbox=False,
        )

        self.assertEqual(
            probe.agent_command(args),
            ["gemini", "--acp", "-y", "-m", "deep-thinker"],
        )

    def test_gemini_command_can_skip_trust_for_startup_probe(self):
        args = Namespace(
            agent="gemini",
            gemini_model="",
            gemini_skip_trust=True,
            gemini_no_sandbox=True,
        )

        self.assertEqual(
            probe.agent_command(args),
            ["gemini", "--acp", "--skip-trust", "--no-sandbox", "-y"],
        )

    def test_extract_tool_snapshot_keeps_meta_and_raw_fields(self):
        notification = {
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call-1",
                    "title": "Task",
                    "kind": "other",
                    "rawInput": {"description": "inspect file"},
                    "rawOutput": {"summary": "done"},
                    "_meta": {"codex": {"parentToolUseId": "parent-1"}},
                }
            },
        }

        snap = probe.extract_tool_snapshot(notification)

        self.assertEqual(snap["toolCallId"], "call-1")
        self.assertEqual(snap["rawInput"], {"description": "inspect file"})
        self.assertEqual(snap["rawOutput"], {"summary": "done"})
        self.assertEqual(snap["_meta"], {"codex": {"parentToolUseId": "parent-1"}})

    def test_response_error_message_formats_json_rpc_error(self):
        response = {"error": {"code": -32000, "message": "Gemini API key is missing"}}

        self.assertEqual(
            probe.response_error_message(response),
            "code=-32000 Gemini API key is missing",
        )

    def test_response_error_message_ignores_success_response(self):
        self.assertIsNone(probe.response_error_message({"result": {"sessionId": "s"}}))

    def test_terminal_wait_response_drains_reader_threads(self):
        terminal = probe.TerminalState(FakeProc(), 1024)

        self.assertEqual(terminal.wait_response(), {"exitCode": 0})
        output = terminal.output_response()["output"]

        self.assertIn("stdout-tail", output)
        self.assertIn("stderr-tail", output)
        self.assertFalse(terminal._stdout.is_alive())
        self.assertFalse(terminal._stderr.is_alive())

    def test_probe_client_capabilities_do_not_advertise_writes(self):
        caps = probe.probe_client_capabilities()

        self.assertEqual(
            caps["fs"],
            {"readTextFile": True, "writeTextFile": False},
        )

    def test_subagent_clue_detection_catches_parent_meta(self):
        update = {
            "sessionUpdate": "tool_call",
            "title": "Read",
            "_meta": {"kimi": {"parentToolUseId": "task-1"}},
        }

        self.assertTrue(probe.has_subagent_clue(update))

    def test_subagent_clue_detection_ignores_plain_message(self):
        update = {"sessionUpdate": "agent_message_chunk", "content": "hello"}

        self.assertFalse(probe.has_subagent_clue(update))


if __name__ == "__main__":
    unittest.main()
