#!/usr/bin/env python3
"""Probe the SPUR-relevant capabilities of an arbitrary ACP stdio agent.

The probe speaks line-delimited JSON-RPC directly, does not inject SPUR MCP
servers, and defaults to a handshake-only run (no billed prompt). Agent argv is
supplied as one shell-like string so flags can be copied from configuration:

    python3 scripts/probe_acp_capabilities.py \\
      --command goose --args "acp --stdio" --label goose

    # Kiro-style vendor notifications + optional active RPC probes
    python3 scripts/probe_acp_capabilities.py \\
      --command kiro-cli --args acp --label kiro \\
      --probe-vendor-rpc --always-approve

Vendor extension notifications (methods outside ``session/update``, typically
``_vendor/...``) are always harvested into ``vendor_notifications`` with full
payloads. Pass ``--probe-vendor-rpc`` to also invoke discovered
``optionsMethod`` targets, ``session/set_mode`` / ``session/set_model`` when
proprietary planes advertise values, and any ``--vendor-method`` extras.

Use ``--prompt`` only when session/update evidence is worth a billed turn.
"""

from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional

JsonObject = dict[str, Any]
JsonRpcId = str | int
MODEL_FALLBACK_IDS = ("model",)
THOUGHT_FALLBACK_IDS = ("thought_level", "reasoning_effort", "effort")
STANDARD_NOTIFICATION_METHODS = {"session/update"}
DEFAULT_LOG_DIR = Path(".spur/logs")


class ProbeHardFailure(RuntimeError):
    """An error that prevents completion of the required ACP handshake."""


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def utc_now_compact() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")


def assemble_command(command: str, args_string: str) -> list[str]:
    """Build process argv from a binary and one shlex-compatible args string."""
    return [command, *shlex.split(args_string)]


def _select_choices(option: JsonObject) -> list[JsonObject]:
    if option.get("type") != "select":
        return []

    choices: list[JsonObject] = []

    def visit(value: Any) -> None:
        if isinstance(value, list):
            for item in value:
                visit(item)
            return
        if not isinstance(value, dict):
            return
        if "value" in value:
            choices.append(value)
            return
        visit(value.get("options"))

    visit(option.get("options"))
    return choices


def _synthesizer_option(
    options: list[JsonObject],
    *,
    category: str,
    fallback_ids: tuple[str, ...],
) -> Optional[JsonObject]:
    """Mirror SPUR's category-first, absent-category fallback matching."""
    categorized = next(
        (option for option in options if option.get("category") == category), None
    )
    if categorized is not None:
        return categorized
    return next(
        (
            option
            for option in options
            if option.get("category") is None and option.get("id") in fallback_ids
        ),
        None,
    )


def predict_spur_synthesis(config_options: list[JsonObject]) -> JsonObject:
    """Predict the slash commands SPUR synthesizes from frozen config options."""
    model = _synthesizer_option(
        config_options, category="model", fallback_ids=MODEL_FALLBACK_IDS
    )
    effort = _synthesizer_option(
        config_options,
        category="thought_level",
        fallback_ids=THOUGHT_FALLBACK_IDS,
    )
    slash_model = model is not None and bool(_select_choices(model))
    slash_effort = effort is not None and bool(_select_choices(effort))
    would_synthesize = []
    if slash_model:
        would_synthesize.append("/model")
    if slash_effort:
        would_synthesize.append("/effort")
    return {
        "supports_set_config_option": bool(config_options),
        "would_synthesize": would_synthesize,
        "slash_model": slash_model,
        "slash_effort": slash_effort,
        "model_config_option_id": model.get("id") if slash_model else None,
        "effort_config_option_id": effort.get("id") if slash_effort else None,
    }


def summarize_config_options(config_options: list[JsonObject]) -> JsonObject:
    summaries = []
    for option in config_options:
        choices = _select_choices(option)
        summaries.append(
            {
                "id": option.get("id"),
                "name": option.get("name"),
                "category": option.get("category"),
                "type": option.get("type"),
                "current_value": option.get("currentValue"),
                "choice_count": len(choices),
                "choices": [
                    {
                        "value": choice.get("value"),
                        "name": choice.get("name"),
                        "description": choice.get("description"),
                    }
                    for choice in choices
                ],
            }
        )
    return {"count": len(config_options), "options": summaries}


def _modes_advertised(modes: Any) -> bool:
    if isinstance(modes, list):
        return bool(modes)
    if not isinstance(modes, dict):
        return False
    available = modes.get("availableModes", modes.get("available_modes"))
    return bool(available)


def is_vendor_extension_method(method: Any) -> bool:
    """True for agent-originated vendor ext notifications (not core ACP updates)."""
    if not isinstance(method, str) or not method:
        return False
    if method in STANDARD_NOTIFICATION_METHODS:
        return False
    if method.startswith("session/") or method.startswith("fs/") or method.startswith(
        "terminal/"
    ):
        return False
    # ACP vendor extensions are conventionally underscore-prefixed on the wire
    # (``_kiro.dev/...``, ``_x.ai/...``). Also accept dotted vendor forms without
    # underscore when agents emit them as notifications.
    return method.startswith("_") or "." in method or "/" in method


