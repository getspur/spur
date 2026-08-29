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
from unittest import mock

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
            ["npx", "--yes", "@agentclientprotocol/codex-acp@1.7.0"],
        )

    def test_codex_cli_default_uses_agentclientprotocol_adapter_1_7_0(self):
        self.assertEqual(
            probe.DEFAULT_CODEX_PACKAGE,
            "@agentclientprotocol/codex-acp@1.7.0",
        )

    def test_codex_mjs_probe_default_uses_agentclientprotocol_adapter_1_7_0(self):
        source = Path(__file__).with_name("probe-codex-acp.mjs").read_text()

        self.assertIn(
            'const CODEX_ACP_PACKAGE = "@agentclientprotocol/codex-acp@1.7.0";',
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

    def test_general_log_reuses_existing_output_and_private_writers_reject_symlinks(
        self,
    ):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            general_out = root / "general-probe.jsonl"
            general_out.write_text("stale general probe output")
            client = None
            with mock.patch.object(probe.os, "name", "nt"):
                try:
                    client = probe.AcpClient(FakeAcpProc(), general_out, quiet=True)
                except OSError:
                    pass
            self.assertIsNotNone(
                client,
                "ordinary probes must retain the portable truncate-and-reuse writer",
            )
            client.close()
            self.assertEqual(general_out.read_text(), "")

            adapter_error = getattr(probe, "AdapterBlockerError", None)
            self.assertIsNotNone(adapter_error)
            restricted_out = root / "restricted-probe.jsonl"
            restricted_client = probe.AcpClient(
                FakeAcpProc(),
                restricted_out,
                quiet=True,
                restricted_artifacts=True,
            )

            class BrokenAdapterInput:
                def write(self, _text):
                    raise BrokenPipeError("adapter stdin closed")

                def flush(self):
                    pass

            original_stdin = restricted_client.stdin
            restricted_client.stdin = BrokenAdapterInput()
            try:
                with self.assertRaises(adapter_error):
                    restricted_client.send({"jsonrpc": "2.0", "method": "probe"})
            finally:
                restricted_client.stdin = original_stdin
                restricted_client.close()

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

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_fresh_private_root_rejects_existing_and_symlink_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            parent = Path(tmp)
            victim = parent / "victim"
            victim.mkdir()
            link = parent / "linked-output"
            link.symlink_to(victim, target_is_directory=True)

            with self.assertRaises(OSError):
                probe.create_fresh_private_root(link)
            self.assertTrue(victim.is_dir())

            existing = parent / "existing-output"
            existing.mkdir()
            with self.assertRaises(FileExistsError):
                probe.create_fresh_private_root(existing)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_private_file_creation_rejects_intermediate_directory_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            victim = Path(tmp) / "victim-directory"
            external_parent = victim / "captured"
            external_parent.mkdir(parents=True)
            link = root / "positive-workspace"
            link.symlink_to(victim, target_is_directory=True)
            artifact = link / "captured" / "positive.stdout"
            external_artifact = external_parent / artifact.name

            with self.assertRaises(OSError):
                probe.write_private_text(artifact, "sensitive output")
            self.assertFalse(external_artifact.exists())

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_final_artifact_audit_rejects_symlinks_and_finalizes_post_root_errors(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            victim = Path(tmp) / "victim"
            victim.write_text("outside")
            (root / "linked-artifact").symlink_to(victim)

            with self.assertRaises(OSError):
                probe.protect_artifact_tree(root)
            self.assertEqual(victim.read_text(), "outside")
            with self.assertRaisesRegex(OSError, "artifact root is missing"):
                probe.protect_artifact_tree(Path(tmp) / "missing-artifact-root")

            def failing_walk(*_args, **kwargs):
                onerror = kwargs.get("onerror")
                if onerror is not None:
                    onerror(OSError("injected artifact subtree scan failure"))
                return ()

            with (
                mock.patch.object(probe.os, "walk", side_effect=failing_walk),
                self.assertRaisesRegex(OSError, "subtree scan failure"),
            ):
                probe.protect_artifact_tree(root)

        versions = Namespace(
            adapter_version="1.1.2",
            codex_package_version="0.144.1",
            codex_cli_version="0.144.1",
            codex_cli_path="/tmp/codex",
            codex_cli_output="codex-cli 0.144.1",
        )
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "post-root-security-error"
            args = Namespace(
                codex_package=probe.DEFAULT_CODEX_PACKAGE,
                profile_probe_out_dir=output,
            )
            with (
                mock.patch.object(
                    probe,
                    "resolve_codex_versions",
                    return_value=versions,
                ),
                mock.patch.object(
                    probe,
                    "initialize_codex_probe_workspace",
                    side_effect=OSError("injected post-root security failure"),
                ),
                mock.patch.object(
                    probe,
                    "finalize_profile_artifacts",
                    wraps=probe.finalize_profile_artifacts,
                ) as finalizer,
                mock.patch.object(sys, "stdout", io.StringIO()),
                mock.patch.object(sys, "stderr", io.StringIO()),
            ):
                try:
                    exit_code = probe.run_codex_profile_probe(args)
                except OSError:
                    exit_code = None

            self.assertEqual(exit_code, 1)
            finalizer.assert_called_once_with(output.resolve(), 1)

        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "workspace-setup-error"
            args = Namespace(
                codex_package=probe.DEFAULT_CODEX_PACKAGE,
                profile_probe_out_dir=output,
            )
            workspace_stderr = io.StringIO()
            with (
                mock.patch.object(
                    probe,
                    "resolve_codex_versions",
                    return_value=versions,
                ),
                mock.patch.object(
                    probe,
                    "initialize_codex_probe_workspace",
                    side_effect=RuntimeError("injected git init failure"),
                ),
                mock.patch.object(
                    probe,
                    "finalize_profile_artifacts",
                    wraps=probe.finalize_profile_artifacts,
                ) as finalizer,
                mock.patch.object(sys, "stdout", io.StringIO()),
                mock.patch.object(sys, "stderr", workspace_stderr),
            ):
                exit_code = probe.run_codex_profile_probe(args)

            self.assertEqual(exit_code, 1)
            finalizer.assert_called_once_with(output.resolve(), 1)
            self.assertIn(
                "[FAIL] Codex probe workspace setup failed",
                workspace_stderr.getvalue(),
            )

        restricted_stderr = io.StringIO()
        with (
            mock.patch.object(
                probe,
                "configure_restricted_profile_process",
                return_value=None,
            ),
            mock.patch.object(
                probe,
                "run_probe",
                side_effect=OSError("injected inner artifact failure"),
            ),
            mock.patch.object(
                sys,
                "argv",
                ["probe_acp_subagents.py", "--restricted-profile-probe"],
            ),
            mock.patch.object(sys, "stderr", restricted_stderr),
        ):
            try:
                restricted_exit = probe.main()
            except OSError:
                restricted_exit = None

        self.assertEqual(restricted_exit, 1)
        self.assertIn(
            "[FAIL] restricted profile artifact operation failed",
            restricted_stderr.getvalue(),
        )

        version_stderr = io.StringIO()
        with (
            mock.patch.object(
                probe,
                "resolve_codex_versions",
                side_effect=FileNotFoundError("npx executable missing"),
            ),
            mock.patch.object(sys, "stdout", io.StringIO()),
            mock.patch.object(sys, "stderr", version_stderr),
        ):
            try:
                version_exit = probe.run_codex_profile_probe(
                    Namespace(
                        codex_package=probe.DEFAULT_CODEX_PACKAGE,
                        profile_probe_out_dir=Path("unused"),
                    )
                )
            except OSError:
                version_exit = None

        self.assertEqual(version_exit, 2)
        self.assertIn(
            "[BLOCKED] Codex version discovery failed",
            version_stderr.getvalue(),
        )

        with tempfile.TemporaryDirectory() as tmp:
            adapter_stderr = io.StringIO()
            adapter_out = Path(tmp) / "restricted.jsonl"
            with (
                mock.patch.object(
                    probe,
                    "configure_restricted_profile_process",
                    return_value=None,
                ),
                mock.patch.object(
                    probe,
                    "_retain_existing_private_root",
                    return_value=adapter_out.parent,
                ),
                mock.patch.object(
                    probe.subprocess,
                    "Popen",
                    side_effect=FileNotFoundError("adapter executable missing"),
                ),
                mock.patch.object(
                    sys,
                    "argv",
                    [
                        "probe_acp_subagents.py",
                        "--restricted-profile-probe",
                        "--out",
                        str(adapter_out),
                    ],
                ),
                mock.patch.object(sys, "stdout", io.StringIO()),
                mock.patch.object(sys, "stderr", adapter_stderr),
            ):
                adapter_exit = probe.main()

            self.assertEqual(adapter_exit, 2)
            self.assertIn("[BLOCKED] restricted adapter", adapter_stderr.getvalue())

    @unittest.skipUnless(os.name == "posix", "POSIX mode contract")
    def test_restricted_profile_process_sets_private_umask(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "restricted-output"
            code = "\n".join(
                [
                    "from pathlib import Path",
                    "import probe_acp_subagents as probe",
                    "probe.configure_restricted_profile_process(True)",
                    f"root = Path({str(output)!r})",
                    "root.mkdir()",
                    "(root / 'rollout.jsonl').write_text('private')",
                ]
            )
            completed = subprocess.run(
                [sys.executable, "-c", code],
                cwd=Path(probe.__file__).resolve().parent,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            self.assertEqual(
                (output / "rollout.jsonl").stat().st_mode & 0o777,
                0o600,
            )

    @unittest.skipUnless(os.name == "posix", "POSIX mode contract")
    def test_rollout_audit_and_readers_reject_intermediate_symlink_swaps(self):
        with tempfile.TemporaryDirectory() as tmp:
            session_root = Path(tmp) / "sessions"
            rollout_dir = session_root / "2026" / "07" / "10"
            rollout_dir.mkdir(parents=True, mode=0o755)
            for directory in (
                session_root,
                session_root / "2026",
                session_root / "2026" / "07",
                rollout_dir,
            ):
                directory.chmod(0o755)
            rollout = rollout_dir / "rollout-test.jsonl"
            rollout.write_text('{"type":"session_meta"}\n')
            rollout.chmod(0o644)
            session_alias = Path(tmp) / "session-alias"
            session_alias.symlink_to(session_root, target_is_directory=True)
            supplied_rollout = session_alias / rollout.relative_to(session_root)

            audit = probe.audit_codex_rollouts(session_alias, {supplied_rollout})

            self.assertEqual(audit.failures, ())
            observed_reader_paths = []
            read_text_no_follow = probe.read_text_no_follow

            def record_reader_path(path):
                observed_reader_paths.append(path)
                return read_text_no_follow(path)

            with mock.patch.object(
                probe,
                "read_text_no_follow",
                side_effect=record_reader_path,
            ):
                probe.load_codex_rollout_activity(audit.paths, Path(tmp))
                probe.load_codex_role_binding(audit.paths, Path(tmp))

            canonical_rollout = rollout.resolve()
            self.assertEqual(observed_reader_paths, [canonical_rollout] * 2)
            self.assertEqual(audit.paths, (canonical_rollout,))
            self.assertEqual(rollout.stat().st_mode & 0o777, 0o600)
            for directory in (
                session_root,
                session_root / "2026",
                session_root / "2026" / "07",
                rollout_dir,
            ):
                self.assertEqual(directory.stat().st_mode & 0o777, 0o700)

            external_rollout = (
                Path(tmp) / "external" / "2026" / "07" / "10" / rollout.name
            )
            external_rollout.parent.mkdir(parents=True)
            external_contents = json.dumps(
                {
                    "type": "session_meta",
                    "payload": {"id": "external", "cwd": str(Path(tmp).resolve())},
                }
            )
            external_rollout.write_text(external_contents)
            external_rollout.chmod(0o644)
            retired_year = session_root / "2026-before-reader-swap"
            (session_root / "2026").rename(retired_year)
            (session_root / "2026").symlink_to(
                external_rollout.parents[2],
                target_is_directory=True,
            )

            swapped_activity = probe.load_codex_rollout_activity(
                audit.paths,
                Path(tmp),
            )
            swapped_binding = probe.load_codex_role_binding(audit.paths, Path(tmp))

            with self.subTest(swap="between audit and activity reader"):
                self.assertTrue(
                    any(
                        "read failed" in item
                        for item in swapped_activity.evidence_errors
                    )
                )
            with self.subTest(swap="between audit and role-binding reader"):
                self.assertIn("read failed", swapped_binding.evidence_error or "")
            with self.subTest(swap="reader external victim"):
                self.assertEqual(external_rollout.read_text(), external_contents)
                self.assertEqual(external_rollout.stat().st_mode & 0o777, 0o644)

        for tamper in (
            "rename replacement 0644",
            "rename replacement 0600",
            "in-place content mutation",
            "hard-link substitution",
        ):
            with self.subTest(tamper=tamper), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                workspace = root / "workspace"
                workspace.mkdir()
                session_root = root / "sessions"
                rollout_dir = session_root / "2026" / "07" / "10"
                rollout_dir.mkdir(parents=True)
                rollout = rollout_dir / "rollout-test.jsonl"
                original_contents = json.dumps(
                    {
                        "type": "session_meta",
                        "payload": {"id": "parent", "cwd": str(workspace)},
                    }
                )
                rollout.write_text(original_contents + "\n")

                audit = probe.audit_codex_rollouts(session_root, {rollout})

                self.assertEqual(audit.failures, ())
                audited = audit.paths[0]
                audited_path = getattr(audited, "path", audited)
                external_victim = None
                if tamper.startswith("rename replacement"):
                    audited_path.rename(audited_path.with_suffix(".audited"))
                    audited_path.write_text("FORGED replacement\n")
                    replacement_mode = 0o644 if tamper.endswith("0644") else 0o600
                    audited_path.chmod(replacement_mode)
                elif tamper == "in-place content mutation":
                    audited_path.write_text("FORGED mutation\n")
                    audited_path.chmod(0o600)
                else:
                    audited_path.rename(audited_path.with_suffix(".audited"))
                    external_victim = root / "external-victim.jsonl"
                    external_victim.write_text("FORGED hard-link victim\n")
                    external_victim.chmod(0o644)
                    os.link(external_victim, audited_path)

                direct_error = None
                try:
                    probe.read_text_no_follow(audited)
                except OSError as exc:
                    direct_error = str(exc)
                activity = probe.load_codex_rollout_activity(audit.paths, workspace)
                binding = probe.load_codex_role_binding(audit.paths, workspace)
                activity_read_failed = any(
                    "read failed" in error for error in activity.evidence_errors
                )
                binding_read_failed = "read failed" in (binding.evidence_error or "")

                self.assertEqual(
                    (
                        direct_error is not None,
                        activity_read_failed,
                        binding_read_failed,
                    ),
                    (True, True, True),
                )
                combined_errors = " ".join(
                    (
                        direct_error or "",
                        *activity.evidence_errors,
                        binding.evidence_error or "",
                    )
                )
                self.assertNotIn("FORGED", combined_errors)
                if external_victim is not None:
                    self.assertEqual(
                        external_victim.read_text(), "FORGED hard-link victim\n"
                    )
                    self.assertEqual(external_victim.stat().st_mode & 0o777, 0o644)

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session_root = root / "sessions"
            year = session_root / "2026"
            rollout_dir = year / "07" / "10"
            rollout_dir.mkdir(parents=True)
            rollout = rollout_dir / "rollout-test.jsonl"
            rollout.write_text('{"type":"session_meta"}\n')

            external_year = root / "external" / "2026"
            external_rollout = external_year / "07" / "10" / rollout.name
            external_rollout.parent.mkdir(parents=True)
            external_contents = "external rollout must remain untouched"
            external_rollout.write_text(external_contents)
            external_rollout.chmod(0o644)
            retired_year = session_root / "2026-before-audit-swap"
            canonical_year = year.resolve()
            original_harden_directory = probe._harden_directory
            parent_swapped = False

            def harden_then_swap_parent(path):
                nonlocal parent_swapped
                original_harden_directory(path)
                if path == canonical_year and not parent_swapped:
                    year.rename(retired_year)
                    year.symlink_to(external_year, target_is_directory=True)
                    parent_swapped = True

            with mock.patch.object(
                probe,
                "_harden_directory",
                side_effect=harden_then_swap_parent,
            ):
                audit = probe.audit_codex_rollouts(session_root, {rollout})

            self.assertTrue(parent_swapped)
            with self.subTest(swap="between rollout directory checks"):
                self.assertEqual(audit.paths, ())
                self.assertTrue(audit.failures)
            with self.subTest(swap="audit external victim"):
                self.assertEqual(external_rollout.read_text(), external_contents)
                self.assertEqual(external_rollout.stat().st_mode & 0o777, 0o644)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_rollout_audit_rejects_symlink_missing_and_out_of_root_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session_root = root / "sessions"
            session_root.mkdir()
            victim = root / "victim.jsonl"
            victim.write_text("external")
            linked = session_root / "rollout-linked.jsonl"
            linked.symlink_to(victim)
            missing = session_root / "rollout-missing.jsonl"
            shared_victim = root / "shared-victim.jsonl"
            shared_victim.write_text("shared external content")
            shared_victim.chmod(0o644)
            hardlinked = session_root / "rollout-hardlinked.jsonl"
            os.link(shared_victim, hardlinked)

            audit = probe.audit_codex_rollouts(
                session_root,
                {hardlinked, linked, missing, victim},
            )

            self.assertEqual(audit.paths, ())
            self.assertTrue(
                any("link count" in item for item in audit.failures), audit.failures
            )
            self.assertTrue(any("symlink" in item for item in audit.failures))
            self.assertTrue(any("missing" in item for item in audit.failures))
            self.assertTrue(any("outside" in item for item in audit.failures))
            self.assertEqual(victim.read_text(), "external")
            self.assertEqual(shared_victim.read_text(), "shared external content")
            self.assertEqual(shared_victim.stat().st_mode & 0o777, 0o644)

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

        classifier = getattr(probe, "profile_subprocess_failure_exit", None)
        self.assertIsNotNone(classifier)
        security_failure = Namespace(
            stderr="[FAIL] restricted profile artifact operation failed: unsafe path"
        )
        adapter_blocker = Namespace(
            stderr="[BLOCKED] restricted adapter process failed: executable missing"
        )
        generic_blocked_failure = Namespace(
            stderr="[BLOCKED] deterministic profile contract failure"
        )
        unsupported_invariant = Namespace(
            stderr=(
                "[BLOCKED] restricted artifact invariant unsupported: "
                "missing O_NOFOLLOW"
            )
        )
        initialize_failure = Namespace(stderr="[FAIL] initialize timed out")
        contract_failure = Namespace(
            stderr=(
                "[FAIL] session/prompt error: deterministic profile contract failure"
            )
        )
        unclassified_failure = Namespace(stderr="arbitrary unexpected failure")
        self.assertEqual(classifier(security_failure), 1)
        self.assertEqual(classifier(adapter_blocker), 2)
        self.assertEqual(classifier(generic_blocked_failure), 1)
        self.assertEqual(classifier(unsupported_invariant), 2)
        self.assertEqual(classifier(initialize_failure), 1)
        self.assertEqual(classifier(contract_failure), 1)
        self.assertEqual(classifier(unclassified_failure), 1)

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_prepare_app_server_log_rejects_symlinked_directory_and_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")

            victim_dir = Path(tmp) / "external-log-directory"
            victim_dir.mkdir()
            victim_log = victim_dir / "app-server.log"
            victim_log.write_text("external-content")
            linked_dir = root / "positive-app-server-logs"
            linked_dir.symlink_to(victim_dir, target_is_directory=True)
            with self.assertRaises(OSError):
                probe.prepare_app_server_log(linked_dir)
            self.assertEqual(victim_log.read_text(), "external-content")

            safe_dir = probe.ensure_private_directory(
                root / "negative-app-server-logs", exist_ok=False
            )
            second_victim = Path(tmp) / "external-file"
            second_victim.write_text("do-not-replace")
            (safe_dir / "app-server.log").symlink_to(second_victim)
            with self.assertRaises(OSError):
                probe.prepare_app_server_log(safe_dir, directory_exists=True)
            self.assertEqual(second_victim.read_text(), "do-not-replace")

    @unittest.skipUnless(os.name == "posix", "POSIX no-follow contract")
    def test_prepare_app_server_log_rejects_existing_regular_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = probe.create_fresh_private_root(Path(tmp) / "artifacts")
            log_dir = probe.ensure_private_directory(
                root / "positive-app-server-logs", exist_ok=False
            )
            log_path = log_dir / "app-server.log"
            log_path.write_text(probe.MALFORMED_ROLE_WARNING)

            with self.assertRaises(FileExistsError):
                probe.prepare_app_server_log(log_dir, directory_exists=True)
            self.assertEqual(log_path.read_text(), probe.MALFORMED_ROLE_WARNING)

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

    def test_space_separated_spawn_title_participates_in_canary_scan(self):
        records = [
            {
                "dir": "recv",
                "msg": {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": "spawn-1",
                            "title": "spawn agent",
                            "rawInput": {"message": "PRIMARY-123"},
                        }
                    },
                },
            }
        ]
        with tempfile.TemporaryDirectory() as tmp:
            raw_log = Path(tmp) / "positive.jsonl"
            raw_log.write_text("\n".join(json.dumps(item) for item in records))
            evidence = probe.load_codex_probe_evidence(raw_log)

        self.assertEqual(evidence.spawn_agent_call_ids, ("spawn-1",))
        self.assertEqual(
            evidence.spawn_agent_raw_inputs,
            (json.dumps({"message": "PRIMARY-123"}, sort_keys=True),),
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

    def test_terminal_wait_response_and_failed_client_setup_clean_up_processes(self):
        terminal = probe.TerminalState(FakeProc(), 1024)

        self.assertEqual(terminal.wait_response(), {"exitCode": 0})
        output = terminal.output_response()["output"]

        self.assertIn("stdout-tail", output)
        self.assertIn("stderr-tail", output)
        self.assertFalse(terminal._stdout.is_alive())
        self.assertFalse(terminal._stderr.is_alive())

        if os.name != "posix":
            return

        with self.subTest(cleanup="real child"), tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            out_path = root / "raw.jsonl"
            args = Namespace(
                agent="codex",
                out=out_path,
                quiet=True,
                restricted_profile_probe=True,
            )
            real_popen = subprocess.Popen
            children = []

            def capture_popen(*popen_args, **popen_kwargs):
                child = real_popen(*popen_args, **popen_kwargs)
                children.append(child)
                return child

            try:
                with (
                    mock.patch.object(
                        probe,
                        "agent_command",
                        return_value=[
                            sys.executable,
                            "-c",
                            "import time; time.sleep(30)",
                        ],
                    ),
                    mock.patch.object(
                        probe.subprocess,
                        "Popen",
                        side_effect=capture_popen,
                    ),
                    mock.patch.object(
                        probe.AcpClient,
                        "__init__",
                        side_effect=OSError("client setup failed"),
                    ),
                    mock.patch("builtins.print"),
                ):
                    with self.assertRaisesRegex(OSError, "client setup failed"):
                        probe.run_probe(args)

                child = children[0]
                observed = (
                    child.poll() is None,
                    tuple(
                        stream.closed
                        for stream in (child.stdin, child.stdout, child.stderr)
                    ),
                    probe._absolute_lexical_path(root)
                    in probe._PRIVATE_ARTIFACT_ROOT_DESCRIPTORS,
                )
            finally:
                for child in children:
                    if child.poll() is None:
                        child.kill()
                        child.wait(timeout=5)
                    for stream in (child.stdin, child.stdout, child.stderr):
                        if stream is not None and not stream.closed:
                            stream.close()

            self.assertEqual(observed, (False, (True, True, True), False))

        with (
            self.subTest(cleanup="kill fallback"),
            tempfile.TemporaryDirectory() as tmp,
        ):
            root = Path(tmp)
            args = Namespace(
                agent="codex",
                out=root / "raw.jsonl",
                quiet=True,
                restricted_profile_probe=True,
            )
            stubborn = mock.Mock()
            stubborn.stdin = io.StringIO()
            stubborn.stdout = io.StringIO()
            stubborn.stderr = io.StringIO()
            stubborn.poll.return_value = None
            stubborn.wait.side_effect = [
                subprocess.TimeoutExpired(cmd="adapter", timeout=5),
                0,
            ]
            try:
                with (
                    mock.patch.object(probe, "agent_command", return_value=["adapter"]),
                    mock.patch.object(probe.subprocess, "Popen", return_value=stubborn),
                    mock.patch.object(
                        probe.AcpClient,
                        "__init__",
                        side_effect=OSError("client setup failed"),
                    ),
                    mock.patch("builtins.print"),
                ):
                    with self.assertRaisesRegex(OSError, "client setup failed"):
                        probe.run_probe(args)

                observed = (
                    stubborn.terminate.call_count,
                    stubborn.kill.call_count,
                    stubborn.wait.call_count,
                    tuple(
                        stream.closed
                        for stream in (stubborn.stdin, stubborn.stdout, stubborn.stderr)
                    ),
                    probe._absolute_lexical_path(root)
                    in probe._PRIVATE_ARTIFACT_ROOT_DESCRIPTORS,
                )
            finally:
                for stream in (stubborn.stdin, stubborn.stdout, stubborn.stderr):
                    if not stream.closed:
                        stream.close()

            self.assertEqual(observed, (1, 1, 2, (True, True, True), False))

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
