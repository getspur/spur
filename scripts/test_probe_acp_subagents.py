#!/usr/bin/env python3
import json
import io
import os
import subprocess
import tempfile
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


class FakeAcpProc:
    def __init__(self):
        self.stdin = io.StringIO()
        self.stdout = io.StringIO()
        self.stderr = io.StringIO()

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


class AcpSubagentProbeTests(unittest.TestCase):
    def test_codex_command_defaults_to_codex_acp_package(self):
        args = Namespace(
            agent="codex",
            codex_package=probe.DEFAULT_CODEX_PACKAGE,
            codex_config=[],
        )

        self.assertEqual(
            probe.agent_command(args),
            ["npx", "--yes", "@agentclientprotocol/codex-acp@1.1.2"],
        )

    def test_codex_cli_default_uses_agentclientprotocol_adapter_1_1_2(self):
        self.assertEqual(
            probe.DEFAULT_CODEX_PACKAGE,
            "@agentclientprotocol/codex-acp@1.1.2",
        )

    def test_codex_mjs_probe_default_uses_agentclientprotocol_adapter_1_1_2(self):
        source = Path(__file__).with_name("probe-codex-acp.mjs").read_text()

        self.assertIn(
            'const CODEX_ACP_PACKAGE = "@agentclientprotocol/codex-acp@1.1.2";',
            source,
        )

    def test_codex_mjs_reports_versions_before_spawning_adapter(self):
        source = Path(__file__).with_name("probe-codex-acp.mjs").read_text()

        version_report = source.index(
            "const resolvedVersions = reportResolvedVersions(probeEnv);"
        )
        adapter_spawn = source.index('const child = spawn("npx"')

        self.assertLess(version_report, adapter_spawn)

    def test_codex_version_script_executes_codex_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            node_modules = root / "node_modules"
            bin_dir = node_modules / ".bin"
            adapter_dir = node_modules / "@agentclientprotocol" / "codex-acp"
            codex_package_dir = node_modules / "@openai" / "codex"
            bin_dir.mkdir(parents=True)
            adapter_dir.mkdir(parents=True)
            codex_package_dir.mkdir(parents=True)
            (bin_dir / "codex-acp").write_text("")
            (adapter_dir / "package.json").write_text(
                json.dumps(
                    {"name": "@agentclientprotocol/codex-acp", "version": "1.1.2"}
                )
            )
            (codex_package_dir / "package.json").write_text(
                json.dumps({"name": "@openai/codex", "version": "0.144.1"})
            )
            bundled_codex = bin_dir / "codex"
            bundled_codex.write_text("#!/bin/sh\necho 'codex-cli 0.144.1'\n")
            bundled_codex.chmod(0o755)
            custom_codex = root / "custom-codex"
            custom_codex.write_text("#!/bin/sh\necho 'codex-cli 9.9.9'\n")
            custom_codex.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{bin_dir}{os.pathsep}{env['PATH']}"
            env["CODEX_PATH"] = str(custom_codex)

            completed = subprocess.run(
                ["node", "-e", probe.CODEX_VERSION_NODE_SCRIPT],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertEqual(payload["codexPackageVersion"], "0.144.1")
        self.assertEqual(payload["codexCliOutput"], "codex-cli 9.9.9")
        self.assertEqual(payload["codexCliPath"], str(custom_codex))

    def test_codex_profile_normalizes_relative_codex_path_once(self):
        self.assertTrue(
            hasattr(probe, "normalized_codex_environment"),
            "version discovery and adapter launch need one normalized environment",
        )
        with tempfile.TemporaryDirectory() as tmp:
            cwd = Path(tmp)
            env = probe.normalized_codex_environment(
                {
                    "CODEX_PATH": "bin/custom-codex",
                    "DEFAULT_AUTH_REQUEST": '{"api_key":"secret"}',
                    "CODEX_API_KEY": "codex-secret",
                    "OPENAI_API_KEY": "openai-secret",
                    "KEEP": "value",
                },
                cwd=cwd,
            )

        self.assertEqual(
            env["CODEX_PATH"],
            str((cwd / "bin" / "custom-codex").resolve()),
        )
        self.assertEqual(env["KEEP"], "value")
        self.assertNotIn("DEFAULT_AUTH_REQUEST", env)
        self.assertNotIn("CODEX_API_KEY", env)
        self.assertNotIn("OPENAI_API_KEY", env)

    def test_codex_mjs_versions_and_launch_share_codex_path_environment(self):
        source = Path(__file__).with_name("probe-codex-acp.mjs").read_text()

        self.assertIn("process.env.CODEX_PATH ||", source)
        self.assertIn(
            "const probeEnv = normalizedCodexEnvironment(process.env);", source
        )
        self.assertIn("reportResolvedVersions(probeEnv)", source)
        self.assertIn("env: probeEnv", source)
        self.assertIn('shell: process.platform === "win32"', source)
        self.assertIn(
            'shell: process.platform === "win32"',
            probe.CODEX_VERSION_NODE_SCRIPT,
        )

    def test_codex_mjs_general_probe_preserves_auth_environment(self):
        source = Path(__file__).with_name("probe-codex-acp.mjs").read_text()

        self.assertNotIn("delete env.DEFAULT_AUTH_REQUEST;", source)
        self.assertNotIn("delete env.CODEX_API_KEY;", source)
        self.assertNotIn("delete env.OPENAI_API_KEY;", source)

    def test_codex_mjs_probe_has_valid_node_syntax(self):
        script = Path(__file__).with_name("probe-codex-acp.mjs")

        completed = subprocess.run(
            ["node", "--check", str(script)],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_profile_fixture_keeps_tokens_in_their_only_authored_sources(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = probe.create_codex_profile_fixture(
                Path(tmp),
                primary_token="PRIMARY-ONLY-123",
                child_token="CHILD-ONLY-456",
            )

            primary_source = fixture.primary_profile_path.read_text()
            child_source = fixture.child_role_path.read_text()

            self.assertIn("PRIMARY-ONLY-123", fixture.primary_body)
            self.assertIn("PRIMARY-ONLY-123", primary_source)
            self.assertNotIn("PRIMARY-ONLY-123", child_source)
            self.assertIn("CHILD-ONLY-456", child_source)
            self.assertNotIn("CHILD-ONLY-456", primary_source)
            self.assertNotIn("PRIMARY-ONLY-123", fixture.positive_prompt)
            self.assertNotIn("CHILD-ONLY-456", fixture.positive_prompt)
            self.assertTrue(
                hasattr(probe, "find_token_sources"),
                "the live probe must verify authored token-source exclusivity",
            )
            self.assertEqual(
                probe.find_token_sources(Path(tmp), "PRIMARY-ONLY-123"),
                (fixture.primary_profile_path,),
            )
            self.assertEqual(
                probe.find_token_sources(Path(tmp), "CHILD-ONLY-456"),
                (fixture.child_role_path,),
            )

    def test_profile_fixture_protects_token_bearing_tree_modes(self):
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / "positive-workspace"
            probe.initialize_codex_probe_workspace(workspace)
            fixture = probe.create_codex_profile_fixture(
                workspace,
                primary_token="PRIMARY-ONLY-123",
                child_token="CHILD-ONLY-456",
            )

            private_directories = (
                workspace,
                workspace / ".spur",
                workspace / ".spur" / "agents",
                workspace / ".codex",
                workspace / ".codex" / "agents",
            )
            for directory in private_directories:
                with self.subTest(directory=directory):
                    self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            for path in (fixture.primary_profile_path, fixture.child_role_path):
                with self.subTest(path=path):
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_profile_probe_raw_stdout_and_stderr_artifacts_are_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            artifact_dir = Path(tmp) / "artifacts"
            artifact_dir.mkdir(mode=0o755)
            raw_log = artifact_dir / "positive.jsonl"
            client = probe.AcpClient(FakeAcpProc(), raw_log, quiet=True)
            client.close()

            stdout_path = artifact_dir / "positive.stdout"
            stderr_path = artifact_dir / "positive.stderr"
            completed = probe.run_captured_probe(
                command=[
                    sys.executable,
                    "-c",
                    "import sys; print('out'); print('err', file=sys.stderr)",
                ],
                cwd=Path(tmp),
                env=os.environ.copy(),
                stdout_path=stdout_path,
                stderr_path=stderr_path,
            )

            self.assertEqual(completed.returncode, 0)
            self.assertEqual(artifact_dir.stat().st_mode & 0o777, 0o700)
            for path in (raw_log, stdout_path, stderr_path):
                with self.subTest(path=path):
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_private_artifact_writers_refuse_symlink_targets(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for writer_name in ("open_private_text", "write_private_text"):
                with self.subTest(writer=writer_name):
                    victim = root / f"{writer_name}-victim"
                    victim.write_text("do-not-truncate")
                    link = root / f"{writer_name}-link"
                    link.symlink_to(victim)
                    raised = False
                    try:
                        if writer_name == "open_private_text":
                            with probe.open_private_text(link):
                                pass
                        else:
                            probe.write_private_text(link, "replacement")
                    except OSError:
                        raised = True

                    self.assertTrue(raised, "symlink target must be rejected")
                    self.assertEqual(victim.read_text(), "do-not-truncate")

    def test_profile_control_workspace_is_an_isolated_git_root(self):
        self.assertTrue(
            hasattr(probe, "initialize_codex_probe_workspace"),
            "controls must not inherit role files from the containing repository",
        )
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / "control"

            probe.initialize_codex_probe_workspace(workspace)
            completed = subprocess.run(
                ["git", "rev-parse", "--show-toplevel"],
                cwd=workspace,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(Path(completed.stdout.strip()).resolve(), workspace.resolve())

    def test_profile_control_workspace_rejects_existing_profile_sources(self):
        with tempfile.TemporaryDirectory() as tmp:
            workspace = Path(tmp) / "control"
            role_dir = workspace / ".codex" / "agents"
            role_dir.mkdir(parents=True)
            existing_role = role_dir / "stale.toml"
            existing_role.write_text('name = "stale"\n')

            with self.assertRaisesRegex(
                RuntimeError,
                "already contains profile or role sources",
            ):
                probe.initialize_codex_probe_workspace(workspace)
            self.assertEqual(existing_role.read_text(), 'name = "stale"\n')

    def test_exact_version_label_requires_codex_cli_0_144_1(self):
        self.assertEqual(
            probe.codex_evidence_label("1.1.2", "0.144.1"),
            "codex-0.144.1",
        )
        self.assertEqual(
            probe.codex_evidence_label("1.1.2", "0.145.0"),
            "codex-actual-0.145.0",
        )

    def test_profile_probe_forces_stable_multi_agent_v1_in_both_controls(self):
        positive = probe.codex_profile_probe_config("selected profile body")
        negative = probe.codex_profile_probe_config(None)

        self.assertEqual(
            positive,
            {
                "developer_instructions": "selected profile body",
                "features": {"multi_agent": True, "multi_agent_v2": False},
                "model": "gpt-5.5",
            },
        )
        self.assertEqual(
            negative,
            {
                "features": {"multi_agent": True, "multi_agent_v2": False},
                "model": "gpt-5.5",
            },
        )

    def test_profile_probe_verdict_requires_primary_child_and_negative_control(self):
        failures = probe.codex_profile_probe_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            primary_response="primary response PRIMARY-123",
            raw_child_results=["child result CHILD-456"],
            positive_stderr="",
            negative_response="NO_PROFILE_ACTIVE",
            negative_raw_child_results=[],
            negative_stderr="",
        )

        self.assertEqual(failures, [])

    def test_profile_probe_activity_accepts_only_spawn_and_wait_flow(self):
        activity_type = getattr(probe, "CodexRolloutActivity", None)
        if activity_type is None:
            self.fail("profile verdict needs controlled rollout activity evidence")
        positive_evidence = Namespace(
            tool_call_titles=("spawnAgent", "wait"),
            server_request_methods=(),
            spawn_agent_raw_inputs=(json.dumps({"status": "inProgress"}),),
        )
        negative_evidence = Namespace(
            tool_call_titles=(),
            server_request_methods=(),
            spawn_agent_raw_inputs=(),
        )
        positive_rollout = activity_type(
            function_calls=("spawn_agent", "wait_agent"),
            spawn_agent_arguments=(
                json.dumps(
                    {
                        "agent_type": probe.CODEX_CHILD_ROLE_NAME,
                        "message": "return the native role verification token",
                    }
                ),
            ),
        )
        negative_rollout = activity_type(function_calls=(), spawn_agent_arguments=())

        failures = probe.codex_profile_activity_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            positive_evidence=positive_evidence,
            negative_evidence=negative_evidence,
            positive_rollout=positive_rollout,
            negative_rollout=negative_rollout,
        )

        self.assertEqual(failures, [])

    def test_profile_probe_verdict_rejects_unexpected_tool_and_function_activity(self):
        activity_type = getattr(probe, "CodexRolloutActivity", None)
        if activity_type is None:
            self.fail("profile verdict needs controlled rollout activity evidence")
        positive_evidence = Namespace(
            tool_call_titles=("spawnAgent", "readTextFile", "wait"),
            server_request_methods=("fs/read_text_file",),
            spawn_agent_raw_inputs=(json.dumps({"status": "inProgress"}),),
        )
        negative_evidence = Namespace(
            tool_call_titles=("terminal",),
            server_request_methods=("session/request_permission",),
            spawn_agent_raw_inputs=(),
        )
        positive_rollout = activity_type(
            function_calls=("spawn_agent", "exec_command", "wait_agent"),
            spawn_agent_arguments=(
                json.dumps(
                    {
                        "agent_type": probe.CODEX_CHILD_ROLE_NAME,
                        "message": "return the native role verification token",
                    }
                ),
            ),
        )
        negative_rollout = activity_type(
            function_calls=("read_file",),
            spawn_agent_arguments=(),
        )

        failures = probe.codex_profile_activity_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            positive_evidence=positive_evidence,
            negative_evidence=negative_evidence,
            positive_rollout=positive_rollout,
            negative_rollout=negative_rollout,
        )

        self.assertTrue(any("unexpected ACP tool" in failure for failure in failures))
        self.assertTrue(
            any("unexpected server request" in failure for failure in failures)
        )
        self.assertTrue(
            any("unexpected rollout function" in failure for failure in failures)
        )

    def test_profile_probe_verdict_rejects_canary_in_spawn_arguments(self):
        activity_type = getattr(probe, "CodexRolloutActivity", None)
        if activity_type is None:
            self.fail("profile verdict needs controlled rollout activity evidence")
        negative_evidence = Namespace(
            tool_call_titles=(),
            server_request_methods=(),
            spawn_agent_raw_inputs=(),
        )
        negative_rollout = activity_type(function_calls=(), spawn_agent_arguments=())

        for leaked_token in ("PRIMARY-123", "CHILD-456"):
            with self.subTest(leaked_token=leaked_token):
                positive_evidence = Namespace(
                    tool_call_titles=("spawnAgent", "wait"),
                    server_request_methods=(),
                    spawn_agent_raw_inputs=(
                        json.dumps({"message": f"return {leaked_token}"}),
                    ),
                )
                positive_rollout = activity_type(
                    function_calls=("spawn_agent", "wait_agent"),
                    spawn_agent_arguments=(
                        json.dumps(
                            {
                                "agent_type": probe.CODEX_CHILD_ROLE_NAME,
                                "message": f"return {leaked_token}",
                            }
                        ),
                    ),
                )

                failures = probe.codex_profile_activity_failures(
                    primary_token="PRIMARY-123",
                    child_token="CHILD-456",
                    positive_evidence=positive_evidence,
                    negative_evidence=negative_evidence,
                    positive_rollout=positive_rollout,
                    negative_rollout=negative_rollout,
                )

                self.assertTrue(
                    any(
                        "spawn_agent arguments contained a probe token" in item
                        for item in failures
                    )
                )

    def test_profile_probe_activity_rejects_missing_wait(self):
        activity_type = getattr(probe, "CodexRolloutActivity", None)
        if activity_type is None:
            self.fail("profile verdict needs controlled rollout activity evidence")
        positive_evidence = Namespace(
            tool_call_titles=("spawnAgent",),
            server_request_methods=(),
            spawn_agent_raw_inputs=(json.dumps({"status": "inProgress"}),),
        )
        empty_evidence = Namespace(
            tool_call_titles=(),
            server_request_methods=(),
            spawn_agent_raw_inputs=(),
        )

        failures = probe.codex_profile_activity_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            positive_evidence=positive_evidence,
            negative_evidence=empty_evidence,
            positive_rollout=activity_type(
                function_calls=("spawn_agent",),
                spawn_agent_arguments=(
                    json.dumps({"agent_type": probe.CODEX_CHILD_ROLE_NAME}),
                ),
            ),
            negative_rollout=activity_type(
                function_calls=(),
                spawn_agent_arguments=(),
            ),
        )

        self.assertTrue(
            any("exact spawn_agent + wait flow" in item for item in failures)
        )

    def test_profile_probe_verdict_rejects_malformed_role_warning(self):
        failures = probe.codex_profile_probe_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            primary_response="primary response PRIMARY-123",
            raw_child_results=["child result CHILD-456"],
            positive_stderr="Ignoring malformed agent role definition: bad-role",
            negative_response="NO_PROFILE_ACTIVE",
            negative_raw_child_results=[],
            negative_stderr="",
        )

        self.assertIn("positive stderr contained malformed-role warning", failures)

    def test_profile_probe_rejects_malformed_warning_in_app_server_log(self):
        self.assertTrue(
            hasattr(probe, "codex_profile_warning_failures"),
            "profile probe must scan Codex app-server logs as well as adapter stderr",
        )

        failures = probe.codex_profile_warning_failures(
            run_name="positive",
            adapter_stderr="",
            app_server_log=(
                "2026-07-10T00:00:00Z [ERR] "
                "Ignoring malformed agent role definition: bad-role"
            ),
        )

        self.assertIn(
            "positive app-server log contained malformed-role warning",
            failures,
        )

    def test_profile_probe_rejects_missing_app_server_log(self):
        failures = probe.codex_profile_warning_failures(
            run_name="positive",
            adapter_stderr="",
            app_server_log=None,
        )

        self.assertIn("positive app-server log was not captured", failures)

        empty_failures = probe.codex_profile_warning_failures(
            run_name="positive",
            adapter_stderr="",
            app_server_log="",
        )
        self.assertIn("positive app-server log was empty", empty_failures)

    def test_prepare_app_server_log_clears_stale_warning(self):
        self.assertTrue(
            hasattr(probe, "prepare_app_server_log"),
            "each control needs an isolated, cleared APP_SERVER_LOGS target",
        )

        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp) / "positive-app-server-logs"
            log_dir.mkdir()
            log_path = log_dir / "app-server.log"
            log_path.write_text(probe.MALFORMED_ROLE_WARNING)

            prepared = probe.prepare_app_server_log(log_dir)

            self.assertEqual(prepared, log_path)
            self.assertEqual(log_path.read_text(), "")
            self.assertEqual(log_dir.stat().st_mode & 0o777, 0o700)
            self.assertEqual(log_path.stat().st_mode & 0o777, 0o600)

    def test_profile_probe_verdict_rejects_profile_token_in_negative_control(self):
        failures = probe.codex_profile_probe_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            primary_response="primary response PRIMARY-123",
            raw_child_results=["child result CHILD-456"],
            positive_stderr="",
            negative_response="unexpected PRIMARY-123",
            negative_raw_child_results=[],
            negative_stderr="",
        )

        self.assertIn("no-profile control repeated the primary token", failures)

    def test_profile_probe_verdict_requires_exact_negative_control_response(self):
        failures = probe.codex_profile_probe_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            primary_response="primary response PRIMARY-123",
            raw_child_results=["child result CHILD-456"],
            positive_stderr="",
            negative_response="NO_PROFILE_ACTIVE but inherited unrelated instructions",
            negative_raw_child_results=[],
            negative_stderr="",
        )

        self.assertIn(
            "no-profile control did not return exactly NO_PROFILE_ACTIVE",
            failures,
        )

    def test_profile_probe_verdict_rejects_negative_spawn_without_result(self):
        records = [
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "negative-spawn",
                            "title": "spawnAgent",
                            "rawInput": {"status": "inProgress"},
                        }
                    },
                },
            },
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {
                                "type": "text",
                                "text": "NO_PROFILE_ACTIVE",
                            },
                        }
                    },
                },
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "negative.jsonl"
            path.write_text("\n".join(json.dumps(record) for record in records))
            evidence = probe.load_codex_probe_evidence(path)

        self.assertTrue(
            hasattr(evidence, "spawn_agent_call_ids"),
            "negative evidence must retain spawn calls even without a wait result",
        )
        failures = probe.codex_profile_probe_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            primary_response="primary response PRIMARY-123",
            raw_child_results=["child result CHILD-456"],
            positive_stderr="",
            negative_response=evidence.primary_response,
            negative_raw_child_results=evidence.raw_child_results,
            negative_stderr="",
            negative_spawn_agent_call_ids=evidence.spawn_agent_call_ids,
        )

        self.assertIn("no-profile control unexpectedly called spawn_agent", failures)

    def test_load_codex_probe_evidence_reads_wait_agent_state_message(self):
        self.assertTrue(
            hasattr(probe, "load_codex_role_binding"),
            "rollout evidence must bind the requested role to a child thread",
        )
        parent_thread_id = "parent-thread"
        child_thread_id = "child-thread"
        workspace = Path("/tmp/codex-profile-positive-workspace")
        rollout_records = [
            {
                "type": "session_meta",
                "payload": {
                    "id": parent_thread_id,
                    "cwd": str(workspace),
                    "parent_thread_id": None,
                    "agent_role": None,
                },
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "spawn_agent",
                    "arguments": json.dumps(
                        {
                            "agent_type": probe.CODEX_CHILD_ROLE_NAME,
                            "message": "return the token",
                        }
                    ),
                    "call_id": "spawn-call",
                },
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "spawn-call",
                    "output": json.dumps(
                        {"agent_id": child_thread_id, "nickname": "Peirce"}
                    ),
                },
            },
        ]
        child_rollout_records = [
            {
                "type": "session_meta",
                "payload": {
                    "id": child_thread_id,
                    "cwd": str(workspace),
                    "parent_thread_id": parent_thread_id,
                    "agent_role": probe.CODEX_CHILD_ROLE_NAME,
                },
            }
        ]
        acp_records = [
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "spawn-1",
                            "title": "spawnAgent",
                            # The adapter's actual ACP rawInput omits agent_type.
                            "rawInput": {"status": "inProgress"},
                        }
                    },
                },
            },
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": "wait-1",
                            "title": "wait",
                            "rawInput": {
                                "agentsStates": {
                                    "unrelated-thread": {
                                        "status": "completed",
                                        "message": "UNRELATED-CHILD-RESULT",
                                    },
                                    "child-thread": {
                                        "status": "completed",
                                        "message": "RAW-CHILD-RESULT",
                                    },
                                }
                            },
                        }
                    },
                },
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parent_rollout = root / "parent-rollout.jsonl"
            parent_rollout.write_text(
                "\n".join(json.dumps(record) for record in rollout_records)
            )
            child_rollout = root / "child-rollout.jsonl"
            child_rollout.write_text(
                "\n".join(json.dumps(record) for record in child_rollout_records)
            )
            path = root / "probe.jsonl"
            path.write_text("\n".join(json.dumps(record) for record in acp_records))

            binding = probe.load_codex_role_binding(
                [parent_rollout, child_rollout],
                workspace,
            )
            evidence = probe.load_codex_probe_evidence(
                path,
                child_thread_id=binding.child_thread_id,
            )

        self.assertEqual(binding.parent_thread_id, parent_thread_id)
        self.assertEqual(binding.requested_agent_type, probe.CODEX_CHILD_ROLE_NAME)
        self.assertEqual(binding.child_thread_id, child_thread_id)
        self.assertEqual(binding.child_parent_thread_id, parent_thread_id)
        self.assertEqual(binding.child_agent_role, probe.CODEX_CHILD_ROLE_NAME)
        self.assertEqual(evidence.raw_child_results, ("RAW-CHILD-RESULT",))

    def test_load_codex_probe_evidence_retains_restricted_activity_surfaces(self):
        records = [
            {
                "dir": "recv",
                "msg": {
                    "jsonrpc": "2.0",
                    "id": "permission-1",
                    "method": "session/request_permission",
                    "params": {"options": [{"optionId": "allow_once"}]},
                },
            },
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "spawn-1",
                            "title": "spawnAgent",
                            "rawInput": {"message": "return the token"},
                        }
                    },
                },
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "probe.jsonl"
            path.write_text("\n".join(json.dumps(record) for record in records))

            evidence = probe.load_codex_probe_evidence(path)

        self.assertEqual(
            evidence.server_request_methods,
            ("session/request_permission",),
        )
        self.assertEqual(evidence.tool_call_titles, ("spawnAgent",))
        self.assertEqual(
            evidence.spawn_agent_raw_inputs,
            (json.dumps({"message": "return the token"}, sort_keys=True),),
        )

    def test_load_codex_rollout_activity_filters_workspace_and_records_arguments(self):
        workspace = Path("/tmp/codex-profile-positive-workspace")
        matching_records = [
            {
                "type": "session_meta",
                "payload": {"id": "parent", "cwd": str(workspace)},
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "spawn_agent",
                    "arguments": json.dumps(
                        {
                            "agent_type": probe.CODEX_CHILD_ROLE_NAME,
                            "message": "return the verification token",
                        }
                    ),
                },
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "wait_agent",
                    "arguments": json.dumps({"ids": ["child"]}),
                },
            },
        ]
        unrelated_records = [
            {
                "type": "session_meta",
                "payload": {"id": "other", "cwd": "/tmp/unrelated"},
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": json.dumps({"cmd": "cat role.toml"}),
                },
            },
        ]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            matching = root / "matching.jsonl"
            matching.write_text(
                "\n".join(json.dumps(item) for item in matching_records)
            )
            unrelated = root / "unrelated.jsonl"
            unrelated.write_text(
                "\n".join(json.dumps(item) for item in unrelated_records)
            )

            activity = probe.load_codex_rollout_activity(
                [matching, unrelated],
                workspace,
            )

        self.assertEqual(activity.function_calls, ("spawn_agent", "wait_agent"))
        self.assertEqual(len(activity.spawn_agent_arguments), 1)
        self.assertNotIn("exec_command", activity.function_calls)

    def test_rollout_activity_rejects_non_function_child_and_negative_actions(self):
        workspace = Path("/tmp/codex-profile-action-workspace")

        def rollout(session_id, parent_id, items):
            return [
                {
                    "type": "session_meta",
                    "payload": {
                        "id": session_id,
                        "cwd": str(workspace),
                        "parent_thread_id": parent_id,
                    },
                },
                *[{"type": "response_item", "payload": item} for item in items],
            ]

        parent_items = [
            {
                "type": "function_call",
                "name": "spawn_agent",
                "arguments": json.dumps(
                    {"agent_type": probe.CODEX_CHILD_ROLE_NAME, "message": "verify"}
                ),
            },
            {
                "type": "function_call",
                "name": "wait_agent",
                "arguments": json.dumps({"ids": ["child"]}),
            },
        ]
        child_items = [
            {"type": "custom_tool_call", "name": "read_secret", "input": "role"}
        ]
        negative_items = [{"type": "web_search_call", "query": "verification token"}]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            parent = root / "parent.jsonl"
            parent.write_text(
                "\n".join(
                    json.dumps(item) for item in rollout("parent", None, parent_items)
                )
            )
            child = root / "child.jsonl"
            child.write_text(
                "\n".join(
                    json.dumps(item) for item in rollout("child", "parent", child_items)
                )
            )
            negative = root / "negative.jsonl"
            negative.write_text(
                "\n".join(
                    json.dumps(item)
                    for item in rollout("negative", None, negative_items)
                )
            )

            positive_activity = probe.load_codex_rollout_activity(
                [parent, child],
                workspace,
            )
            negative_activity = probe.load_codex_rollout_activity(
                [negative],
                workspace,
            )

        failures = probe.codex_profile_activity_failures(
            primary_token="PRIMARY-123",
            child_token="CHILD-456",
            positive_evidence=Namespace(
                tool_call_titles=("spawnAgent", "wait"),
                server_request_methods=(),
                spawn_agent_raw_inputs=(),
            ),
            negative_evidence=Namespace(
                tool_call_titles=(),
                server_request_methods=(),
                spawn_agent_raw_inputs=(),
            ),
            positive_rollout=positive_activity,
            negative_rollout=negative_activity,
        )

        self.assertIn("custom_tool_call", positive_activity.response_item_types)
        self.assertIn("web_search_call", negative_activity.response_item_types)
        self.assertTrue(
            any(
                "unexpected rollout response item custom_tool_call" in item
                for item in failures
            )
        )
        self.assertTrue(
            any(
                "unexpected rollout response item web_search_call" in item
                for item in failures
            )
        )

    def test_rollout_activity_rejects_malformed_and_unreadable_evidence(self):
        workspace = Path("/tmp/codex-profile-malformed-workspace")
        session_meta = {
            "type": "session_meta",
            "payload": {"id": "parent", "cwd": str(workspace)},
        }
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            malformed = root / "malformed.jsonl"
            malformed.write_text(json.dumps(session_meta) + "\n{not-json\n")
            missing = root / "missing.jsonl"

            malformed_activity = probe.load_codex_rollout_activity(
                [malformed],
                workspace,
            )
            unreadable_activity = probe.load_codex_rollout_activity(
                [missing],
                workspace,
            )

        self.assertTrue(
            any("invalid JSON" in error for error in malformed_activity.evidence_errors)
        )
        self.assertTrue(
            any("read failed" in error for error in unreadable_activity.evidence_errors)
        )

    def test_role_binding_verdict_rejects_wrong_requested_and_loaded_roles(self):
        self.assertTrue(
            hasattr(probe, "codex_role_binding_failures"),
            "profile verdict must enforce exact parent and child roles",
        )
        binding = probe.CodexRoleBinding(
            parent_thread_id="parent-thread",
            requested_agent_type="wrong-role",
            spawn_call_id="spawn-call",
            child_thread_id="child-thread",
            child_parent_thread_id="parent-thread",
            child_agent_role="wrong-role",
        )

        failures = probe.codex_role_binding_failures(binding)

        self.assertIn(
            "parent rollout spawn_agent did not request exact role "
            f"{probe.CODEX_CHILD_ROLE_NAME}",
            failures,
        )
        self.assertIn(
            "child session metadata did not bind exact role "
            f"{probe.CODEX_CHILD_ROLE_NAME}",
            failures,
        )

    def test_role_binding_verdict_rejects_missing_child_session_metadata(self):
        self.assertTrue(
            hasattr(probe, "codex_role_binding_failures"),
            "profile verdict must reject missing child session metadata",
        )
        binding = probe.CodexRoleBinding(
            parent_thread_id="parent-thread",
            requested_agent_type=probe.CODEX_CHILD_ROLE_NAME,
            spawn_call_id="spawn-call",
            child_thread_id="child-thread",
            child_parent_thread_id=None,
            child_agent_role=None,
        )

        failures = probe.codex_role_binding_failures(binding)

        self.assertIn(
            "child session metadata did not bind exact role "
            f"{probe.CODEX_CHILD_ROLE_NAME}",
            failures,
        )
        self.assertIn(
            "child session metadata did not bind to the spawning parent thread",
            failures,
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

    def test_profile_probe_client_capabilities_omit_filesystem_and_terminal(self):
        capabilities = getattr(probe, "profile_probe_client_capabilities", None)
        if capabilities is None:
            self.fail("profile mode needs a restricted capability set")

        caps = capabilities()

        self.assertEqual(caps, {})

    def test_profile_probe_handlers_deny_filesystem_terminal_and_permissions(self):
        handler_type = getattr(probe, "ProfileProbeRequestHandlers", None)
        if handler_type is None:
            self.fail("profile mode needs deny-by-default request handlers")

        with tempfile.TemporaryDirectory() as tmp:
            handler = handler_type(Path(tmp))
            requests = (
                {"method": "fs/read_text_file", "params": {"path": "/etc/hosts"}},
                {"method": "terminal/create", "params": {"command": "true"}},
                {
                    "method": "session/request_permission",
                    "params": {"options": [{"optionId": "allow_once"}]},
                },
            )
            for request in requests:
                with self.subTest(method=request["method"]):
                    with self.assertRaisesRegex(
                        probe.ProbeRequestError,
                        "denied by restricted Codex profile probe",
                    ):
                        handler.handle(request)
            handler.close()

    def test_profile_inner_command_enables_restricted_mode(self):
        args = Namespace(
            codex_package=probe.DEFAULT_CODEX_PACKAGE,
            timeout=1,
            init_timeout=1,
            session_timeout=1,
            authenticate=False,
        )

        command = probe.codex_profile_inner_command(
            args,
            "profile prompt",
            Path("positive.jsonl"),
        )

        self.assertIn("--restricted-profile-probe", command)

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