def harvest_vendor_notifications(notifications: list[JsonObject]) -> JsonObject:
    """Group vendor extension notifications with full payloads and command meta.

    Keeps every payload for each vendor method (not just method names). When
    ``_*/commands/available`` frames carry a ``commands`` list, builds a
    de-duplicated catalog and extracts ``meta.optionsMethod`` targets for
    optional ``--probe-vendor-rpc`` active probes.
    """
    payloads: dict[str, list[Any]] = {}
    counts: dict[str, int] = {}
    commands_by_name: dict[str, JsonObject] = {}

    for notification in notifications:
        method = notification.get("method")
        if not is_vendor_extension_method(method):
            continue
        assert isinstance(method, str)
        params = notification.get("params")
        payload: Any = params if params is not None else {}
        payloads.setdefault(method, []).append(payload)
        counts[method] = counts.get(method, 0) + 1

        if not isinstance(params, dict):
            continue
        raw_commands = params.get("commands")
        if not isinstance(raw_commands, list):
            continue
        for command in raw_commands:
            if not isinstance(command, dict):
                continue
            name = command.get("name")
            if not isinstance(name, str) or not name:
                continue
            existing = commands_by_name.get(name)
            if existing is None:
                commands_by_name[name] = dict(command)
                continue
            # Prefer the entry with richer meta / description.
            existing_meta = existing.get("meta") or existing.get("_meta")
            new_meta = command.get("meta") or command.get("_meta")
            if not existing_meta and new_meta:
                commands_by_name[name] = dict(command)
            elif not existing.get("description") and command.get("description"):
                merged = dict(existing)
                merged["description"] = command.get("description")
                if new_meta and not existing_meta:
                    merged["meta"] = new_meta
                commands_by_name[name] = merged

    commands_catalog: list[JsonObject] = []
    options_methods: set[str] = set()
    for name in sorted(commands_by_name):
        command = commands_by_name[name]
        meta = command.get("meta")
        if meta is None:
            meta = command.get("_meta")
        entry: JsonObject = {
            "name": command.get("name"),
            "description": command.get("description"),
            "meta": meta if isinstance(meta, dict) else meta,
        }
        commands_catalog.append(entry)
        if isinstance(meta, dict):
            options_method = meta.get("optionsMethod", meta.get("options_method"))
            if isinstance(options_method, str) and options_method:
                options_methods.add(options_method)

    methods_seen = sorted(payloads)
    return {
        "methods_seen": methods_seen,
        "counts": {method: counts[method] for method in methods_seen},
        "payloads": {method: payloads[method] for method in methods_seen},
        "commands_catalog": commands_catalog,
        "options_methods": sorted(options_methods),
    }


def _available_mode_ids(modes: Any) -> list[str]:
    if isinstance(modes, list):
        raw = modes
    elif isinstance(modes, dict):
        raw = modes.get("availableModes", modes.get("available_modes", []))
    else:
        return []
    if not isinstance(raw, list):
        return []
    ids: list[str] = []
    for item in raw:
        if isinstance(item, str) and item:
            ids.append(item)
        elif isinstance(item, dict):
            mode_id = item.get("id", item.get("modeId"))
            if isinstance(mode_id, str) and mode_id:
                ids.append(mode_id)
    return ids


def _available_model_ids(session: JsonObject) -> list[str]:
    models = session.get("models")
    if not isinstance(models, dict):
        return []
    available = models.get("availableModels", models.get("available_models", []))
    if not isinstance(available, list):
        return []
    ids: list[str] = []
    for item in available:
        if isinstance(item, dict):
            model_id = item.get("modelId", item.get("id"))
            if isinstance(model_id, str) and model_id:
                ids.append(model_id)
        elif isinstance(item, str) and item:
            ids.append(item)
    return ids


