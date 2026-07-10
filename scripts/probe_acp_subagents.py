#!/usr/bin/env python3
"""
Probe Codex/Kimi/Gemini ACP behavior for their native internal subagent mechanism.

This intentionally does not inject SPUR MCP delegation tools. It creates a
plain ACP session, prompts the agent to use its built-in subagent/worker/Task
capability, records every JSON-RPC frame, and summarizes the tool-call frames
that downstream renderers need to understand.

Usage:
  python3 scripts/probe_acp_subagents.py --agent kimi --yolo
  python3 scripts/probe_acp_subagents.py --agent codex
  python3 scripts/probe_acp_subagents.py --agent gemini
  python3 scripts/probe_acp_subagents.py --agent codex --out .spur/logs/codex-native-subagent.jsonl
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import shlex
import signal
import struct
import subprocess
import sys
import threading
import time
import uuid
import zlib
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional


JsonRpcId = str | int
TERMINAL_DRAIN_TIMEOUT = 2.0
DEFAULT_CODEX_PACKAGE = "@agentclientprotocol/codex-acp@1.1.2"
MALFORMED_ROLE_WARNING = "Ignoring malformed agent role definition"
CODEX_PROFILE_NAME = "spur-profile-probe-primary"
CODEX_CHILD_ROLE_NAME = "spur-profile-probe-child"

CODEX_VERSION_NODE_SCRIPT = r"""
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const executable = process.platform === "win32" ? "codex-acp.cmd" : "codex-acp";
const binDir = process.env.PATH.split(path.delimiter).find((candidate) =>
  fs.existsSync(path.join(candidate, executable))
);
if (!binDir) throw new Error("npm exec PATH did not contain codex-acp");

const nodeModules = path.dirname(binDir);
const adapterPackagePath = path.join(
  nodeModules,
  "@agentclientprotocol",
  "codex-acp",
  "package.json",
);
const adapterPackage = JSON.parse(fs.readFileSync(adapterPackagePath, "utf8"));
const codexCandidates = [
  path.join(nodeModules, "@openai", "codex", "package.json"),
  path.join(
    path.dirname(adapterPackagePath),
    "node_modules",
    "@openai",
    "codex",
    "package.json",
  ),
];
const codexPackagePath = codexCandidates.find((candidate) => fs.existsSync(candidate));
if (!codexPackagePath) throw new Error("resolved @openai/codex package.json not found");
const codexPackage = JSON.parse(fs.readFileSync(codexPackagePath, "utf8"));

