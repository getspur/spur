"""Real-process regression tests for ACP terminal probing and continuation."""

from __future__ import annotations

import io
import json
import os
import queue
import shlex
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

from scripts import probe_acp_capabilities as probe


class GoldenLexingTests(unittest.TestCase):
    def test_probe_preserves_exact_shared_canonical_argv(self) -> None:
        corpus_path = (
            Path(__file__).resolve().parents[1]
            / "crates/spur-acp/tests/fixtures/grok_terminal_golden.json"
        )
        corpus = json.loads(corpus_path.read_text())
        for case in corpus["cases"] + corpus["lexical_only"]:
            expected = [case["canonical"]["command"], *case["canonical"]["args"]]
            for index, packed in enumerate(case["packed"]):
                with self.subTest(case=case["id"], wrapper=index):
                    self.assertEqual(
                        probe._split_terminal_shell_words(packed["command"]), expected
                    )


@unittest.skipUnless(os.name == "posix", "shell lifecycle requires POSIX")
class TerminalTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.responses: queue.Queue = queue.Queue()
        self.assertTrue(
            hasattr(probe, "TerminalHost"), "probe must implement advertised terminals"
        )
        self.host = probe.TerminalHost(
            self.responses.put, Path(self.tmp.name), "strict"
        )
        self.addCleanup(self.host.close)
        self.request_id = 0

    def send(self, method: str, **params: object) -> int:
        self.request_id += 1
        self.assertTrue(
            self.host.handle(
                {
                    "jsonrpc": "2.0",
                    "id": self.request_id,
                    "method": method,
                    "params": {"sessionId": "s", **params},
                }
            )
        )
        return self.request_id

    def call(self, method: str, **params: object) -> dict:
        request_id = self.send(method, **params)
        response = self.responses.get(timeout=5)
        self.assertEqual(response["id"], request_id)
        return response

    def create(self, command: str, args: list[str], **params: object) -> str:
        response = self.call("terminal/create", command=command, args=args, **params)
        self.assertIn("result", response)
        return response["result"]["terminalId"]

    def test_nonzero_exit_preserves_output_and_allows_next_command(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "printf failure >&2; exit 7"])
        self.assertEqual(
            self.call("terminal/wait_for_exit", terminalId=terminal)["result"][
                "exitCode"
            ],
            7,
        )
        output = self.call("terminal/output", terminalId=terminal)["result"]
        self.assertEqual(output["output"], "failure")
        self.assertEqual(output["exitStatus"]["exitCode"], 7)
        self.assertEqual(
            self.call("terminal/release", terminalId=terminal)["result"], {}
        )
        terminal = self.create("/bin/sh", ["-c", "printf recovered"])
        self.call("terminal/wait_for_exit", terminalId=terminal)
        self.assertEqual(
            self.call("terminal/output", terminalId=terminal)["result"]["output"],
            "recovered",
        )

    def test_spawn_failure_is_rpc_error_then_host_remains_usable(self) -> None:
        result = self.call("terminal/create", command="/nonexistent/spur-probe-command")
        self.assertEqual(result["error"]["code"], -32603)
        terminal = self.create("/bin/sh", ["-c", "exit 0"])
        self.assertEqual(
            self.call("terminal/wait_for_exit", terminalId=terminal)["result"][
                "exitCode"
            ],
            0,
        )

    def test_grok_compat_is_explicit_and_preserves_quoted_script(self) -> None:
        script = "printf '%s' 'quoted; 世界'"
        packed = "/bin/bash -c " + shlex.quote(script)
        self.assertIn("error", self.call("terminal/create", command=packed))
        self.host.mode = "grok"
        terminal = self.create(packed, [])
        self.call("terminal/wait_for_exit", terminalId=terminal)
        self.assertEqual(
            self.call("terminal/output", terminalId=terminal)["result"]["output"],
            "quoted; 世界",
        )

    def test_grok_probe_matches_shared_script_goldens(self) -> None:
        import shutil

        corpus_path = (
            Path(__file__).resolve().parents[1]
            / "crates/spur-acp/tests/fixtures/grok_terminal_golden.json"
        )
        corpus = json.loads(corpus_path.read_text())
        self.host.mode = "grok"
        for case in corpus["cases"]:
            missing = [
                runtime for runtime in case["requires"] if not shutil.which(runtime)
            ]
            if missing:
                with self.subTest(case=case["id"]):
                    if any(
                        runtime in corpus["required_runtimes"] for runtime in missing
                    ):
                        self.fail(f"required runtimes missing: {missing}")
                    self.skipTest(
                        f"optional runtimes missing for {case['id']}: {missing}"
                    )
                continue
            for index, request in enumerate(case["packed"]):
                with self.subTest(case=case["id"], wrapper=index):
                    root = Path(self.tmp.name) / f"{case['id']}-{index}"
                    root.mkdir()
                    for name, content in case["files"].items():
                        path = root / name
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text(content)
                    env = {"BASH_ENV": "/dev/null", "ENV": "/dev/null", **case["env"]}
                    terminal = self.create(
                        request["command"],
                        request["args"],
                        cwd=str(root),
                        env=[{"name": k, "value": v} for k, v in env.items()],
                    )
                    status = self.call("terminal/wait_for_exit", terminalId=terminal)[
                        "result"
                    ]
                    result = self.call("terminal/output", terminalId=terminal)["result"]
                    self.call("terminal/release", terminalId=terminal)
                    expected = case["expected"]
                    self.assertEqual(status["exitCode"], expected["exit_code"])
                    if "stderr_suffix" in expected:
                        self.assertTrue(
                            result["output"].endswith(expected["stderr_suffix"]), result
                        )
                    else:
                        self.assertEqual(
                            result["output"], expected["stdout"] + expected["stderr"]
                        )

    def test_compat_does_not_interpret_arbitrary_shell_strings(self) -> None:
        self.host.mode = "grok"
        result = self.call("terminal/create", command="printf unsafe; exit 0")
        self.assertIn("error", result)

    def test_stdin_eof_cwd_and_env_are_honored(self) -> None:
        terminal = self.create(
            "/bin/sh",
            ["-c", 'cat; printf "%s:%s" "$PROBE_VALUE" "$PWD"'],
            env=[{"name": "PROBE_VALUE", "value": "ok"}],
        )
        self.call("terminal/wait_for_exit", terminalId=terminal)
        output = self.call("terminal/output", terminalId=terminal)["result"]["output"]
        self.assertEqual(output, f"ok:{Path(self.tmp.name).resolve()}")

    def test_utf8_truncation_retains_tail_and_zero_limit(self) -> None:
        for limit, expected in [(4, "界x"), (0, "")]:
            with self.subTest(limit=limit):
                terminal = self.create(
                    sys.executable,
                    ["-c", "print('a世界x', end='')"],
                    outputByteLimit=limit,
                )
                self.call("terminal/wait_for_exit", terminalId=terminal)
                result = self.call("terminal/output", terminalId=terminal)["result"]
                self.assertEqual(result["output"], expected)
                self.assertTrue(result["truncated"])

    def test_wait_does_not_block_kill_or_other_callbacks(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "sleep 60"])
        wait_id = self.send("terminal/wait_for_exit", terminalId=terminal)
        kill_id = self.send("terminal/kill", terminalId=terminal)
        responses = {
            r["id"]: r
            for r in [self.responses.get(timeout=5), self.responses.get(timeout=5)]
        }
        self.assertEqual(responses[kill_id]["result"], {})
        self.assertIsNotNone(responses[wait_id]["result"]["signal"])
        self.assertIn(
            "exitStatus", self.call("terminal/output", terminalId=terminal)["result"]
        )

    def test_release_stops_running_command_and_invalidates_id(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "sleep 60"])
        self.call("terminal/release", terminalId=terminal)
        self.assertIn("error", self.call("terminal/output", terminalId=terminal))

    def test_terminal_is_scoped_to_session(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "exit 0"])
        self.assertIn(
            "error",
            self.call("terminal/output", terminalId=terminal, sessionId="other"),
        )

    def test_close_reaps_unreleased_child_and_unblocks_wait(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "sleep 60"])
        child = self.host.terminals[terminal]
        wait_id = self.send("terminal/wait_for_exit", terminalId=terminal)
        self.host.close()
        self.assertIsNotNone(child.proc.returncode)
        self.assertFalse(child.reader.is_alive())
        self.assertEqual(self.responses.get(timeout=5)["id"], wait_id)

    def test_nullable_output_limit_uses_default(self) -> None:
        terminal = self.create("/bin/sh", ["-c", "printf ok"], outputByteLimit=None)
        self.call("terminal/wait_for_exit", terminalId=terminal)
        self.assertEqual(
            self.call("terminal/output", terminalId=terminal)["result"]["output"], "ok"
        )

    def test_malformed_callback_params_returns_error_without_crashing(self) -> None:
        self.host.handle({"id": 0, "method": "terminal/create", "params": ["bad"]})
        self.assertEqual(self.responses.get(timeout=5)["error"]["code"], -32602)