def build_vendor_rpc_targets(
    *,
    session_id: str,
    session_new: JsonObject,
    vendor_harvest: JsonObject,
    extra_methods: Optional[list[str]] = None,
) -> list[JsonObject]:
    """Build soft-fail vendor RPC probes from harvest + proprietary planes."""
    targets: list[JsonObject] = []
    seen: set[tuple[str, str]] = set()

    def add(method: str, params: JsonObject, reason: str) -> None:
        key = (method, json.dumps(params, sort_keys=True, separators=(",", ":")))
        if key in seen:
            return
        seen.add(key)
        targets.append({"method": method, "params": params, "reason": reason})

    for options_method in vendor_harvest.get("options_methods") or []:
        if isinstance(options_method, str) and options_method:
            add(
                options_method,
                {"sessionId": session_id},
                "command_meta.optionsMethod",
            )

    modes = session_new.get("modes")
    mode_ids = _available_mode_ids(modes)
    current_mode = None
    if isinstance(modes, dict):
        current_mode = modes.get("currentModeId", modes.get("current_mode_id"))
    alternate = next((mode_id for mode_id in mode_ids if mode_id != current_mode), None)
    if alternate:
        add(
            "session/set_mode",
            {"sessionId": session_id, "modeId": alternate},
            "session_new.modes",
        )
        if isinstance(current_mode, str) and current_mode:
            add(
                "session/set_mode",
                {"sessionId": session_id, "modeId": current_mode},
                "restore_current_mode",
            )

    model_ids = _available_model_ids(session_new)
    models = session_new.get("models")
    current_model = None
    if isinstance(models, dict):
        current_model = models.get("currentModelId", models.get("current_model_id"))
    alternate_model = next(
        (model_id for model_id in model_ids if model_id != current_model), None
    )
    if alternate_model:
        add(
            "session/set_model",
            {"sessionId": session_id, "modelId": alternate_model},
            "session_new.models",
        )

    for method in extra_methods or []:
        if isinstance(method, str) and method.strip():
            add(method.strip(), {"sessionId": session_id}, "cli --vendor-method")

    return targets


def summarize_proprietary_planes(session_new: Optional[JsonObject]) -> JsonObject:
    session = session_new or {}
    modes = session.get("modes")
    models = session.get("models")
    config_options = session.get("configOptions", session.get("config_options", []))
    return {
        "has_config_options": bool(config_options),
        "has_modes_plane": isinstance(modes, (dict, list)) and bool(modes),
        "has_models_plane": isinstance(models, dict) and bool(models),
        "current_mode_id": (
            modes.get("currentModeId", modes.get("current_mode_id"))
            if isinstance(modes, dict)
            else None
        ),
        "current_model_id": (
            models.get("currentModelId", models.get("current_model_id"))
            if isinstance(models, dict)
            else None
        ),
        "mode_ids": _available_mode_ids(modes),
        "model_ids": _available_model_ids(session),
        "modes": modes,
        "models": models,
    }


def build_matrix(
    *,
    config_options: list[JsonObject],
    modes: Any,
    spur_prediction: JsonObject,
    available_commands: list[JsonObject],
    extension_methods: list[str],
    session_update_variants: list[str],
    vendor_harvest: Optional[JsonObject] = None,
    vendor_rpc_results: Optional[list[JsonObject]] = None,
) -> JsonObject:
    """Build the compact matrix shared with the Grok capability report."""
    harvest = vendor_harvest or harvest_vendor_notifications([])
    rpc_results = vendor_rpc_results or []
    rpc_ok = [
        str(item.get("method"))
        for item in rpc_results
        if item.get("status") == "ok" and item.get("method") is not None
    ]
    rpc_errors = [
        (
            item.get("method"),
            (item.get("error") or {}).get("code")
            if isinstance(item.get("error"), dict)
            else None,
            (item.get("error") or {}).get("message")
            if isinstance(item.get("error"), dict)
            else item.get("status"),
        )
        for item in rpc_results
        if item.get("status") not in (None, "ok")
    ]
    return {
        "config_options_advertised": bool(config_options),
        "model_select_advertised": bool(spur_prediction["slash_model"]),
        "effort_select_advertised": bool(spur_prediction["slash_effort"]),
        "available_commands_advertised": bool(available_commands),
        "modes_advertised": _modes_advertised(modes),
        "supports_set_config_option": bool(
            spur_prediction["supports_set_config_option"]
        ),
        "spur_slash_model": bool(spur_prediction["slash_model"]),
        "spur_slash_effort": bool(spur_prediction["slash_effort"]),
        "extension_methods_seen": sorted(set(extension_methods)),
        "session_update_variants_seen": sorted(set(session_update_variants)),
        "vendor_notification_methods_seen": list(harvest.get("methods_seen") or []),
        "vendor_commands_count": len(harvest.get("commands_catalog") or []),
        "options_methods_discovered": list(harvest.get("options_methods") or []),
        "vendor_rpc_ok": rpc_ok,
        "vendor_rpc_errors": rpc_errors,
    }


def _session_update(notification: JsonObject) -> JsonObject:
    params = notification.get("params")
    if not isinstance(params, dict):
        return {}
    update = params.get("update")
    return update if isinstance(update, dict) else {}


def _session_update_variants(notifications: list[JsonObject]) -> list[str]:
    variants = []
    for notification in notifications:
        variant = _session_update(notification).get("sessionUpdate")
        if isinstance(variant, str):
            variants.append(variant)
    return sorted(set(variants))