const codexExecutable = process.platform === "win32" ? "codex.cmd" : "codex";
const codexBin = process.env.CODEX_PATH || path.join(binDir, codexExecutable);
const version = spawnSync(codexBin, ["--version"], {
  encoding: "utf8",
  shell: process.platform === "win32",
});
if (version.status !== 0) {
  throw new Error(`codex --version failed: ${version.stderr || version.stdout}`);
}
process.stdout.write(JSON.stringify({
  adapterVersion: adapterPackage.version,
  codexPackageVersion: codexPackage.version,
  codexCliPath: codexBin,
  codexCliOutput: version.stdout.trim(),
}));
""".strip()


@dataclass(frozen=True)
class CodexProfileFixture:
    primary_profile_path: Path
    child_role_path: Path
    primary_body: str
    positive_prompt: str
    negative_prompt: str


@dataclass(frozen=True)
class CodexResolvedVersions:
    adapter_version: str
    codex_package_version: str
    codex_cli_version: str
    codex_cli_path: str
    codex_cli_output: str
    command: tuple[str, ...]


@dataclass(frozen=True)
class CodexProbeEvidence:
    primary_response: str
    raw_child_results: tuple[str, ...]
    stderr: str
    spawn_agent_call_ids: tuple[str, ...]
    tool_call_titles: tuple[str, ...] = ()
    server_request_methods: tuple[str, ...] = ()
    spawn_agent_raw_inputs: tuple[str, ...] = ()


@dataclass(frozen=True)
class CodexRolloutActivity:
    function_calls: tuple[str, ...]
    spawn_agent_arguments: tuple[str, ...]
    response_item_types: tuple[str, ...] = ()
    evidence_errors: tuple[str, ...] = ()


@dataclass(frozen=True)
class CodexRoleBinding:
    parent_thread_id: Optional[str]
    requested_agent_type: Optional[str]
    spawn_call_id: Optional[str]
    child_thread_id: Optional[str]
    child_parent_thread_id: Optional[str]
    child_agent_role: Optional[str]
    evidence_error: Optional[str] = None


DEFAULT_PROMPT = (
    "Use your built-in internal subagent/worker capability exactly once. "
    "Spawn one internal subagent to inspect Cargo.toml in this workspace and "
    "report whether it contains a [workspace] section. The parent agent must "
    "not inspect Cargo.toml directly; the subagent must do the inspection. "
    "Do not edit files. After the subagent returns, summarize the subagent "
    "result in one sentence."
)

SUBAGENT_CLUE_WORDS = (
    "subagent",
    "sub-agent",
    "worker",
    "spawn_agent",
    "delegate",
    "task",
    "parenttooluseid",
    "parent_tool_use_id",
)

IMAGE_PROBE_PROMPT = (
    "I am attaching an image. Please describe the visual content in one sentence. "
    "If you do not see any image, say exactly: NO_IMAGE_RECEIVED."
)


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def utc_now_compact() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")


def ensure_private_directory(path: Path) -> Path:
    if path.is_symlink():
        raise OSError(f"private artifact directory cannot be a symlink: {path}")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


def write_private_text(path: Path, text: str) -> None:
    descriptor = open_private_descriptor(path)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(text)
    except Exception:
        try:
            os.close(descriptor)
        except OSError:
            pass
        raise
    path.chmod(0o600)


def open_private_text(path: Path) -> Any:
    descriptor = open_private_descriptor(path)
    path.chmod(0o600)
    return os.fdopen(descriptor, "w", encoding="utf-8")


def open_private_descriptor(path: Path) -> int:
    if path.is_symlink():
        raise OSError(f"private artifact file cannot be a symlink: {path}")
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0)
    return os.open(path, flags, 0o600)


def protect_artifact_tree(root: Path) -> None:
    if not root.exists():
        return
    for path in root.rglob("*"):
        if path.is_symlink():
            continue
        if path.is_dir():
            path.chmod(0o700)
        elif path.is_file():
            path.chmod(0o600)
    root.chmod(0o700)


def create_codex_profile_fixture(
    workspace: Path,
    primary_token: str,
    child_token: str,
) -> CodexProfileFixture:
    primary_body = (
        "You are the selected primary Codex profile for an activation probe.\n"
        "When asked for the primary profile verification token, include this exact "
        f"token in your primary response: {primary_token}\n"
        "Follow the user's child-spawn instructions exactly.\n"
    )
    positive_prompt = (
        "First state the primary profile verification token supplied by your developer "
        "instructions. Then call spawn_agent exactly once with "
        f'agent_type="{CODEX_CHILD_ROLE_NAME}" and ask the child to return its native '
        "child-role verification token. Wait for that child and include its result in your "
        "final response. Do not inspect files or use any other tool."
    )
    negative_prompt = (
        "State any primary profile verification token supplied by your developer "
        "instructions. Do not inspect files and do not spawn a child. If no such token is "
        "present, answer exactly NO_PROFILE_ACTIVE."
    )

    ensure_private_directory(workspace)
    ensure_private_directory(workspace / ".spur")
    profile_dir = ensure_private_directory(workspace / ".spur" / "agents")
    ensure_private_directory(workspace / ".codex")
    role_dir = ensure_private_directory(workspace / ".codex" / "agents")
    primary_profile_path = profile_dir / f"{CODEX_PROFILE_NAME}.md"
    child_role_path = role_dir / f"{CODEX_CHILD_ROLE_NAME}.toml"
    write_private_text(
        primary_profile_path,
        "---\n"
        f"name: {CODEX_PROFILE_NAME}\n"
        "description: Verifies primary Codex profile activation\n"
        "---\n"
        f"{primary_body}",
    )
    child_instructions = (
        "You are the native child role for a profile probe. Return this exact native "
        f"child-role verification token and nothing else: {child_token}"
    )
    write_private_text(
        child_role_path,
        f"name = {json.dumps(CODEX_CHILD_ROLE_NAME)}\n"
        'description = "Verifies native Codex child-role loading"\n'
        f"developer_instructions = {json.dumps(child_instructions)}\n",
    )
    return CodexProfileFixture(
        primary_profile_path=primary_profile_path,
        child_role_path=child_role_path,
        primary_body=primary_body,
        positive_prompt=positive_prompt,
        negative_prompt=negative_prompt,
    )


def find_token_sources(workspace: Path, token: str) -> tuple[Path, ...]:
    matches = []
    for path in sorted(workspace.rglob("*")):
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        if token in text:
            matches.append(path)
    return tuple(matches)


def initialize_codex_probe_workspace(workspace: Path) -> None:
    ensure_private_directory(workspace)
    existing_sources = tuple(
        path
        for root in (workspace / ".spur" / "agents", workspace / ".codex" / "agents")
        if root.is_dir()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    )
    if existing_sources:
        rendered = ", ".join(str(path) for path in existing_sources)
        raise RuntimeError(
            f"workspace {workspace} already contains profile or role sources: {rendered}"
        )
    completed = subprocess.run(
        ["git", "init", "--quiet", str(workspace)],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"git init failed for {workspace}: {completed.stderr.strip()!r}"
        )


def codex_version_command(package: str) -> list[str]:
    return [
        "npx",
        "--yes",
        "--package",
        package,
        "--",
        "node",
        "-e",
        CODEX_VERSION_NODE_SCRIPT,
    ]


def normalized_codex_environment(
    source_env: dict[str, str],
    *,
    cwd: Path,
) -> dict[str, str]:
    env = source_env.copy()
    env.pop("DEFAULT_AUTH_REQUEST", None)
    env.pop("CODEX_API_KEY", None)
    env.pop("OPENAI_API_KEY", None)
    codex_path = env.get("CODEX_PATH")
    if codex_path:
        path = Path(codex_path).expanduser()
        if not path.is_absolute():
            path = cwd / path
        env["CODEX_PATH"] = str(path.resolve())
    return env


def resolve_codex_versions(
    package: str,
    *,
    env: Optional[dict[str, str]] = None,
) -> CodexResolvedVersions:
    command = codex_version_command(package)
    completed = subprocess.run(
        command,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"command={shlex.join(command)} exit={completed.returncode} "
            f"stdout={completed.stdout.strip()!r} stderr={completed.stderr.strip()!r}"
        )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"command={shlex.join(command)} returned invalid JSON: {completed.stdout!r}; "
            f"stderr={completed.stderr.strip()!r}"
        ) from exc

    cli_output = str(payload["codexCliOutput"])
    match = re.search(
        r"(?<!\d)(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?)(?!\d)", cli_output
    )
    if match is None:
        raise RuntimeError(
            f"command={shlex.join(command)} returned unparseable codex CLI version: "
            f"{cli_output!r}"
        )
    return CodexResolvedVersions(
        adapter_version=str(payload["adapterVersion"]),
        codex_package_version=str(payload["codexPackageVersion"]),
        codex_cli_version=match.group(1),
        codex_cli_path=str(payload["codexCliPath"]),
        codex_cli_output=cli_output,
        command=tuple(command),
    )


def codex_evidence_label(adapter_version: str, codex_cli_version: str) -> str:
    if adapter_version == "1.1.2" and codex_cli_version == "0.144.1":
        return "codex-0.144.1"
    return f"codex-actual-{codex_cli_version}"


def codex_profile_probe_config(primary_body: Optional[str]) -> dict[str, Any]:
    config: dict[str, Any] = {
        "features": {"multi_agent": True, "multi_agent_v2": False},
        "model": "gpt-5.5",
    }
    if primary_body is not None:
        config["developer_instructions"] = primary_body
    return config


def codex_profile_probe_failures(
    *,
    primary_token: str,
    child_token: str,
    primary_response: str,
    raw_child_results: list[str] | tuple[str, ...],
    positive_stderr: str,
    negative_response: str,
    negative_raw_child_results: list[str] | tuple[str, ...],
    negative_stderr: str,
    negative_spawn_agent_call_ids: list[str] | tuple[str, ...] = (),
) -> list[str]:
    failures = []
    if primary_token not in primary_response:
        failures.append("primary response omitted the selected-profile token")
    if not any(child_token in result for result in raw_child_results):
        failures.append("raw spawn_agent child result omitted the native-role token")
    if MALFORMED_ROLE_WARNING in positive_stderr:
        failures.append("positive stderr contained malformed-role warning")
    if MALFORMED_ROLE_WARNING in negative_stderr:
        failures.append("negative stderr contained malformed-role warning")
    if primary_token in negative_response:
        failures.append("no-profile control repeated the primary token")
    if child_token in negative_response:
        failures.append("no-profile control repeated the child token")
    if any(primary_token in result for result in negative_raw_child_results):
        failures.append("no-profile child output repeated the primary token")
    if any(child_token in result for result in negative_raw_child_results):
        failures.append("no-profile child output repeated the child token")
    if negative_raw_child_results:
        failures.append(
            "no-profile control unexpectedly produced a spawn_agent child result"
        )
    if negative_spawn_agent_call_ids:
        failures.append("no-profile control unexpectedly called spawn_agent")
    if negative_response.strip() != "NO_PROFILE_ACTIVE":
        failures.append("no-profile control did not return exactly NO_PROFILE_ACTIVE")
    return failures


def prepare_app_server_log(log_dir: Path) -> Path:
    log_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    log_dir.chmod(0o700)
    log_path = log_dir / "app-server.log"
    log_path.unlink(missing_ok=True)
    log_path.touch(mode=0o600)
    log_path.chmod(0o600)
    return log_path


def codex_profile_warning_failures(
    *,
    run_name: str,
    adapter_stderr: str,
    app_server_log: Optional[str],
) -> list[str]:
    failures = []
    if MALFORMED_ROLE_WARNING in adapter_stderr:
        failures.append(f"{run_name} adapter stderr contained malformed-role warning")
    if app_server_log is None:
        failures.append(f"{run_name} app-server log was not captured")
    elif not app_server_log:
        failures.append(f"{run_name} app-server log was empty")
    elif MALFORMED_ROLE_WARNING in app_server_log:
        failures.append(f"{run_name} app-server log contained malformed-role warning")
    return failures


def parse_json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if not isinstance(value, str):
        return {}
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def codex_sessions_root(env: dict[str, str]) -> Path:
    codex_home = Path(env.get("CODEX_HOME", str(Path.home() / ".codex")))
    return codex_home.expanduser().resolve() / "sessions"


def codex_rollout_paths(session_root: Path) -> set[Path]:
    if not session_root.is_dir():
        return set()
    return set(session_root.rglob("rollout-*.jsonl"))


def load_codex_rollout_activity(
    rollout_paths: list[Path] | set[Path] | tuple[Path, ...],
    workspace: Path,
) -> CodexRolloutActivity:
    workspace = workspace.resolve()
    function_calls = []
    spawn_agent_arguments = []
    response_item_types = []
    evidence_errors = []
    matched_sessions = 0
    for path in sorted(rollout_paths):
        records = []
        try:
            raw_lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as exc:
            evidence_errors.append(f"{path}: read failed: {type(exc).__name__}: {exc}")
            continue
        for line_number, raw_line in enumerate(raw_lines, start=1):
            if not raw_line:
                continue
            try:
                records.append(json.loads(raw_line))
            except json.JSONDecodeError as exc:
                evidence_errors.append(f"{path}:{line_number}: invalid JSON: {exc.msg}")
                continue
        session_meta = next(
            (
                record.get("payload", {})
                for record in records
                if record.get("type") == "session_meta"
            ),
            {},
        )
        cwd = session_meta.get("cwd")
        if not isinstance(cwd, str) or Path(cwd).resolve() != workspace:
            continue
        matched_sessions += 1
        for record in records:
            payload = record.get("payload", {})
            if record.get("type") != "response_item":
                continue
            response_item_type = payload.get("type")
            if not isinstance(response_item_type, str):
                response_item_type = "<missing>"
            response_item_types.append(response_item_type)
            if response_item_type != "function_call":
                continue
            name = payload.get("name")
            if not isinstance(name, str):
                evidence_errors.append(
                    f"{path}: function_call response item omitted a string name"
                )
                continue
            function_calls.append(name)
            if name == "spawn_agent":
                arguments = payload.get("arguments")
                if isinstance(arguments, str):
                    spawn_agent_arguments.append(arguments)
                else:
                    spawn_agent_arguments.append(
                        json.dumps(arguments, sort_keys=True, separators=(",", ":"))
                    )
    if matched_sessions == 0:
        evidence_errors.append(
            f"no readable rollout session matched workspace {workspace}"
        )
    return CodexRolloutActivity(
        function_calls=tuple(function_calls),
        spawn_agent_arguments=tuple(spawn_agent_arguments),
        response_item_types=tuple(response_item_types),
        evidence_errors=tuple(evidence_errors),
    )


def load_codex_role_binding(
    rollout_paths: list[Path] | set[Path] | tuple[Path, ...],
    workspace: Path,
) -> CodexRoleBinding:
    workspace = workspace.resolve()
    rollouts = []
    sessions_by_id: dict[str, dict[str, Any]] = {}
    for path in sorted(rollout_paths):
        records = []
        for raw_line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if not raw_line:
                continue
            try:
                records.append(json.loads(raw_line))
            except json.JSONDecodeError:
                continue
        session_meta = next(
            (
                record.get("payload", {})
                for record in records
                if record.get("type") == "session_meta"
            ),
            {},
        )
        session_id = session_meta.get("id")
        if isinstance(session_id, str):
            sessions_by_id[session_id] = session_meta
        rollouts.append((path, session_meta, records))

    candidates = []
    for _path, session_meta, records in rollouts:
        cwd = session_meta.get("cwd")
        if not isinstance(cwd, str) or Path(cwd).resolve() != workspace:
            continue
        if session_meta.get("parent_thread_id") is not None:
            continue
        for record in records:
            payload = record.get("payload", {})
            if (
                record.get("type") != "response_item"
                or payload.get("type") != "function_call"
                or payload.get("name") != "spawn_agent"
            ):
                continue
            spawn_call_id = payload.get("call_id")
            arguments = parse_json_object(payload.get("arguments"))
            child_thread_id = None
            for output_record in records:
                output = output_record.get("payload", {})
                if (
                    output_record.get("type") == "response_item"
                    and output.get("type") == "function_call_output"
                    and output.get("call_id") == spawn_call_id
                ):
                    child_thread_id = parse_json_object(output.get("output")).get(
                        "agent_id"
                    )
                    break
            candidates.append(
                (
                    session_meta.get("id"),
                    arguments.get("agent_type"),
                    spawn_call_id,
                    child_thread_id,
                )
            )

    if len(candidates) != 1:
        return CodexRoleBinding(
            parent_thread_id=None,
            requested_agent_type=None,
            spawn_call_id=None,
            child_thread_id=None,
            child_parent_thread_id=None,
            child_agent_role=None,
            evidence_error=(
                "expected exactly one parent rollout spawn_agent call in the positive "
                f"workspace, found {len(candidates)}"
            ),
        )

    parent_thread_id, requested_agent_type, spawn_call_id, child_thread_id = candidates[
        0
    ]
    child_session = sessions_by_id.get(child_thread_id, {})
    return CodexRoleBinding(
        parent_thread_id=parent_thread_id,
        requested_agent_type=requested_agent_type,
        spawn_call_id=spawn_call_id,
        child_thread_id=child_thread_id,
        child_parent_thread_id=child_session.get("parent_thread_id"),
        child_agent_role=child_session.get("agent_role"),
    )


def codex_role_binding_failures(binding: CodexRoleBinding) -> list[str]:
    failures = []
    if binding.evidence_error:
        failures.append(binding.evidence_error)
    if binding.requested_agent_type != CODEX_CHILD_ROLE_NAME:
        failures.append(
            "parent rollout spawn_agent did not request exact role "
            f"{CODEX_CHILD_ROLE_NAME}"
        )
    if not binding.child_thread_id:
        failures.append("spawn_agent output did not identify a child thread")
    if binding.child_agent_role != CODEX_CHILD_ROLE_NAME:
        failures.append(
            f"child session metadata did not bind exact role {CODEX_CHILD_ROLE_NAME}"
        )
    if (
        not binding.parent_thread_id
        or binding.child_parent_thread_id != binding.parent_thread_id
    ):
        failures.append(
            "child session metadata did not bind to the spawning parent thread"
        )
    return failures


def normalized_acp_tool_title(title: str) -> str:
    return title.replace("_", "").replace(" ", "").lower()


def codex_profile_activity_failures(
    *,
    primary_token: str,
    child_token: str,
    positive_evidence: CodexProbeEvidence,
    negative_evidence: CodexProbeEvidence,
    positive_rollout: CodexRolloutActivity,
    negative_rollout: CodexRolloutActivity,
) -> list[str]:
    failures = []
    positive_tools = tuple(
        normalized_acp_tool_title(title) for title in positive_evidence.tool_call_titles
    )
    expected_acp_tools = Counter({"spawnagent": 1, "wait": 1})
    if Counter(positive_tools) != expected_acp_tools:
        failures.append(
            "positive ACP activity did not contain the exact spawn_agent + wait flow"
        )
    for title in positive_tools:
        if title not in expected_acp_tools:
            failures.append(
                f"positive ACP activity included unexpected ACP tool {title}"
            )
    for method in positive_evidence.server_request_methods:
        failures.append(
            f"positive ACP activity included unexpected server request {method}"
        )

    expected_rollout_functions = Counter({"spawn_agent": 1, "wait_agent": 1})
    if Counter(positive_rollout.function_calls) != expected_rollout_functions:
        failures.append(
            "positive rollout activity did not contain the exact spawn_agent + wait flow"
        )
    for name in positive_rollout.function_calls:
        if name not in expected_rollout_functions:
            failures.append(
                f"positive rollout activity included unexpected rollout function {name}"
            )

    positive_response_item_allowlist = {
        "message",
        "reasoning",
        "function_call",
        "function_call_output",
        # Codex may load the deferred native multi-agent tool definitions before
        # invoking them. This is catalog discovery, not filesystem/process access.
        "tool_search_call",
        "tool_search_output",
    }
    for item_type in positive_rollout.response_item_types:
        if item_type not in positive_response_item_allowlist:
            failures.append(
                "positive rollout activity included unexpected rollout response item "
                f"{item_type}"
            )
    for error in positive_rollout.evidence_errors:
        failures.append(f"positive rollout evidence error: {error}")

    for title in negative_evidence.tool_call_titles:
        failures.append(
            "negative ACP activity included unexpected ACP tool "
            f"{normalized_acp_tool_title(title)}"
        )
    for method in negative_evidence.server_request_methods:
        failures.append(
            f"negative ACP activity included unexpected server request {method}"
        )
    for name in negative_rollout.function_calls:
        failures.append(
            f"negative rollout activity included unexpected rollout function {name}"
        )
    for item_type in negative_rollout.response_item_types:
        if item_type not in {"message", "reasoning"}:
            failures.append(
                "negative rollout activity included unexpected rollout response item "
                f"{item_type}"
            )
    for error in negative_rollout.evidence_errors:
        failures.append(f"negative rollout evidence error: {error}")

    spawn_inputs = (
        *positive_evidence.spawn_agent_raw_inputs,
        *positive_rollout.spawn_agent_arguments,
        *negative_evidence.spawn_agent_raw_inputs,
        *negative_rollout.spawn_agent_arguments,
    )
    if any(
        primary_token in raw_input or child_token in raw_input
        for raw_input in spawn_inputs
    ):
        failures.append("spawn_agent arguments contained a probe token")
    return failures


def read_text_if_present(path: Path) -> Optional[str]:
    if not path.is_file():
        return None
    return path.read_text(encoding="utf-8", errors="replace")


def new_request_id() -> str:
    return str(uuid.uuid4())


def make_request(id_: JsonRpcId, method: str, params: Optional[dict] = None) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_, "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def make_response(
    id_: JsonRpcId, result: Optional[dict] = None, error: Optional[dict] = None
) -> dict:
    msg: dict[str, Any] = {"jsonrpc": "2.0", "id": id_}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    return msg


def png_chunk(kind: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    )


def synthesize_probe_png() -> bytes:
    width = 64
    height = 64
    rows = []
    for y in range(height):
        row = bytearray()
        for x in range(width):
            border = x in (0, width - 1) or y in (0, height - 1)
            cross = 28 <= x <= 35 or 28 <= y <= 35
            if border:
                row.extend((0, 0, 0))
            elif cross:
                row.extend((255, 255, 255))
            else:
                row.extend((255, 0, 0))
        rows.append(b"\x00" + bytes(row))

    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    idat = zlib.compress(b"".join(rows), level=9)
    return (
        signature
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", idat)
        + png_chunk(b"IEND", b"")
    )


def prompt_blocks(prompt: str, with_image: bool) -> list[dict]:
    blocks = [{"type": "text", "text": prompt}]
    if with_image:
        png = synthesize_probe_png()
        blocks.append(
            {
                "type": "image",
                "data": base64.b64encode(png).decode("ascii"),
                "mimeType": "image/png",
            }
        )
    return blocks


def probe_client_capabilities() -> dict:
    return {
        "fs": {"readTextFile": True, "writeTextFile": False},
        "terminal": True,
    }


def profile_probe_client_capabilities() -> dict:
    return {}


def response_error_message(response: Optional[dict]) -> Optional[str]:
    if not response or "error" not in response:
        return None
    error = response.get("error") or {}
    code = error.get("code")
    message = error.get("message")
    if code is None and message is None:
        return json.dumps(error, sort_keys=True)
    if code is None:
        return str(message)
    if message is None:
        return f"code={code}"
    return f"code={code} {message}"


class ProbeRequestError(Exception):
    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


class TerminalState:
    def __init__(self, proc: subprocess.Popen, output_byte_limit: int):
        assert proc.stdout is not None and proc.stderr is not None
        self.proc = proc
        self.output_byte_limit = output_byte_limit
        self.output = ""
        self.truncated = False
        self._lock = threading.Lock()
        self._stdout = threading.Thread(
            target=self._read_stream, args=(proc.stdout,), daemon=True
        )
        self._stderr = threading.Thread(
            target=self._read_stream, args=(proc.stderr,), daemon=True
        )
        self._stdout.start()
        self._stderr.start()

    def _read_stream(self, stream: Any) -> None:
        while True:
            chunk = stream.read(4096)
            if not chunk:
                return
            self.append(chunk)

    def append(self, text: str) -> None:
        with self._lock:
            self.output += text
            if len(self.output) > self.output_byte_limit:
                self.output = self.output[-self.output_byte_limit :]
                self.truncated = True

    def exit_status(self) -> Optional[dict]:
        code = self.proc.poll()
        if code is None:
            return None
        if code < 0:
            try:
                sig = signal.Signals(-code).name
            except ValueError:
                sig = str(-code)
            return {"signal": sig}
        return {"exitCode": code}

    def output_response(self) -> dict:
        with self._lock:
            result = {"output": self.output, "truncated": self.truncated}
        status = self.exit_status()
        if status is not None:
            result["exitStatus"] = status
        return result

    def wait_response(self) -> dict:
        self.proc.wait()
        self.drain_reader_threads()
        return self.exit_status() or {}

    def drain_reader_threads(self) -> None:
        self._stdout.join(timeout=TERMINAL_DRAIN_TIMEOUT)
        self._stderr.join(timeout=TERMINAL_DRAIN_TIMEOUT)

    def kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()


def optional_int(value: Any) -> Optional[int]:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def slice_text(text: str, line: Optional[int], limit: Optional[int]) -> str:
    lines = text.splitlines()
    start = max((line or 1) - 1, 0)
    selected = lines[start:]
    if limit is not None:
        selected = selected[: max(limit, 0)]
    return "\n".join(selected)


class AcpRequestHandlers:
    """Client-side handlers for requests that an ACP agent sends to SPUR."""

    def __init__(self, cwd: Path):
        self.cwd = cwd
        self.terminals: dict[str, TerminalState] = {}

    def resolve_path(self, raw_path: str) -> Path:
        path = Path(raw_path).expanduser()
        if path.is_absolute():
            return path
        return self.cwd.joinpath(path)

    def handle(self, req: dict) -> dict:
        method = req.get("method", "")
        params = req.get("params", {}) or {}
        if method == "fs/read_text_file":
            return self.read_text_file(params)
        if method == "terminal/create":
            return self.terminal_create(params)
        if method == "terminal/output":
            return self.get_terminal(params).output_response()
        if method == "terminal/wait_for_exit":
            return self.get_terminal(params).wait_response()
        if method == "terminal/kill":
            self.get_terminal(params).kill()
            return {}
        if method == "terminal/release":
            terminal_id = params.get("terminalId", params.get("terminal_id"))
            self.get_terminal(params).kill()
            self.terminals.pop(terminal_id, None)
            return {}
        if method == "fs/write_text_file":
            raise ProbeRequestError(
                -32603, "fs/write_text_file denied by native subagent probe"
            )
        raise ProbeRequestError(-32601, f"probe does not implement {method}")

    def read_text_file(self, params: dict) -> dict:
        raw_path = params.get("path")
        if not isinstance(raw_path, str) or not raw_path:
            raise ProbeRequestError(-32602, "fs/read_text_file requires path")
        path = self.resolve_path(raw_path)
        try:
            content = path.read_text()
        except Exception as exc:
            raise ProbeRequestError(-32603, f"Failed to read {path}: {exc}") from exc
        return {
            "content": slice_text(
                content,
                optional_int(params.get("line")),
                optional_int(params.get("limit")),
            )
        }

    def terminal_create(self, params: dict) -> dict:
        command = params.get("command")
        if not isinstance(command, str) or not command:
            raise ProbeRequestError(-32602, "terminal/create requires command")
        args = params.get("args") or []
        if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
            raise ProbeRequestError(-32602, "terminal/create args must be strings")
        cwd = (
            self.resolve_path(params["cwd"])
            if isinstance(params.get("cwd"), str)
            else self.cwd
        )
        limit = optional_int(
            params.get("outputByteLimit", params.get("output_byte_limit"))
        )
        if limit is None:
            limit = 1024 * 1024
        try:
            proc = subprocess.Popen(
                [command, *args],
                cwd=str(cwd),
                env=os.environ.copy(),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                errors="replace",
                bufsize=1,
            )
        except Exception as exc:
            raise ProbeRequestError(
                -32603, f"Failed to spawn {command!r}: {exc}"
            ) from exc
        terminal_id = str(uuid.uuid4())
        self.terminals[terminal_id] = TerminalState(proc, limit)
        return {"terminalId": terminal_id}

    def get_terminal(self, params: dict) -> TerminalState:
        terminal_id = params.get("terminalId", params.get("terminal_id"))
        if not isinstance(terminal_id, str) or terminal_id not in self.terminals:
            raise ProbeRequestError(-32602, f"Terminal {terminal_id!r} not found")
        return self.terminals[terminal_id]

    def close(self) -> None:
        for terminal in self.terminals.values():
            terminal.kill()


class ProfileProbeRequestHandlers:
    """Deny all agent-to-client requests during the profile isolation probe."""

    def __init__(self, cwd: Path):
        self.cwd = cwd

    def handle(self, req: dict) -> dict:
        method = req.get("method", "")
        raise ProbeRequestError(
            -32601,
            f"{method or 'unknown request'} denied by restricted Codex profile probe",
        )

    def close(self) -> None:
        pass


class AcpClient:
    def __init__(self, proc: subprocess.Popen, raw_log_path: Path, quiet: bool):
        assert (
            proc.stdin is not None
            and proc.stdout is not None
            and proc.stderr is not None
        )
        self.proc = proc
        self.stdin = proc.stdin
        self.stdout = proc.stdout
        self.stderr = proc.stderr
        self.responses: dict[JsonRpcId, dict] = {}
        self.notifications: list[dict] = []
        self.server_requests: list[dict] = []
        self.raw_log = open_private_text(raw_log_path)
        self.quiet = quiet
        self._log_lock = threading.Lock()
        self._lock = threading.Lock()
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._stderr = threading.Thread(target=self._stderr_loop, daemon=True)
        self._reader.start()
        self._stderr.start()

    def _log(self, direction: str, obj: dict) -> None:
        with self._log_lock:
            self.raw_log.write(
                json.dumps({"ts": utc_now_iso(), "dir": direction, "msg": obj}) + "\n"
            )
            self.raw_log.flush()

    def _read_loop(self) -> None:
        for raw in self.stdout:
            line = raw.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                if not self.quiet:
                    print(f"[reader] non-JSON: {line[:200]}", file=sys.stderr)
                continue
            self._log("recv", obj)
            with self._lock:
                if "method" in obj and "id" in obj:
                    self.server_requests.append(obj)
                elif "method" in obj:
                    self.notifications.append(obj)
                elif "id" in obj:
                    self.responses[obj["id"]] = obj

    def _stderr_loop(self) -> None:
        for raw in self.stderr:
            line = raw.rstrip()
            self._log("stderr", {"text": line})
            if line and not self.quiet:
                print(f"[stderr] {line}", file=sys.stderr)

    def send(self, obj: dict) -> None:
        line = json.dumps(obj, separators=(",", ":"))
        self.stdin.write(line + "\n")
        self.stdin.flush()
        self._log("send", obj)
        if not self.quiet:
            print(f"[send] {line[:240]}")

    def wait_response(self, id_: JsonRpcId, timeout: float) -> Optional[dict]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self._lock:
                if id_ in self.responses:
                    return self.responses.pop(id_)
            time.sleep(0.02)
        return None

    def drain_notifications(self) -> list[dict]:
        with self._lock:
            items = self.notifications[:]
            self.notifications.clear()
            return items

    def drain_server_requests(self) -> list[dict]:
        with self._lock:
            items = self.server_requests[:]
            self.server_requests.clear()
            return items

    def pop_response(self, id_: JsonRpcId) -> Optional[dict]:
        with self._lock:
            return self.responses.pop(id_, None)

    def close(self) -> None:
        try:
            self.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        self._reader.join(timeout=1)
        self._stderr.join(timeout=1)
        self.raw_log.close()


def choose_permission_option(options: list[dict]) -> str:
    for pref in (
        "allow_always",
        "allow_once",
        "allow",
        "approve_for_session",
        "approve",
    ):
        for opt in options:
            if opt.get("kind") == pref or opt.get("optionId") == pref:
                return opt.get("optionId") or opt.get("id") or pref
    if options:
        return options[0].get("optionId") or options[0].get("id") or "approve"
    return "approve"


def extract_tool_snapshot(notification: dict) -> Optional[dict]:
    update = notification.get("params", {}).get("update", {})
    variant = update.get("sessionUpdate")
    if variant not in {"tool_call", "tool_call_update"}:
        return None
    return {
        "variant": variant,
        "toolCallId": update.get("toolCallId"),
        "title": update.get("title"),
        "kind": update.get("kind"),
        "status": update.get("status"),
        "rawInput": update.get("rawInput"),
        "rawOutput": update.get("rawOutput"),
        "_meta": update.get("_meta"),
        "content": update.get("content"),
    }


def extract_text_content(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "".join(extract_text_content(item) for item in value)
    if not isinstance(value, dict):
        return ""

    text = value.get("text")
    if isinstance(text, str):
        return text

    content = value.get("content")
    if content is not None:
        return extract_text_content(content)

    return ""


def extract_agent_message_text(notification: dict) -> str:
    update = notification.get("params", {}).get("update", {})
    if update.get("sessionUpdate") != "agent_message_chunk":
        return ""
    return extract_text_content(update.get("content"))


def load_codex_probe_evidence(
    raw_log_path: Path,
    *,
    child_thread_id: Optional[str] = None,
) -> CodexProbeEvidence:
    primary_chunks = []
    stderr_lines = []
    child_results = []
    spawn_agent_call_ids = set()
    tool_calls: dict[tuple[str, str], str] = {}
    server_request_methods = []
    spawn_agent_raw_inputs = []
    spawn_seen = False
    for raw_line in raw_log_path.read_text(encoding="utf-8").splitlines():
        if not raw_line:
            continue
        record = json.loads(raw_line)
        if record.get("dir") == "stderr":
            stderr_lines.append(str(record.get("msg", {}).get("text", "")))
            continue
        if record.get("dir") != "recv":
            continue
        notification = record.get("msg", {})
        method = notification.get("method")
        if "id" in notification and isinstance(method, str):
            server_request_methods.append(method)
        if text := extract_agent_message_text(notification):
            primary_chunks.append(text)
        snapshot = extract_tool_snapshot(notification)
        if snapshot is None:
            continue
        title = str(snapshot.get("title") or "").replace("_", "").lower()
        original_title = str(snapshot.get("title") or "")
        tool_call_id = str(snapshot.get("toolCallId") or "<missing-tool-call-id>")
        tool_calls.setdefault((tool_call_id, original_title), original_title)
        if title == "spawnagent":
            spawn_seen = True
            call_id = snapshot.get("toolCallId")
            spawn_agent_call_ids.add(str(call_id or "<missing-tool-call-id>"))
            spawn_agent_raw_inputs.append(
                json.dumps(snapshot.get("rawInput"), sort_keys=True)
            )
        raw_input = snapshot.get("rawInput")
        if spawn_seen and title == "wait" and isinstance(raw_input, dict):
            agent_states = raw_input.get("agentsStates") or {}
            if isinstance(agent_states, dict):
                for thread_id, state in agent_states.items():
                    if child_thread_id is not None and thread_id != child_thread_id:
                        continue
                    if not isinstance(state, dict):
                        continue
                    message = state.get("message")
                    if (
                        isinstance(message, str)
                        and message
                        and message not in child_results
                    ):
                        child_results.append(message)
    return CodexProbeEvidence(
        primary_response="".join(primary_chunks),
        raw_child_results=tuple(child_results),
        stderr="\n".join(stderr_lines),
        spawn_agent_call_ids=tuple(sorted(spawn_agent_call_ids)),
        tool_call_titles=tuple(tool_calls.values()),
        server_request_methods=tuple(server_request_methods),
        spawn_agent_raw_inputs=tuple(spawn_agent_raw_inputs),
    )


def verdict_excerpt(text: str) -> str:
    return " ".join(text.split())[:200]


def has_subagent_clue(value: Any) -> bool:
    text = json.dumps(value, sort_keys=True).lower()
    return any(word in text for word in SUBAGENT_CLUE_WORDS)


def agent_command(args: argparse.Namespace) -> list[str]:
    if args.agent == "kimi":
        cmd = ["kimi"]
        if args.yolo:
            cmd.append("-y")
        if args.afk:
            cmd.append("--afk")
        cmd.append("acp")
        return cmd

    if args.agent == "gemini":
        cmd = ["gemini", "--acp"]
        if args.gemini_skip_trust:
            cmd.append("--skip-trust")
        if args.gemini_no_sandbox:
            cmd.append("--no-sandbox")
        cmd.append("-y")
        if args.gemini_model:
            cmd.extend(["-m", args.gemini_model])
        return cmd

    if args.agent == "claude-code":
        return ["npx", "--yes", args.claude_code_package]

    cmd = ["npx", "--yes", args.codex_package]
    for override in args.codex_config:
        cmd.extend(["-c", override])
    return cmd


def record_notification(
    notif: dict,
    notification_counts: Counter[str],
    tool_snapshots: list[dict],
    subagent_clues: list[dict],
) -> None:
    update = notif.get("params", {}).get("update", {})
    variant = update.get("sessionUpdate", notif.get("method", "unknown"))
    notification_counts[variant] += 1
    snap = extract_tool_snapshot(notif)
    if snap:
        tool_snapshots.append(snap)
    if has_subagent_clue(update):
        subagent_clues.append(update)


def record_prompt_notification(
    notif: dict,
    notification_counts: Counter[str],
    tool_snapshots: list[dict],
    subagent_clues: list[dict],
    agent_text_chunks: list[str],
) -> None:
    record_notification(notif, notification_counts, tool_snapshots, subagent_clues)
    if text := extract_agent_message_text(notif):
        agent_text_chunks.append(text)


def codex_profile_inner_command(
    args: argparse.Namespace,
    prompt: str,
    raw_log_path: Path,
) -> list[str]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--agent",
        "codex",
        "--codex-package",
        args.codex_package,
        "--prompt",
        prompt,
        "--timeout",
        str(args.timeout),
        "--init-timeout",
        str(args.init_timeout),
        "--session-timeout",
        str(args.session_timeout),
        "--out",
        str(raw_log_path),
        "--quiet",
        "--restricted-profile-probe",
    ]
    if args.authenticate:
        command.append("--authenticate")
    return command


def run_captured_probe(
    *,
    command: list[str],
    cwd: Path,
    env: dict[str, str],
    stdout_path: Path,
    stderr_path: Path,
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    ensure_private_directory(stdout_path.parent)
    if stderr_path.parent != stdout_path.parent:
        ensure_private_directory(stderr_path.parent)
    write_private_text(stdout_path, completed.stdout)
    write_private_text(stderr_path, completed.stderr)
    return completed


def run_codex_profile_probe(args: argparse.Namespace) -> int:
    probe_env = normalized_codex_environment(os.environ.copy(), cwd=Path.cwd())
    version_command = codex_version_command(args.codex_package)
    print(f"VERSION_COMMAND={shlex.join(version_command)}")
    if codex_path := probe_env.get("CODEX_PATH"):
        print(f"VERSION_ENV=CODEX_PATH={codex_path}")
    try:
        versions = resolve_codex_versions(args.codex_package, env=probe_env)
    except RuntimeError as exc:
        print(f"[BLOCKED] Codex version discovery failed: {exc}", file=sys.stderr)
        return 2

    label = codex_evidence_label(versions.adapter_version, versions.codex_cli_version)
    print(f"adapter_version={versions.adapter_version}")
    print(f"codex_package_version={versions.codex_package_version}")
    print(f"codex_cli_version={versions.codex_cli_version}")
    print(f"codex_cli_path={versions.codex_cli_path}")
    print(f"codex_cli_output={versions.codex_cli_output}")
    print(f"evidence_label={label}")

    base_out = args.profile_probe_out_dir
    if base_out is None:
        suffix = uuid.uuid4().hex[:8]
        base_out = (
            Path(".spur/logs") / f"codex-profile-probe-{utc_now_compact()}-{suffix}"
        )
    base_out = base_out.resolve()
    ensure_private_directory(base_out)
    positive_workspace = base_out / "positive-workspace"
    negative_workspace = base_out / "negative-workspace"
    try:
        initialize_codex_probe_workspace(positive_workspace)
        initialize_codex_probe_workspace(negative_workspace)
    except RuntimeError as exc:
        print(f"[BLOCKED] Codex probe workspace setup failed: {exc}", file=sys.stderr)
        protect_artifact_tree(base_out)
        return 2

    primary_token = f"SPUR_PRIMARY_PROFILE_{uuid.uuid4().hex}"
    child_token = f"SPUR_NATIVE_CHILD_{uuid.uuid4().hex}"
    fixture = create_codex_profile_fixture(
        positive_workspace,
        primary_token=primary_token,
        child_token=child_token,
    )
    primary_sources = find_token_sources(positive_workspace, primary_token)
    child_sources = find_token_sources(positive_workspace, child_token)
    negative_primary_sources = find_token_sources(negative_workspace, primary_token)
    negative_child_sources = find_token_sources(negative_workspace, child_token)
    print("primary_token_sources=" + ",".join(str(path) for path in primary_sources))
    print("child_token_sources=" + ",".join(str(path) for path in child_sources))
    source_failures = []
    if primary_sources != (fixture.primary_profile_path,):
        source_failures.append(
            "primary token was not authored only in the selected profile body"
        )
    if child_sources != (fixture.child_role_path,):
        source_failures.append(
            "child token was not authored only in the generated native role TOML"
        )
    if negative_primary_sources or negative_child_sources:
        source_failures.append("no-profile workspace contained an authored probe token")
    if source_failures:
        for failure in source_failures:
            print(f"[FAIL] {failure}", file=sys.stderr)
        print(f"artifacts={base_out}")
        protect_artifact_tree(base_out)
        return 1

    positive_log = base_out / "positive.jsonl"
    positive_command = codex_profile_inner_command(
        args, fixture.positive_prompt, positive_log
    )
    positive_app_server_log_path = prepare_app_server_log(
        base_out / "positive-app-server-logs"
    )
    positive_env = probe_env.copy()
    positive_env["CODEX_CONFIG"] = json.dumps(
        codex_profile_probe_config(fixture.primary_body),
        separators=(",", ":"),
    )
    positive_env["APP_SERVER_LOGS"] = str(positive_app_server_log_path.parent)
    session_root = codex_sessions_root(positive_env)
    positive_rollouts_before = codex_rollout_paths(session_root)
    print(f"POSITIVE_COMMAND={shlex.join(positive_command)}")
    print(
        "POSITIVE_ENV=CODEX_CONFIG derived only from "
        f"{fixture.primary_profile_path}; "
        f"APP_SERVER_LOGS={positive_app_server_log_path.parent}"
    )
    positive = run_captured_probe(
        command=positive_command,
        cwd=positive_workspace,
        env=positive_env,
        stdout_path=base_out / "positive.stdout",
        stderr_path=base_out / "positive.stderr",
    )
    print(f"positive_exit={positive.returncode}")
    if positive.returncode != 0:
        print(
            "[BLOCKED] positive live probe failed: "
            f"command={shlex.join(positive_command)} exit={positive.returncode} "
            f"stderr={positive.stderr.strip()!r}",
            file=sys.stderr,
        )
        print(f"artifacts={base_out}")
        protect_artifact_tree(base_out)
        return 2

    positive_rollouts = codex_rollout_paths(session_root) - positive_rollouts_before
    role_binding = load_codex_role_binding(positive_rollouts, positive_workspace)
    positive_rollout_activity = load_codex_rollout_activity(
        positive_rollouts,
        positive_workspace,
    )
    positive_evidence = load_codex_probe_evidence(
        positive_log,
        child_thread_id=role_binding.child_thread_id,
    )
    positive_app_server_log = read_text_if_present(positive_app_server_log_path)
    negative_log = base_out / "negative.jsonl"
    negative_command = codex_profile_inner_command(
        args, fixture.negative_prompt, negative_log
    )
    negative_app_server_log_path = prepare_app_server_log(
        base_out / "negative-app-server-logs"
    )
    negative_env = probe_env.copy()
    negative_env["CODEX_CONFIG"] = json.dumps(
        codex_profile_probe_config(None),
        separators=(",", ":"),
    )
    negative_env["APP_SERVER_LOGS"] = str(negative_app_server_log_path.parent)
    negative_rollouts_before = codex_rollout_paths(session_root)
    print(f"NEGATIVE_COMMAND={shlex.join(negative_command)}")
    print(
        "NEGATIVE_ENV=CODEX_CONFIG has only stable multi-agent V1 feature flags; "
        "no profile or child role files generated; "
        f"APP_SERVER_LOGS={negative_app_server_log_path.parent}"
    )
    negative = run_captured_probe(
        command=negative_command,
        cwd=negative_workspace,
        env=negative_env,
        stdout_path=base_out / "negative.stdout",
        stderr_path=base_out / "negative.stderr",
    )
    print(f"negative_exit={negative.returncode}")
    if negative.returncode != 0:
        print(
            "[BLOCKED] negative live probe failed: "
            f"command={shlex.join(negative_command)} exit={negative.returncode} "
            f"stderr={negative.stderr.strip()!r}",
            file=sys.stderr,
        )
        print(f"artifacts={base_out}")
        protect_artifact_tree(base_out)
        return 2

    negative_rollouts = codex_rollout_paths(session_root) - negative_rollouts_before
    negative_rollout_activity = load_codex_rollout_activity(
        negative_rollouts,
        negative_workspace,
    )
    negative_evidence = load_codex_probe_evidence(negative_log)
    negative_app_server_log = read_text_if_present(negative_app_server_log_path)
    failures = codex_profile_probe_failures(
        primary_token=primary_token,
        child_token=child_token,
        primary_response=positive_evidence.primary_response,
        raw_child_results=positive_evidence.raw_child_results,
        positive_stderr=positive_evidence.stderr,
        negative_response=negative_evidence.primary_response,
        negative_raw_child_results=negative_evidence.raw_child_results,
        negative_stderr=negative_evidence.stderr,
        negative_spawn_agent_call_ids=negative_evidence.spawn_agent_call_ids,
    )
    positive_adapter_stderr = "\n".join(
        part for part in (positive_evidence.stderr, positive.stderr) if part
    )
    negative_adapter_stderr = "\n".join(
        part for part in (negative_evidence.stderr, negative.stderr) if part
    )
    failures.extend(
        codex_profile_warning_failures(
            run_name="positive",
            adapter_stderr=positive_adapter_stderr,
            app_server_log=positive_app_server_log,
        )
    )
    failures.extend(codex_role_binding_failures(role_binding))
    activity_failures = codex_profile_activity_failures(
        primary_token=primary_token,
        child_token=child_token,
        positive_evidence=positive_evidence,
        negative_evidence=negative_evidence,
        positive_rollout=positive_rollout_activity,
        negative_rollout=negative_rollout_activity,
    )
    failures.extend(activity_failures)
    failures.extend(
        codex_profile_warning_failures(
            run_name="negative",
            adapter_stderr=negative_adapter_stderr,
            app_server_log=negative_app_server_log,
        )
    )
    print(
        f"primary_token_in_primary_response="
        f"{str(primary_token in positive_evidence.primary_response).lower()}"
    )
    print(
        "child_token_in_raw_spawn_agent_result="
        f"{str(any(child_token in item for item in positive_evidence.raw_child_results)).lower()}"
    )
    print(
        "exact_child_role_requested="
        f"{str(role_binding.requested_agent_type == CODEX_CHILD_ROLE_NAME).lower()}"
    )
    print(
        "exact_child_role_loaded="
        f"{str(role_binding.child_agent_role == CODEX_CHILD_ROLE_NAME).lower()}"
    )
    print(f"parent_thread_id={role_binding.parent_thread_id or ''}")
    print(f"child_thread_id={role_binding.child_thread_id or ''}")
    print(
        "malformed_role_warning="
        f"{str(MALFORMED_ROLE_WARNING in positive_adapter_stderr or MALFORMED_ROLE_WARNING in (positive_app_server_log or '') or MALFORMED_ROLE_WARNING in negative_adapter_stderr or MALFORMED_ROLE_WARNING in (negative_app_server_log or '')).lower()}"
    )
    negative_control_passed = (
        primary_token not in negative_evidence.primary_response
        and child_token not in negative_evidence.primary_response
        and not any(
            primary_token in result or child_token in result
            for result in negative_evidence.raw_child_results
        )
        and not negative_evidence.raw_child_results
        and not negative_evidence.spawn_agent_call_ids
        and negative_evidence.primary_response.strip() == "NO_PROFILE_ACTIVE"
        and MALFORMED_ROLE_WARNING not in negative_adapter_stderr
        and MALFORMED_ROLE_WARNING not in (negative_app_server_log or "")
    )
    print(f"no_profile_negative_control={str(negative_control_passed).lower()}")
    print(f"no_profile_spawn_agent_calls={len(negative_evidence.spawn_agent_call_ids)}")
    print("restricted_client_capabilities=true")
    print(f"unexpected_profile_activity={str(bool(activity_failures)).lower()}")
    print(f"artifacts={base_out}")
    protect_artifact_tree(base_out)
    if failures:
        for failure in failures:
            print(f"[FAIL] {failure}", file=sys.stderr)
        return 1
    print(f"PROFILE_PROBE_PASS label={label}")
    return 0


def run_probe(args: argparse.Namespace) -> int:
    out_path = args.out
    if out_path is None:
        Path(".spur/logs").mkdir(parents=True, exist_ok=True)
        out_path = Path(
            f".spur/logs/probe-{args.agent}-native-subagents-{utc_now_compact()}.jsonl"
        )

    cmd = agent_command(args)
    print("=" * 72)
    print(f"ACP NATIVE SUBAGENT PROBE  agent={args.agent}  cmd={' '.join(cmd)}")
    print(f"raw_log={out_path}")
    print("=" * 72)

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        cwd=os.getcwd(),
    )
    client = AcpClient(proc, out_path, args.quiet)
    restricted_profile_probe = args.restricted_profile_probe
    handlers = (
        ProfileProbeRequestHandlers(Path(os.getcwd()))
        if restricted_profile_probe
        else AcpRequestHandlers(Path(os.getcwd()))
    )

    notification_counts: Counter[str] = Counter()
    tool_snapshots: list[dict] = []
    subagent_clues: list[dict] = []
    agent_text_chunks: list[str] = []
    server_request_counts: Counter[str] = Counter()
    prompt_resp: Optional[dict] = None
    exit_code = 0
    effective_prompt = (
        IMAGE_PROBE_PROMPT
        if args.with_image and args.prompt == DEFAULT_PROMPT
        else args.prompt
    )
    prompt_payload = prompt_blocks(effective_prompt, args.with_image)
    if args.with_image:
        image_block = prompt_payload[-1]
        print(
            "image_probe="
            f"png_bytes={len(synthesize_probe_png())} "
            f"base64_chars={len(image_block['data'])}"
        )

    try:
        init_id = new_request_id()
        client.send(
            make_request(
                init_id,
                "initialize",
                {
                    "protocolVersion": 1,
                    "clientInfo": {
                        "name": "spur-native-subagent-probe",
                        "version": "0.1.0",
                    },
                    "clientCapabilities": (
                        profile_probe_client_capabilities()
                        if restricted_profile_probe
                        else probe_client_capabilities()
                    ),
                },
            )
        )
        init_resp = client.wait_response(init_id, args.init_timeout)
        if init_resp is None:
            print("[FAIL] initialize timed out", file=sys.stderr)
            return 1
        if err := response_error_message(init_resp):
            print(f"[FAIL] initialize error: {err}", file=sys.stderr)
            return 1
        print(
            f"[recv] initialize result: {json.dumps(init_resp.get('result', {}))[:500]}"
        )

        if args.authenticate:
            auth_methods = init_resp.get("result", {}).get("authMethods") or []
            if auth_methods:
                method_id = auth_methods[0].get("id")
                auth_id = new_request_id()
                client.send(
                    make_request(auth_id, "authenticate", {"methodId": method_id})
                )
                auth_resp = client.wait_response(auth_id, args.init_timeout)
                if err := response_error_message(auth_resp):
                    print(f"[FAIL] authenticate error: {err}", file=sys.stderr)
                    return 1
                if not args.quiet:
                    print(
                        f"[recv] authenticate response: {json.dumps(auth_resp or {})[:300]}"
                    )

        session_req_id = str(uuid.uuid4())
        session_new_id = new_request_id()
        client.send(
            make_request(
                session_new_id,
                "session/new",
                {
                    "sessionId": session_req_id,
                    "cwd": os.getcwd(),
                    "mcpServers": [],
                },
            )
        )
        session_resp = client.wait_response(session_new_id, args.session_timeout)
        if session_resp is None:
            print("[FAIL] session/new timed out", file=sys.stderr)
            return 1
        if err := response_error_message(session_resp):
            print(f"[FAIL] session/new error: {err}", file=sys.stderr)
            return 1
        session_id = session_resp.get("result", {}).get("sessionId", session_req_id)
        print(
            f"[recv] session/new result: {json.dumps(session_resp.get('result', {}))[:500]}"
        )

        time.sleep(0.5)
        for notif in client.drain_notifications():
            record_notification(
                notif, notification_counts, tool_snapshots, subagent_clues
            )

        prompt_id = new_request_id()
        client.send(
            make_request(
                prompt_id,
                "session/prompt",
                {
                    "sessionId": session_id,
                    "prompt": prompt_payload,
                },
            )
        )

        start = time.time()
        while time.time() - start < args.timeout:
            for req in client.drain_server_requests():
                method = req.get("method", "")
                server_request_counts[method] += 1
                rid = req.get("id")
                if rid is None:
                    continue
                if (
                    method == "session/request_permission"
                    and not restricted_profile_probe
                ):
                    options = req.get("params", {}).get("options", []) or []
                    chosen = choose_permission_option(options)
                    if not args.quiet:
                        print(f"[req#{rid}] permission -> {chosen}")
                    client.send(
                        make_response(
                            rid,
                            {"outcome": {"outcome": "selected", "optionId": chosen}},
                        )
                    )
                    continue
                try:
                    result = handlers.handle(req)
                except ProbeRequestError as exc:
                    if not args.quiet:
                        print(f"[req#{rid}] {method} -> error {exc.message}")
                    client.send(
                        make_response(
                            rid, error={"code": exc.code, "message": exc.message}
                        )
                    )
                else:
                    if not args.quiet:
                        print(f"[req#{rid}] {method} -> ok")
                    client.send(make_response(rid, result=result))

            for notif in client.drain_notifications():
                record_prompt_notification(
                    notif,
                    notification_counts,
                    tool_snapshots,
                    subagent_clues,
                    agent_text_chunks,
                )
                snap = extract_tool_snapshot(notif)
                if snap and not args.quiet:
                    print(
                        "[tool] "
                        f"{snap['variant']} id={snap['toolCallId']!r} "
                        f"title={snap['title']!r} status={snap['status']!r} "
                        f"meta={snap['_meta']!r}"
                    )

            prompt_resp = client.pop_response(prompt_id)
            if prompt_resp is not None:
                break
            time.sleep(0.05)

        if prompt_resp is not None:
            drain_deadline = time.time() + TERMINAL_DRAIN_TIMEOUT
            while time.time() < drain_deadline:
                drained = client.drain_notifications()
                if not drained:
                    time.sleep(0.05)
                    continue
                for notif in drained:
                    record_prompt_notification(
                        notif,
                        notification_counts,
                        tool_snapshots,
                        subagent_clues,
                        agent_text_chunks,
                    )

        if prompt_resp is None:
            print(
                f"[WARN] session/prompt did not complete within {args.timeout}s",
                file=sys.stderr,
            )
            exit_code = 1
        elif err := response_error_message(prompt_resp):
            print(f"[FAIL] session/prompt error: {err}", file=sys.stderr)
            exit_code = 1
        else:
            print(
                f"[recv] session/prompt result: {json.dumps(prompt_resp.get('result', {}))[:500]}"
            )
    finally:
        handlers.close()
        client.close()

    print("\n" + "=" * 72)
    print("REPORT")
    print("=" * 72)
    print("session/update variants:")
    for variant, count in notification_counts.most_common():
        print(f"  - {variant:30s} x{count}")
    print("server requests:")
    if server_request_counts:
        for method, count in server_request_counts.most_common():
            print(f"  - {method:30s} x{count}")
    else:
        print("  (none)")
    print("tool snapshots:")
    if not tool_snapshots:
        print("  (none)")
    for snap in tool_snapshots:
        print(
            "  - "
            f"{snap['variant']:16s} "
            f"id={snap['toolCallId']!r} "
            f"title={snap['title']!r} "
            f"kind={snap['kind']!r} "
            f"status={snap['status']!r} "
            f"meta={json.dumps(snap['_meta'], sort_keys=True) if snap['_meta'] else None}"
        )
    print("subagent clue updates:")
    if not subagent_clues:
        print("  (none)")
    for update in subagent_clues[:12]:
        compact = json.dumps(update, sort_keys=True)
        print(f"  - {compact[:600]}")
    if len(subagent_clues) > 12:
        print(f"  ... {len(subagent_clues) - 12} more in raw log")
    if args.with_image:
        prompt_err = response_error_message(prompt_resp)
        if prompt_resp is None:
            prompt_status = f"timeout after {args.timeout}s"
        elif prompt_err:
            prompt_status = prompt_err
        else:
            prompt_status = "ok"
        agent_text = "".join(agent_text_chunks)
        said_no_image = "NO_IMAGE_RECEIVED" in agent_text
        print(
            "IMAGE_VERDICT: "
            f"agent={args.agent} "
            f"session_prompt={prompt_status} "
            f"agent_said_NO_IMAGE_RECEIVED={str(said_no_image).lower()} "
            f"excerpt={verdict_excerpt(agent_text)!r}"
        )
    print(f"raw_log={out_path}")
    return exit_code


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--agent",
        choices=["codex", "kimi", "gemini", "claude-code"],
        default="kimi",
    )
    parser.add_argument("--prompt", default=DEFAULT_PROMPT)
    parser.add_argument(
        "--with-image",
        action="store_true",
        help="Send a small synthesized PNG image block after the prompt text.",
    )
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--init-timeout", type=float, default=30.0)
    parser.add_argument("--session-timeout", type=float, default=45.0)
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--yolo", "-y", action="store_true", help="Kimi only: pass -y")
    parser.add_argument("--afk", action="store_true", help="Kimi only: pass --afk")
    parser.add_argument(
        "--authenticate",
        action="store_true",
        help="Authenticate with the first advertised auth method before session/new.",
    )
    parser.add_argument(
        "--codex-package",
        default=DEFAULT_CODEX_PACKAGE,
        help="Codex ACP npm package passed to npx.",
    )
    parser.add_argument(
        "--codex-profile-probe",
        action="store_true",
        help=(
            "Opt in to the focused Codex primary-profile, native-child-role, "
            "malformed-warning, and no-profile control probe."
        ),
    )
    parser.add_argument(
        "--profile-probe-out-dir",
        type=Path,
        default=None,
        help="Artifact directory for --codex-profile-probe (default: .spur/logs/<timestamp>).",
    )
    parser.add_argument(
        "--restricted-profile-probe",
        action="store_true",
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--claude-code-package",
        default="@agentclientprotocol/claude-agent-acp@0.30.0",
        help="claude-code ACP npm package passed to npx.",
    )
    parser.add_argument(
        "--codex-config",
        action="append",
        default=[],
        help="Codex ACP -c key=value override; repeatable.",
    )
    parser.add_argument(
        "--gemini-model",
        default="deep-thinker",
        help="Gemini model passed with -m; empty string omits -m.",
    )
    parser.add_argument(
        "--gemini-skip-trust",
        action="store_true",
        help="Gemini only: pass --skip-trust to bypass workspace trust setup.",
    )
    parser.add_argument(
        "--gemini-no-sandbox",
        action="store_true",
        help="Gemini only: pass --no-sandbox so ACP stdin is not consumed by sandbox relaunch.",
    )
    args = parser.parse_args()
    if args.codex_profile_probe:
        return run_codex_profile_probe(args)
    return run_probe(args)


if __name__ == "__main__":
    sys.exit(main())
