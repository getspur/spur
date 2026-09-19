"""Regressions for combining terminal modes with asynchronous ACP callbacks."""

from __future__ import annotations

import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from scripts import probe_acp_capabilities as probe


class FilesystemTests(unittest.TestCase):
    def test_write_read_and_line_slicing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(
                probe._handle_fs_request(
                    "fs/write_text_file",
                    {"path": "nested/file.txt", "content": "one\ntwo\nthree\n"},
                    root,
                ),
                {},
            )
            for method in ("fs/read_text_file", "fs/readTextFile"):
                for extra, expected in (
                    ({}, "one\ntwo\nthree\n"),
                    ({"line": 2, "limit": 1}, "two"),
                    ({"limit": 0}, ""),
                    ({"line": 9}, ""),
                ):
                    with self.subTest(method=method, extra=extra):
                        self.assertEqual(
                            probe._handle_fs_request(
                                method, {"path": "nested/file.txt", **extra}, root
                            ),
                            {"content": expected},
                        )


class TerminalMergeTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "requires process-group signaling")
    def test_completed_terminal_cleanup_does_not_signal_old_process_group(self):
        responses = queue.Queue()
        host = probe.TerminalHost(responses.put, Path.cwd(), "strict")
        try:
            host.handle(
                {
                    "id": 1,
                    "method": "terminal/create",
                    "params": {
                        "sessionId": "s",
                        "command": sys.executable,
                        "args": ["-c", "pass"],
                    },
                }
            )
            terminal_id = responses.get(timeout=1)["result"]["terminalId"]
            terminal = host.terminals[terminal_id]
            self.assertTrue(terminal.done.wait(timeout=2))
            terminal.reader.join(timeout=1)
            self.assertFalse(terminal.reader.is_alive())
            with patch.object(probe.os, "killpg") as kill_group:
                host.close()
            kill_group.assert_not_called()
        finally:
            host.close()

    def test_disabled_host_does_not_fall_back_to_shell_execution(self):
        responses = []

        class Client:
            send = staticmethod(responses.append)

        host = probe.TerminalHost(Client.send, Path.cwd(), "off")
        self.addCleanup(host.close)
        handler = probe._server_request_handler(Client(), True, host)
        with patch.object(probe.subprocess, "Popen") as spawn:
            handler(
                {
                    "id": 1,
                    "method": "terminal/create",
                    "params": {
                        "sessionId": "s",
                        "command": "printf forbidden",
                        "args": [],
                    },
                }
            )
        spawn.assert_not_called()
        self.assertEqual(responses[0]["error"]["code"], -32601)

    def test_closed_host_rejects_late_callbacks(self):
        responses = []
        host = probe.TerminalHost(responses.append, Path.cwd(), "strict")
        host.close()
        with patch.object(
            probe.subprocess, "Popen", side_effect=AssertionError("late spawn")
        ):
            self.assertTrue(
                host.handle(
                    {
                        "id": 1,
                        "method": "terminal/create",
                        "params": {
                            "sessionId": "s",
                            "command": sys.executable,
                            "args": ["-c", "pass"],
                        },
                    }
                )
            )
        self.assertIn("error", responses[0])

    @unittest.skipUnless(os.name == "posix", "requires process-group cleanup")
    def test_exit_and_cleanup_do_not_wait_for_inherited_pipe_eof(self):
        for cleanup in ("terminal/kill", "terminal/release", "close"):
            with (
                self.subTest(cleanup=cleanup),
                tempfile.TemporaryDirectory() as directory,
            ):
                responses = queue.Queue()
                host = probe.TerminalHost(responses.put, Path(directory), "strict")
                child = "import time; print('child-ready', flush=True); time.sleep(10)"
                parent = (
                    "import subprocess,sys; "
                    f"subprocess.Popen([sys.executable, '-c', {child!r}]); "
                    "print('parent-done', flush=True)"
                )
                try:
                    host.handle(
                        {
                            "id": 1,
                            "method": "terminal/create",
                            "params": {
                                "sessionId": "s",
                                "command": sys.executable,
                                "args": ["-c", parent],
                            },
                        }
                    )
                    terminal_id = responses.get(timeout=1)["result"]["terminalId"]
                    terminal = host.terminals[terminal_id]
                    deadline = time.monotonic() + 2
                    while (
                        "child-ready" not in terminal.output
                        and time.monotonic() < deadline
                    ):
                        time.sleep(0.01)
                    self.assertIn("child-ready", terminal.output)
                    host.handle(
                        {
                            "id": 2,
                            "method": "terminal/wait_for_exit",
                            "params": {
                                "sessionId": "s",
                                "terminalId": terminal_id,
                            },
                        }
                    )
                    self.assertEqual(
                        responses.get(timeout=0.5)["result"]["exitCode"], 0
                    )
                    self.assertTrue(
                        terminal.reader.is_alive(), "descendant still holds the pipe"
                    )
                    if cleanup == "close":
                        host.close()
                    else:
                        host.handle(
                            {
                                "id": 3,
                                "method": cleanup,
                                "params": {
                                    "sessionId": "s",
                                    "terminalId": terminal_id,
                                },
                            }
                        )
                        self.assertIn("result", responses.get(timeout=1))
                    terminal.reader.join(timeout=1)
                    self.assertFalse(
                        terminal.reader.is_alive(), "cleanup must stop descendant"
                    )
                finally:
                    host.close()


class AsyncCallbackTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "watchdog cleans the probe process group")
    def test_probe_timeout_survives_a_blocked_terminal_response(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            agent = root / "nonreading_agent.py"
            agent.write_text("""
import json,pathlib,sys,time
def send(**message):
    print(json.dumps({'jsonrpc':'2.0', **message}), flush=True)
for line in sys.stdin:
    message=json.loads(line)
    method=message.get('method')
    if method=='initialize':
        send(id=message['id'], result={'protocolVersion':1,'agentCapabilities':{}})
    elif method=='session/new':
        send(id=message['id'], result={'sessionId':'s'})
    elif method=='session/prompt':
        send(id='create', method='terminal/create', params={'sessionId':'s',
            'command':sys.executable,'args':['-c',"print('x' * (4 * 1024 * 1024))"]})
    elif message.get('id')=='create':
        terminal=message['result']['terminalId']
        send(id='wait', method='terminal/wait_for_exit',params={'sessionId':'s','terminalId':terminal})
    elif message.get('id')=='wait':
        send(id='output', method='terminal/output',params={'sessionId':'s','terminalId':terminal})
        pathlib.Path('not-reading').touch()
        time.sleep(20)
""")
            proc = subprocess.Popen(
                [
                    sys.executable,
                    str(Path(probe.__file__).resolve()),
                    "--command",
                    sys.executable,
                    "--args",
                    str(agent),
                    "--cwd",
                    directory,
                    "--out",
                    str(root / "frames.jsonl"),
                    "--report",
                    str(root / "report.json"),
                    "--no-try-set",
                    "--prompt",
                    "large output",
                    "--timeout",
                    "1",
                    "--preamble-timeout",
                    "0",
                    "--terminal-mode",
                    "strict",
                    "--quiet",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            try:
                stdout, stderr = proc.communicate(timeout=9)
            finally:
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.communicate(timeout=2)
            self.assertTrue(
                (root / "not-reading").exists(),
                "agent must reach the blocked-output case",
            )
            self.assertEqual(proc.returncode, 2, stdout + stderr)

    def test_close_unblocks_a_callback_writing_to_a_nonreading_agent(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(20)"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            client = probe.AcpClient(proc, Path(directory) / "frames.jsonl", True)
            entered = threading.Event()

            def callback():
                entered.set()
                client.send(
                    {
                        "id": "large-response",
                        "result": {"output": "x" * (4 * 1024 * 1024)},
                    }
                )

            future = client._server_request_executor.submit(callback)
            closer = threading.Thread(target=client.close, daemon=True)
            try:
                self.assertTrue(entered.wait(timeout=1))
                time.sleep(0.05)
                self.assertFalse(
                    future.done(), "response must exceed the unread pipe capacity"
                )
                closer.start()
                closer.join(timeout=7)
                stuck = closer.is_alive()
            finally:
                if proc.poll() is None:
                    proc.kill()
                closer.join(timeout=3)
                future.exception(timeout=2)
            self.assertFalse(stuck, "close blocked behind a callback's pipe write")

    def test_callbacks_are_serviced_without_an_active_request(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            program = """
import json,sys
json.loads(sys.stdin.readline())
def call(method,params):
    print(json.dumps({'jsonrpc':'2.0','id':method,'method':method,'params':params}), flush=True)
    return json.loads(sys.stdin.readline())
reply=call('session/request_permission',{'options':[{'optionId':'yes','kind':'allow_once'}]})
assert reply['result']['outcome']['optionId']=='yes', reply
reply=call('fs/write_text_file',{'path':'callback.txt','content':'one\\ntwo\\n'})
assert reply['result']=={}, reply
reply=call('fs/read_text_file',{'path':'callback.txt','line':2,'limit':1})
assert reply['result']=={'content':'two'}, reply
"""
            proc = subprocess.Popen(
                [sys.executable, "-c", program],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            client = probe.AcpClient(proc, root / "frames.jsonl", True)
            try:
                client.server_request_handler = probe._server_request_handler(
                    client, True, session_cwd=root
                )
                client.send({"method": "start"})
                self.assertEqual(proc.wait(timeout=2), 0)
            finally:
                client.close()

    def test_close_finishes_callback_workers_before_closing_log(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = subprocess.Popen(
                [sys.executable, "-c", "import sys; sys.stdin.read()"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            client = probe.AcpClient(proc, Path(directory) / "frames.jsonl", True)
            entered, release = threading.Event(), threading.Event()
            observations = []

            def callback():
                entered.set()
                release.wait(timeout=3)
                observations.append(client._log_file.closed)

            future = client._server_request_executor.submit(callback)
            closer = threading.Thread(target=client.close)
            try:
                self.assertTrue(entered.wait(timeout=1))
                closer.start()
                proc.wait(timeout=2)
                closer.join(timeout=0.1)
                self.assertTrue(
                    closer.is_alive(), "close must wait for active callbacks"
                )
            finally:
                release.set()
                closer.join(timeout=3)
                future.result(timeout=1)
            self.assertFalse(closer.is_alive())
            self.assertEqual(observations, [False])


if __name__ == "__main__":
    unittest.main()