def _available_commands(notifications: list[JsonObject]) -> list[JsonObject]:
    commands: list[JsonObject] = []
    seen: set[str] = set()
    for notification in notifications:
        update = _session_update(notification)
        if update.get("sessionUpdate") != "available_commands_update":
            continue
        advertised = update.get(
            "availableCommands",
            update.get("available_commands", update.get("commands")),
        )
        if not isinstance(advertised, list):
            continue
        for command in advertised:
            if not isinstance(command, dict):
                continue
            key = json.dumps(command, sort_keys=True, separators=(",", ":"))
            if key not in seen:
                seen.add(key)
                commands.append(command)
    return commands


def _extension_methods(notifications: list[JsonObject]) -> list[str]:
    methods = {
        method
        for notification in notifications
        if isinstance((method := notification.get("method")), str)
        and method not in STANDARD_NOTIFICATION_METHODS
    }
    return sorted(methods)


def _summarize_json(value: Any, depth: int = 0) -> Any:
    if depth >= 4:
        if isinstance(value, dict):
            return {"_summary": f"object with {len(value)} keys"}
        if isinstance(value, list):
            return [f"{len(value)} items"]
    if isinstance(value, str):
        return value if len(value) <= 500 else value[:500] + "…"
    if isinstance(value, list):
        summarized = [_summarize_json(item, depth + 1) for item in value[:20]]
        if len(value) > 20:
            summarized.append(f"… {len(value) - 20} more items")
        return summarized
    if isinstance(value, dict):
        items = list(value.items())
        summarized = {
            str(key): _summarize_json(item, depth + 1) for key, item in items[:30]
        }
        if len(items) > 30:
            summarized["_truncated"] = f"{len(items) - 30} more keys"
        return summarized
    return value


def _meta_planes(sources: list[tuple[str, Any]]) -> list[JsonObject]:
    planes: list[JsonObject] = []

    def visit(source: str, value: Any, path: str) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                child_path = f"{path}/{key}"
                if key == "_meta":
                    planes.append(
                        {
                            "source": source,
                            "path": child_path,
                            "summary": _summarize_json(item),
                        }
                    )
                visit(source, item, child_path)
        elif isinstance(value, list):
            for index, item in enumerate(value):
                visit(source, item, f"{path}/{index}")

    for source, value in sources:
        visit(source, value, "")
    return planes


def build_report(
    *,
    probed_at: str,
    cmd: list[str],
    cwd: str,
    version: Optional[str],
    initialize: Optional[JsonObject],
    session_new: Optional[JsonObject],
    notifications: list[JsonObject],
    set_results: list[JsonObject],
    authentication: Optional[JsonObject] = None,
    prompt_result: Optional[JsonObject] = None,
    hard_failure: Optional[str] = None,
    vendor_rpc_results: Optional[list[JsonObject]] = None,
) -> JsonObject:
    session = session_new or {}
    raw_options = session.get("configOptions", session.get("config_options", []))
    config_options = (
        [item for item in raw_options if isinstance(item, dict)]
        if isinstance(raw_options, list)
        else []
    )
    modes = session.get("modes")
    prediction = predict_spur_synthesis(config_options)
    available_commands = _available_commands(notifications)
    variants = _session_update_variants(notifications)
    extensions = _extension_methods(notifications)
    vendor_harvest = harvest_vendor_notifications(notifications)
    rpc_results = vendor_rpc_results or []
    meta_sources: list[tuple[str, Any]] = [
        ("initialize", initialize),
        ("session_new", session_new),
        ("notifications", notifications),
        ("set_results", set_results),
        ("vendor_rpc_results", rpc_results),
    ]
    matrix = build_matrix(
        config_options=config_options,
        modes=modes,
        spur_prediction=prediction,
        available_commands=available_commands,
        extension_methods=extensions,
        session_update_variants=variants,
        vendor_harvest=vendor_harvest,
        vendor_rpc_results=rpc_results,
    )
    return {
        "probed_at": probed_at,
        "cmd": cmd,
        "cwd": cwd,
        "version": version,
        "initialize": initialize,
        "authentication": authentication,
        "session_new": session_new,
        "config_options_summary": summarize_config_options(config_options),
        "modes": modes,
        "spur_prediction": prediction,
        "meta_planes": _meta_planes(meta_sources),
        "available_commands": available_commands,
        "set_results": set_results,
        "prompt_result": prompt_result,
        "session_update_variants": variants,
        "extension_methods": extensions,
        "vendor_notifications": vendor_harvest,
        "vendor_rpc_results": rpc_results,
        "proprietary_planes": summarize_proprietary_planes(session_new),
        "matrix": matrix,
        "hard_failure": hard_failure,
    }


def _make_request(request_id: JsonRpcId, method: str, params: JsonObject) -> JsonObject:
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}


def _make_response(
    request_id: JsonRpcId,
    *,
    result: Optional[JsonObject] = None,
    error: Optional[JsonObject] = None,
) -> JsonObject:
    response: JsonObject = {"jsonrpc": "2.0", "id": request_id}
    if error is not None:
        response["error"] = error
    else:
        response["result"] = result or {}
    return response


