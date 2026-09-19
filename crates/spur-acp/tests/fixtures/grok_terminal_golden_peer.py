#!/usr/bin/env python3
"""Deterministic ACP peer: drive SPUR's real terminal callbacks, no model calls."""

from __future__ import annotations

import json
import os
import queue
import shlex
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path


class Peer:
    def __init__(self, corpus_path: Path, report_path: Path):
        self.corpus = json.loads(corpus_path.read_text())
        self.report_path = report_path
        self.messages: queue.Queue = queue.Queue()
        self.next_id = 0
        self.cwd = report_path.parent
        self.report = {
            "schema": "spur.grok-terminal-golden-result",
            "version": 1,
            "cases": [],
            "follow_up": False,
            "lifecycle": None,
        }
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in sys.stdin:
            self.messages.put(json.loads(line))
        self.messages.put(None)

    def send(self, **message):
        print(json.dumps({"jsonrpc": "2.0", **message}), flush=True)

    def begin(self, method, **params):
        self.next_id += 1
        self.send(
            id=self.next_id, method=method, params={"sessionId": "golden", **params}
        )
        return self.next_id

    def response(self, request_id, timeout=5):
        response = self.messages.get(timeout=timeout)
        if response is None:
            raise RuntimeError("client closed during callback")
        if response.get("id") != request_id or "method" in response:
            raise RuntimeError(f"unexpected callback response: {response}")
        if "error" in response:
            raise RuntimeError(f"callback error: {response['error']}")
        return response["result"]

    def rpc(self, method, **params):
        return self.response(self.begin(method, **params))

    def persist(self):
        self.report_path.write_text(
            json.dumps(self.report, ensure_ascii=False, indent=2) + "\n"
        )

    def terminal(self, command, args, cwd, env):
        terminal = self.rpc(
            "terminal/create",
            command=command,
            args=args,
            cwd=str(cwd),
            env=[{"name": k, "value": v} for k, v in env.items()],
        )["terminalId"]
        try:
            status = self.rpc("terminal/wait_for_exit", terminalId=terminal)
            output = self.rpc("terminal/output", terminalId=terminal)
            if output.get("truncated"):
                raise AssertionError("golden output was unexpectedly truncated")
            if output.get("exitStatus") != status:
                raise AssertionError(f"inconsistent exit status: {output} vs {status}")
            return {"output": output["output"], "exit_code": status.get("exitCode")}
        finally:
            self.rpc("terminal/release", terminalId=terminal)

    def evaluate(self):
        for runtime in self.corpus["required_runtimes"]:
            if not shutil.which(runtime):
                raise RuntimeError(f"required runtime missing: {runtime}")
        versions = {}
        for runtime in [
            "bash",
            *self.corpus["required_runtimes"],
            *self.corpus["optional_runtimes"],
        ]:
            path = shutil.which(runtime)
            if path:
                result = subprocess.run(
                    [path, "--version"], capture_output=True, text=True, timeout=5
                )
                versions[runtime] = {
                    "path": path,
                    "version": next(
                        (
                            line
                            for line in (result.stdout + result.stderr).splitlines()
                            if line.strip()
                        ),
                        "unknown",
                    ),
                }
            else:
                versions[runtime] = {"path": None, "version": None}
        self.report["runtimes"] = versions
        for case in self.corpus["cases"]:
            missing = [
                runtime for runtime in case["requires"] if not shutil.which(runtime)
            ]
            if missing:
                self.report["cases"].append(
                    {"id": case["id"], "status": "skipped", "missing": missing}
                )
                self.persist()
                continue
            directory = self.cwd / case["id"]
            directory.mkdir()
            for name, contents in case["files"].items():
                path = directory / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            env = {"BASH_ENV": "/dev/null", "ENV": "/dev/null", **case["env"]}
            canonical = case["canonical"]
            reference = subprocess.run(
                [canonical["command"], *canonical["args"]],
                cwd=directory,
                env={**os.environ, **env},
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                timeout=5,
            )
            expected = case["expected"]
            assert reference.returncode == expected["exit_code"], (
                case["id"],
                reference,
            )
            assert reference.stdout == expected["stdout"], (
                case["id"],
                reference.stdout,
            )
            if "stderr" in expected:
                assert reference.stderr == expected["stderr"], (
                    case["id"],
                    reference.stderr,
                )
            else:
                assert reference.stderr.endswith(expected["stderr_suffix"]), (
                    case["id"],
                    reference.stderr,
                )
            reference_result = {
                "output": reference.stdout + reference.stderr,
                "exit_code": reference.returncode,
            }
            actual = []
            for request in [canonical, *case["packed"]]:
                result = self.terminal(**request, cwd=directory, env=env)
                assert result == reference_result, (
                    case["id"],
                    request,
                    result,
                    reference_result,
                )
                actual.append(result)
            self.report["cases"].append(
                {"id": case["id"], "status": "pass", "runs": actual}
            )
            self.persist()

    def inherited_pipe(self):
        cases = []
        for packed in (False, True):
            cases.append(self.inherited_pipe_variant(packed))
            self.report["lifecycle"] = {
                "cases": cases,
                "verdict": "pass"
                if all(case["wait_returned_before_release"] for case in cases)
                else "fail",
            }
            self.persist()

    def inherited_pipe_variant(self, packed):
        command_form = "packed" if packed else "split"
        directory = self.cwd / command_form
        directory.mkdir()
        # The descendant exits even if the test is interrupted before release.
        child_script = (
            "import pathlib,time; pathlib.Path('child-ready').touch(); "
            "deadline=time.monotonic()+10\n"
            "while not pathlib.Path('release-child').exists() and time.monotonic()<deadline: time.sleep(0.01)\n"
        )
        parent_script = (
            "import os,pathlib,subprocess,sys; "
            "pathlib.Path('parent-pid').write_text(str(os.getpid())); "
            f"subprocess.Popen([sys.executable,'-c',{child_script!r}]); "
            "print('parent-finished',flush=True)"
        )
        script = "exec python3 -c " + shlex.quote(parent_script)
        terminal = self.rpc(
            "terminal/create",
            command="/bin/bash -c " + shlex.quote(script) if packed else "/bin/bash",
            args=[] if packed else ["-c", script],
            cwd=str(directory),
            env=[{"name": "BASH_ENV", "value": "/dev/null"}],
        )["terminalId"]
        try:
            deadline = time.monotonic() + 5
            exited = False
            while time.monotonic() < deadline:
                pid_path = directory / "parent-pid"
                if pid_path.exists() and (directory / "child-ready").exists():
                    pid = pid_path.read_text()
                    state = subprocess.run(
                        ["ps", "-o", "stat=", "-p", pid],
                        capture_output=True,
                        text=True,
                        timeout=1,
                    ).stdout.strip()
                    if not state or state.startswith("Z"):
                        exited = True
                        break
                time.sleep(0.01)
            assert exited, "command process did not exit before held-pipe observation"
            wait_id = self.begin("terminal/wait_for_exit", terminalId=terminal)
            try:
                status = self.response(wait_id, timeout=0.25)
                returned_before_release = True
            except queue.Empty:
                status = None
                returned_before_release = False
            finally:
                (directory / "release-child").touch()
            if status is None:
                status = self.response(wait_id)
            assert status.get("exitCode") == 0, status
            return {
                "command_form": command_form,
                "parent_exit_observed": exited,
                "wait_returned_before_release": returned_before_release,
                "observation_window_seconds": 0.25,
                "exit_code": status.get("exitCode"),
                "verdict": "pass" if returned_before_release else "fail",
            }
        finally:
            (directory / "release-child").touch()
            self.rpc("terminal/release", terminalId=terminal)

    def run(self):
        while (message := self.messages.get()) is not None:
            method = message.get("method")
            if method == "initialize":
                self.send(
                    id=message["id"],
                    result={"protocolVersion": 1, "agentCapabilities": {}},
                )
            elif method == "session/new":
                self.cwd = Path(message["params"]["cwd"])
                self.send(id=message["id"], result={"sessionId": "golden"})
            elif method == "session/prompt":
                try:
                    assert message["params"]["sessionId"] == "golden"
                    action = message["params"]["prompt"][0]["text"]
                    if action == "golden":
                        self.evaluate()
                    elif action == "inherited-pipe":
                        self.inherited_pipe()
                    elif action == "continue":
                        result = self.terminal(
                            "/bin/sh", ["-c", "printf continuation-ok"], self.cwd, {}
                        )
                        assert result == {
                            "output": "continuation-ok",
                            "exit_code": 0,
                        }, result
                        self.report["follow_up"] = True
                    else:
                        raise ValueError(action)
                    self.persist()
                    self.send(id=message["id"], result={"stopReason": "end_turn"})
                except Exception as error:
                    self.report["error"] = repr(error)
                    self.persist()
                    self.send(
                        id=message["id"], error={"code": -32603, "message": repr(error)}
                    )


if __name__ == "__main__":
    Peer(Path(sys.argv[1]), Path(sys.argv[2])).run()