FAKE_AGENT = r"""
import json, sys
turn = 0
pending = None
def send(m):
    print(json.dumps(dict(jsonrpc="2.0", **m)), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    method = m.get("method")
    if method == "initialize":
        send({"id":m["id"], "result":{"protocolVersion":1,"agentCapabilities":{}}})
    elif method == "session/new":
        send({"id":m["id"], "result":{"sessionId":"same-session"}})
    elif method == "session/prompt":
        turn += 1
        assert m["params"]["sessionId"] == "same-session"
        if turn == 1:
            pending = m["id"]
            send({"id":0,"method":"terminal/create","params":{"sessionId":"same-session","command":"/nonexistent/spur-probe-command"}})
        else:
            send({"method":"session/update","params":{"sessionId":"same-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"FOLLOW_UP_OK"}}}})
            send({"id":m["id"],"result":{"stopReason":"end_turn"}})
    elif m.get("id") == 0 and pending:
        assert "error" in m
        send({"method":"session/update","params":{"sessionId":"same-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"failed-shell","status":"failed"}}})
        send({"id":pending,"result":{"stopReason":"end_turn"}})
        pending = None
"""


class ContinuationTests(unittest.TestCase):
    def test_probe_records_follow_up_after_terminal_failure(self) -> None:
        self.assertTrue(
            hasattr(probe, "TerminalHost"), "live callback support is missing"
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake = root / "fake.py"
            fake.write_text(FAKE_AGENT)
            args = probe.parse_cli(
                [
                    "--command",
                    sys.executable,
                    "--args",
                    str(fake),
                    "--cwd",
                    tmp,
                    "--out",
                    str(root / "frames.jsonl"),
                    "--report",
                    str(root / "report.json"),
                    "--no-try-set",
                    "--prompt",
                    "fail shell",
                    "--follow-up-prompt",
                    "continue",
                    "--terminal-mode",
                    "strict",
                    "--preamble-timeout",
                    "0",
                    "--quiet",
                ]
            )
            with redirect_stdout(io.StringIO()):
                self.assertEqual(probe.run_probe(args), 0)
            report = json.loads((root / "report.json").read_text())
            self.assertEqual(len(report["prompt_results"]), 2)
            self.assertEqual(
                report["prompt_result"], report["prompt_results"][0]["rpc"]
            )
            self.assertEqual(
                report["prompt_results"][0]["failed_tool_calls"], ["failed-shell"]
            )
            self.assertEqual(
                report["prompt_results"][1]["assistant_text"], "FOLLOW_UP_OK"
            )
            self.assertEqual(report["prompt_results"][1]["rpc"]["status"], "ok")
            frames = [
                json.loads(line)
                for line in (root / "frames.jsonl").read_text().splitlines()
            ]
            init = next(
                r["msg"] for r in frames if r["msg"].get("method") == "initialize"
            )
            self.assertTrue(init["params"]["clientCapabilities"]["terminal"])
            self.assertFalse(init["params"]["clientCapabilities"]["fs"]["readTextFile"])

    def test_terminal_default_is_off(self) -> None:
        args = probe.parse_cli(["--command", "unused"])
        self.assertEqual(getattr(args, "terminal_mode", None), "off")

    def test_timeout_cancels_without_overlapping_follow_up_and_exits_nonzero(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake = root / "fake.py"
            fake.write_text(FAKE_AGENT.replace('pending = m["id"]', "continue"))
            args = probe.parse_cli(
                [
                    "--command",
                    sys.executable,
                    "--args",
                    str(fake),
                    "--cwd",
                    tmp,
                    "--out",
                    str(root / "frames.jsonl"),
                    "--report",
                    str(root / "report.json"),
                    "--no-try-set",
                    "--prompt",
                    "hang",
                    "--follow-up-prompt",
                    "do not send",
                    "--timeout",
                    "0.1",
                    "--preamble-timeout",
                    "0",
                    "--quiet",
                ]
            )
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                code = probe.run_probe(args)
            report = json.loads((root / "report.json").read_text())
            self.assertEqual(report["prompt_results"][0]["rpc"]["status"], "timeout")
            self.assertEqual(report["prompts_requested"], 2)
            frames = [
                json.loads(line)["msg"]
                for line in (root / "frames.jsonl").read_text().splitlines()
            ]
            methods = [m.get("method") for m in frames]
            self.assertEqual(methods.count("session/prompt"), 1)
            self.assertEqual(methods.count("session/cancel"), 1)
            init = next(m for m in frames if m.get("method") == "initialize")
            self.assertFalse(init["params"]["clientCapabilities"]["terminal"])
            self.assertEqual(
                code, 2, "an incomplete prompt probe must not report CLI success"
            )


if __name__ == "__main__":
    unittest.main()