class AcpClient:
    """Minimal thread-safe NDJSON client with a complete frame log."""

    def __init__(self, proc: subprocess.Popen[str], log_path: Path, quiet: bool):
        if proc.stdin is None or proc.stdout is None or proc.stderr is None:
            raise ProbeHardFailure("agent process pipes were not created")
        self.proc = proc
        self.stdin = proc.stdin
        self.stdout = proc.stdout
        self.stderr = proc.stderr
        self.quiet = quiet
        self.responses: dict[JsonRpcId, JsonObject] = {}
        self.notifications: list[JsonObject] = []
        self.server_requests: list[JsonObject] = []
        self._condition = threading.Condition()
        self._log_lock = threading.Lock()
        self._log_file = log_path.open("w", encoding="utf-8")
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._stderr_reader = threading.Thread(target=self._stderr_loop, daemon=True)
        self._reader.start()
        self._stderr_reader.start()

    def _log(self, direction: str, message: Any) -> None:
        with self._log_lock:
            self._log_file.write(
                json.dumps(
                    {"ts": utc_now_iso(), "dir": direction, "msg": message},
                    ensure_ascii=False,
                )
                + "\n"
            )
            self._log_file.flush()

    def _read_loop(self) -> None:
        for raw_line in self.stdout:
            line = raw_line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                self._log("stdout_unparsed", {"text": line})
                if not self.quiet:
                    print(f"[stdout] non-JSON: {line[:240]}", file=sys.stderr)
                continue
            self._log("recv", message)
            with self._condition:
                if "method" in message and "id" in message:
                    self.server_requests.append(message)
                elif "method" in message:
                    self.notifications.append(message)
                elif "id" in message:
                    self.responses[message["id"]] = message
                self._condition.notify_all()

    def _stderr_loop(self) -> None:
        for raw_line in self.stderr:
            line = raw_line.rstrip("\n")
            self._log("stderr", {"text": line})
            if line and not self.quiet:
                print(f"[stderr] {line}", file=sys.stderr)

    def send(self, message: JsonObject) -> None:
        encoded = json.dumps(message, separators=(",", ":"), ensure_ascii=False)
        try:
            self.stdin.write(encoded + "\n")
            self.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            raise ProbeHardFailure(f"agent stdin closed: {exc}") from exc
        self._log("send", message)
        if not self.quiet:
            print(f"[send] {encoded[:300]}")

    def request(
        self,
        method: str,
        params: JsonObject,
        timeout: float,
        server_request_handler: Callable[[JsonObject], None],
    ) -> Optional[JsonObject]:
        request_id = str(uuid.uuid4())
        self.send(_make_request(request_id, method, params))
        deadline = time.monotonic() + timeout
        while True:
            for request in self.drain_server_requests():
                server_request_handler(request)
            with self._condition:
                response = self.responses.pop(request_id, None)
                if response is not None:
                    return response
                return_code = self.proc.poll()
                if return_code is not None:
                    raise ProbeHardFailure(
                        f"agent exited with status {return_code} while waiting for {method}"
                    )
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self._condition.wait(timeout=min(remaining, 0.05))

    def drain_notifications(self) -> list[JsonObject]:
        with self._condition:
            notifications = self.notifications[:]
            self.notifications.clear()
        return notifications

    def drain_server_requests(self) -> list[JsonObject]:
        with self._condition:
            requests = self.server_requests[:]
            self.server_requests.clear()
        return requests

    def close(self) -> tuple[Optional[int], bool]:
        forced = False
        try:
            self.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            forced = True
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=2)
        self._reader.join(timeout=1)
        self._stderr_reader.join(timeout=1)
        self._log_file.close()
        return self.proc.returncode, forced


def process_close_failure(*, return_code: Optional[int], forced: bool) -> Optional[str]:
    """Classify an agent exit observed before probe-directed termination."""
    if forced or return_code in (None, 0):
        return None
    return f"agent exited with status {return_code} during probe cleanup"


def _permission_option(options: list[JsonObject], approve: bool) -> str:
    preferred = (
        ("allow_always", "allow_once", "allow", "approve_for_session", "approve")
        if approve
        else ("reject_once", "reject_always", "reject", "deny", "cancel")
    )
    for candidate in preferred:
        for option in options:
            option_id = option.get("optionId", option.get("id"))
            if option.get("kind") == candidate or option_id == candidate:
                return str(option_id or candidate)
    if options:
        option = options[0] if approve else options[-1]
        return str(option.get("optionId", option.get("id", "approve")))
    return "approve" if approve else "cancel"


def _server_request_handler(
    client: AcpClient, always_approve: bool
) -> Callable[[JsonObject], None]:
    def handle(request: JsonObject) -> None:
        request_id = request.get("id")
        method = request.get("method")
        if request_id is None:
            return
        if method == "session/request_permission":
            raw_options = (request.get("params") or {}).get("options", [])
            options = (
                [item for item in raw_options if isinstance(item, dict)]
                if isinstance(raw_options, list)
                else []
            )
            selected = _permission_option(options, always_approve)
            client.send(
                _make_response(
                    request_id,
                    result={"outcome": {"outcome": "selected", "optionId": selected}},
                )
            )
            return
        client.send(
            _make_response(
                request_id,
                error={
                    "code": -32601,
                    "message": f"capability probe does not implement {method}",
                },
            )
        )

    return handle


def _response_error(response: Optional[JsonObject]) -> Optional[JsonObject]:
    if response is None:
        return None
    error = response.get("error")
    return error if isinstance(error, dict) else None


def _probe_version(command: str, cwd: Path) -> Optional[str]:
    try:
        completed = subprocess.run(
            [command, "--version"],
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    output = completed.stdout.strip()
    return output[:1000] if output else None


def _record_result(
    method: str, params: JsonObject, response: Optional[JsonObject]
) -> JsonObject:
    return {
        "method": method,
        "params": params,
        "status": (
            "timeout"
            if response is None
            else ("error" if "error" in response else "ok")
        ),
        "response": response,
    }


def _probe_config_sets(
    client: AcpClient,
    session_id: str,
    config_options: list[JsonObject],
    timeout: float,
    handler: Callable[[JsonObject], None],
) -> list[JsonObject]:
    results = []
    targets: list[JsonObject] = []
    for category, fallback_ids in (
        ("model", MODEL_FALLBACK_IDS),
        ("thought_level", THOUGHT_FALLBACK_IDS),
    ):
        option = _synthesizer_option(
            config_options, category=category, fallback_ids=fallback_ids
        )
        if option is not None and option not in targets:
            targets.append(option)

    if not config_options:
        params = {"sessionId": session_id, "configId": "model", "value": "spur-probe"}
        response = client.request("session/set_config_option", params, timeout, handler)
        return [_record_result("session/set_config_option", params, response)]

    for option in targets:
        choices = _select_choices(option)
        if not choices:
            continue
        current = option.get("currentValue")
        choice = next(
            (item for item in choices if item.get("value") != current), choices[0]
        )
        params = {
            "sessionId": session_id,
            "configId": option.get("id"),
            "value": choice.get("value"),
        }
        response = client.request("session/set_config_option", params, timeout, handler)
        results.append(_record_result("session/set_config_option", params, response))
    return results


def _legacy_model_id(session: JsonObject, config_options: list[JsonObject]) -> str:
    models = session.get("models")
    if isinstance(models, dict):
        current = models.get("currentModelId", models.get("current_model_id"))
        if isinstance(current, str) and current:
            return current
        available = models.get("availableModels", models.get("available_models", []))
        if isinstance(available, list):
            for model in available:
                if isinstance(model, dict):
                    model_id = model.get("modelId", model.get("id"))
                    if isinstance(model_id, str) and model_id:
                        return model_id
    option = _synthesizer_option(
        config_options, category="model", fallback_ids=MODEL_FALLBACK_IDS
    )
    if option is not None and isinstance(option.get("currentValue"), str):
        return option["currentValue"]
    return "spur-probe"


def _probe_vendor_rpcs(
    client: AcpClient,
    *,
    session_id: str,
    session_new: JsonObject,
    notifications: list[JsonObject],
    extra_methods: list[str],
    timeout: float,
    handler: Callable[[JsonObject], None],
) -> tuple[list[JsonObject], list[JsonObject], Optional[str]]:
    """Actively probe vendor RPCs; soft-fail so handshake evidence is preserved.

    Returns ``(results, extra_notifications, stop_reason)``. ``stop_reason`` is
    set when the agent dies mid-probe; callers should not treat that as a hard
    handshake failure if initialize/session-new already succeeded.
    """
    harvest = harvest_vendor_notifications(notifications)
    targets = build_vendor_rpc_targets(
        session_id=session_id,
        session_new=session_new,
        vendor_harvest=harvest,
        extra_methods=extra_methods,
    )
    results: list[JsonObject] = []
    extra_notifications: list[JsonObject] = []
    stop_reason: Optional[str] = None

    for target in targets:
        method = str(target["method"])
        params = target.get("params") if isinstance(target.get("params"), dict) else {}
        assert isinstance(params, dict)
        try:
            response = client.request(method, params, timeout, handler)
        except ProbeHardFailure as exc:
            stop_reason = str(exc)
            results.append(
                {
                    "method": method,
                    "params": params,
                    "reason": target.get("reason"),
                    "status": "agent_exited",
                    "error": {"code": -32000, "message": stop_reason},
                    "response": None,
                }
            )
            break
        record = _record_result(method, params, response)
        record["reason"] = target.get("reason")
        if isinstance(response, dict) and isinstance(response.get("error"), dict):
            record["error"] = response["error"]
        results.append(record)
        time.sleep(0.05)
        extra_notifications.extend(client.drain_notifications())
        if stop_reason:
            break

    return results, extra_notifications, stop_reason


def _write_report(path: Path, report: JsonObject) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def _artifact_paths(args: argparse.Namespace, command: list[str]) -> tuple[Path, Path]:
    label = args.label or Path(command[0]).name
    safe_label = re.sub(r"[^A-Za-z0-9_.-]+", "-", label).strip("-.") or "agent"
    stamp = utc_now_compact()
    out = args.out or DEFAULT_LOG_DIR / f"probe-{safe_label}-{stamp}.jsonl"
    report = args.report or DEFAULT_LOG_DIR / f"probe-{safe_label}-{stamp}.report.json"
    return out, report


def run_probe(args: argparse.Namespace) -> int:
    cwd = args.cwd.expanduser().resolve()
    command = assemble_command(args.command, args.args)
    out_path, report_path = _artifact_paths(args, command)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    probed_at = utc_now_iso()
    version = _probe_version(args.command, cwd)
    initialize: Optional[JsonObject] = None
    session_new: Optional[JsonObject] = None
    authentication: Optional[JsonObject] = None
    prompt_result: Optional[JsonObject] = None
    notifications: list[JsonObject] = []
    set_results: list[JsonObject] = []
    vendor_rpc_results: list[JsonObject] = []
    hard_failure: Optional[str] = None
    handshake_complete = False
    client: Optional[AcpClient] = None

    try:
        proc = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
        )
        client = AcpClient(proc, out_path, args.quiet)
        handler = _server_request_handler(client, args.always_approve)

        init_response = client.request(
            "initialize",
            {
                "protocolVersion": 1,
                "clientInfo": {
                    "name": "spur-acp-capability-probe",
                    "version": "0.1.0",
                },
                "clientCapabilities": {
                    "fs": {"readTextFile": True, "writeTextFile": True},
                    "terminal": True,
                    "session": {"configOptions": {}},
                },
            },
            args.init_timeout,
            handler,
        )
        if init_response is None:
            raise ProbeHardFailure("initialize timed out")
        if error := _response_error(init_response):
            raise ProbeHardFailure(
                f"initialize failed: {json.dumps(error, sort_keys=True)}"
            )
        result = init_response.get("result")
        initialize = result if isinstance(result, dict) else {}
        agent_info = initialize.get("agentInfo", initialize.get("agent_info"))
        if isinstance(agent_info, dict) and agent_info.get("version") is not None:
            version = str(agent_info["version"])

        if args.authenticate:
            auth_methods = initialize.get(
                "authMethods", initialize.get("auth_methods", [])
            )
            if isinstance(auth_methods, list) and auth_methods:
                first = auth_methods[0] if isinstance(auth_methods[0], dict) else {}
                method_id = first.get("id")
                auth_response = client.request(
                    "authenticate", {"methodId": method_id}, args.init_timeout, handler
                )
                authentication = _record_result(
                    "authenticate", {"methodId": method_id}, auth_response
                )

        requested_session_id = str(uuid.uuid4())
        session_response = client.request(
            "session/new",
            {"sessionId": requested_session_id, "cwd": str(cwd), "mcpServers": []},
            args.session_timeout,
            handler,
        )
        if session_response is None:
            raise ProbeHardFailure("session/new timed out")
        if error := _response_error(session_response):
            raise ProbeHardFailure(
                f"session/new failed: {json.dumps(error, sort_keys=True)}"
            )
        session_result = session_response.get("result")
        session_new = session_result if isinstance(session_result, dict) else {}
        session_id = str(session_new.get("sessionId", requested_session_id))
        handshake_complete = True

        time.sleep(args.preamble_timeout)
        notifications.extend(client.drain_notifications())
        raw_options = session_new.get(
            "configOptions", session_new.get("config_options", [])
        )
        config_options = (
            [item for item in raw_options if isinstance(item, dict)]
            if isinstance(raw_options, list)
            else []
        )

        if args.try_set:
            set_results.extend(
                _probe_config_sets(
                    client, session_id, config_options, args.session_timeout, handler
                )
            )
            notifications.extend(client.drain_notifications())

        if args.try_set_model:
            params = {
                "sessionId": session_id,
                "modelId": _legacy_model_id(session_new, config_options),
            }
            response = client.request(
                "session/set_model", params, args.session_timeout, handler
            )
            set_results.append(_record_result("session/set_model", params, response))
            notifications.extend(client.drain_notifications())

        if args.probe_vendor_rpc:
            rpc_results, extra_notifs, vendor_stop = _probe_vendor_rpcs(
                client,
                session_id=session_id,
                session_new=session_new,
                notifications=notifications,
                extra_methods=list(args.vendor_method or []),
                timeout=args.session_timeout,
                handler=handler,
            )
            vendor_rpc_results.extend(rpc_results)
            notifications.extend(extra_notifs)
            if vendor_stop and not args.quiet:
                print(
                    f"[vendor-rpc] stopped early: {vendor_stop}",
                    file=sys.stderr,
                )

        if args.prompt is not None:
            params = {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": args.prompt}],
            }
            response = client.request("session/prompt", params, args.timeout, handler)
            prompt_result = _record_result("session/prompt", params, response)
            time.sleep(0.2)
            notifications.extend(client.drain_notifications())
        if return_code := client.proc.poll():
            # After a successful handshake, a later agent exit (e.g. unstable
            # vendor RPC) is recorded on the report but is not a hard fail.
            if not handshake_complete:
                raise ProbeHardFailure(
                    f"agent exited with status {return_code} before probe cleanup"
                )
    except (OSError, ValueError, ProbeHardFailure) as exc:
        hard_failure = str(exc)
    finally:
        if client is not None:
            return_code, forced = client.close()
            if hard_failure is None and not handshake_complete:
                hard_failure = process_close_failure(
                    return_code=return_code, forced=forced
                )

    report = build_report(
        probed_at=probed_at,
        cmd=command,
        cwd=str(cwd),
        version=version,
        initialize=initialize,
        session_new=session_new,
        notifications=notifications,
        set_results=set_results,
        authentication=authentication,
        prompt_result=prompt_result,
        hard_failure=hard_failure,
        vendor_rpc_results=vendor_rpc_results,
    )
    if client is None:
        out_path.write_text(
            json.dumps(
                {
                    "ts": utc_now_iso(),
                    "dir": "hard_failure",
                    "msg": {"text": hard_failure},
                }
            )
            + "\n",
            encoding="utf-8",
        )
    _write_report(report_path, report)

    print(f"jsonl={out_path}")
    print(f"report={report_path}")
    if hard_failure is not None:
        print(f"[FAIL] {hard_failure}", file=sys.stderr)
        return 1
    print(json.dumps(report["matrix"], indent=2, sort_keys=True))
    vendor = report.get("vendor_notifications") or {}
    if vendor.get("methods_seen"):
        print(
            "vendor_notifications="
            + json.dumps(
                {
                    "methods": vendor.get("methods_seen"),
                    "commands": len(vendor.get("commands_catalog") or []),
                    "options_methods": vendor.get("options_methods"),
                },
                sort_keys=True,
            )
        )
    if vendor_rpc_results:
        print(
            "vendor_rpc="
            + json.dumps(
                {
                    "ok": report["matrix"].get("vendor_rpc_ok"),
                    "errors": report["matrix"].get("vendor_rpc_errors"),
                },
                sort_keys=True,
            )
        )
    return 0


def parse_cli(argv: Optional[list[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--command", required=True, help="ACP agent executable to spawn."
    )
    parser.add_argument(
        "--args",
        default="",
        help="One shlex-parsed argument string, e.g. 'agent --flag stdio'.",
    )
    parser.add_argument(
        "--label", help="Artifact filename label (default: command basename)."
    )
    parser.add_argument(
        "--cwd", type=Path, default=Path.cwd(), help="Process and ACP session cwd."
    )
    parser.add_argument("--out", type=Path, help="JSONL frame log path.")
    parser.add_argument("--report", type=Path, help="Structured JSON report path.")
    parser.add_argument(
        "--authenticate",
        action="store_true",
        help="Authenticate with the first method advertised by initialize.",
    )
    parser.add_argument(
        "--try-set",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Probe relevant session/set_config_option values (default: on).",
    )
    parser.add_argument(
        "--try-set-model",
        action="store_true",
        help="Also probe legacy session/set_model using an advertised model id.",
    )
    parser.add_argument(
        "--probe-vendor-rpc",
        action="store_true",
        help=(
            "After handshake, actively probe vendor RPCs discovered from "
            "command meta.optionsMethod plus session/set_mode and "
            "session/set_model when proprietary planes advertise values. "
            "Failures are soft (recorded, not hard exit)."
        ),
    )
    parser.add_argument(
        "--vendor-method",
        action="append",
        default=[],
        help=(
            "Extra vendor JSON-RPC method to probe when --probe-vendor-rpc is set "
            "(repeatable). Params are {sessionId}."
        ),
    )
    parser.add_argument(
        "--prompt",
        help="Optional billed prompt; omitted by default for handshake-only probing.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        help="Prompt response timeout in seconds.",
    )
    parser.add_argument("--init-timeout", type=float, default=30.0)
    parser.add_argument("--session-timeout", type=float, default=45.0)
    parser.add_argument(
        "--preamble-timeout",
        type=float,
        default=0.5,
        help="Seconds to drain notifications after session/new.",
    )
    parser.add_argument(
        "--always-approve",
        action="store_true",
        help="Select an allow option for agent permission requests.",
    )
    parser.add_argument("--quiet", action="store_true")
    return parser.parse_args(argv)


def main() -> int:
    return run_probe(parse_cli())


if __name__ == "__main__":
    sys.exit(main())
