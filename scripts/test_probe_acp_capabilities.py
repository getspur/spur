#!/usr/bin/env python3
"""Unit tests for the generic ACP capability probe (no live agent required)."""

from __future__ import annotations

import io
import unittest
from contextlib import redirect_stderr

from scripts import probe_acp_capabilities as probe


def select_option(
    option_id: str,
    *,
    category: str | None = None,
    values: tuple[str, ...] = ("one", "two"),
) -> dict:
    option = {
        "id": option_id,
        "name": option_id.replace("_", " ").title(),
        "type": "select",
        "currentValue": values[0] if values else "",
        "options": [{"value": value, "name": value.title()} for value in values],
    }
    if category is not None:
        option["category"] = category
    return option


class SynthesizerPredictionTests(unittest.TestCase):
    def test_model_and_effort_categories_predict_slash_commands(self) -> None:
        options = [
            select_option("vendor-model", category="model"),
            select_option("vendor-effort", category="thought_level"),
        ]

        prediction = probe.predict_spur_synthesis(options)

        self.assertEqual(prediction["would_synthesize"], ["/model", "/effort"])
        self.assertTrue(prediction["slash_model"])
        self.assertTrue(prediction["slash_effort"])
        self.assertEqual(prediction["model_config_option_id"], "vendor-model")
        self.assertEqual(prediction["effort_config_option_id"], "vendor-effort")

    def test_absent_categories_use_spur_fallback_ids(self) -> None:
        for effort_id in ("thought_level", "reasoning_effort", "effort"):
            with self.subTest(effort_id=effort_id):
                prediction = probe.predict_spur_synthesis(
                    [select_option("model"), select_option(effort_id)]
                )
                self.assertTrue(prediction["slash_model"])
                self.assertTrue(prediction["slash_effort"])

    def test_category_match_wins_over_fallback_id(self) -> None:
        categorized_without_choices = select_option(
            "vendor-model", category="model", values=()
        )
        fallback_with_choices = select_option("model")

        prediction = probe.predict_spur_synthesis(
            [categorized_without_choices, fallback_with_choices]
        )

        self.assertFalse(prediction["slash_model"])

    def test_empty_options_predict_no_slash_synthesis(self) -> None:
        prediction = probe.predict_spur_synthesis([])

        self.assertFalse(prediction["supports_set_config_option"])
        self.assertEqual(prediction["would_synthesize"], [])
        self.assertFalse(prediction["slash_model"])
        self.assertFalse(prediction["slash_effort"])

    def test_non_select_or_choice_less_options_do_not_synthesize(self) -> None:
        options = [
            {"id": "model", "category": "model", "type": "boolean"},
            select_option("effort", values=()),
        ]

        prediction = probe.predict_spur_synthesis(options)

        self.assertTrue(prediction["supports_set_config_option"])
        self.assertEqual(prediction["would_synthesize"], [])


class SlashSurfacePredictionTests(unittest.TestCase):
    def test_grok_models_and_session_config_predict_model_and_effort(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            initialize={
                "_meta": {
                    "modelState": {
                        "currentModelId": "grok-4.5",
                        "availableModels": [
                            {
                                "modelId": "grok-4.5",
                                "name": "Grok 4.5",
                                "_meta": {
                                    "reasoningEfforts": [
                                        {"id": "low", "label": "Low Effort"},
                                        {"id": "high", "label": "High Effort"},
                                    ]
                                },
                            }
                        ],
                    }
                }
            },
            session_new={
                "sessionId": "grok-session",
                "configOptions": [],
                "models": {
                    "currentModelId": "grok-4.5",
                    "availableModels": [{"modelId": "grok-4.5"}],
                },
                "_meta": {
                    "x.ai/sessionConfig": {
                        "options": [
                            {
                                "id": "grok-4.5",
                                "category": "model",
                                "selected": True,
                            },
                            {"id": "high", "category": "mode", "selected": True},
                            {"id": "medium", "category": "mode"},
                            {"id": "low", "category": "mode"},
                        ]
                    }
                },
            },
            set_results=[{"method": "session/set_model", "status": "ok"}],
        )

        self.assertTrue(prediction["slash_model"])
        self.assertTrue(prediction["slash_effort"])
        self.assertFalse(prediction["supports_set_config_option"])
        self.assertEqual(prediction["model_source"], "proprietary_models_plane")
        self.assertEqual(prediction["effort_source"], "proprietary_session_config")
        self.assertTrue(prediction["proprietary_direct_set_model"])
        self.assertEqual(prediction["proprietary_model_ids_sample"], ["grok-4.5"])

    def test_kiro_models_predict_model_without_effort(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            session_new={
                "sessionId": "kiro-session",
                "configOptions": [],
                "models": {
                    "currentModelId": "auto",
                    "availableModels": [
                        {"modelId": "auto", "name": "auto"},
                        {
                            "modelId": "claude-sonnet-4.5",
                            "name": "Claude Sonnet 4.5",
                        },
                    ],
                },
            },
        )

        self.assertTrue(prediction["slash_model"])
        self.assertFalse(prediction["slash_effort"])
        self.assertFalse(prediction["supports_set_config_option"])
        self.assertEqual(prediction["model_source"], "proprietary_models_plane")
        self.assertEqual(prediction["effort_source"], "none")
        self.assertTrue(prediction["proprietary_direct_set_model"])
        self.assertEqual(
            prediction["proprietary_model_ids_sample"],
            ["auto", "claude-sonnet-4.5"],
        )

    def test_grok_current_model_state_predicts_effort_without_session_mode(
        self,
    ) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            initialize={
                "_meta": {
                    "modelState": {
                        "currentModelId": "grok-4.5",
                        "availableModels": [
                            {
                                "modelId": "grok-4.5",
                                "_meta": {
                                    "reasoningEfforts": [
                                        {"id": "medium", "label": "Medium"}
                                    ]
                                },
                            }
                        ],
                    }
                }
            },
            session_new={},
        )

        self.assertTrue(prediction["slash_model"])
        self.assertTrue(prediction["slash_effort"])
        self.assertEqual(prediction["effort_source"], "proprietary_model_state")

    def test_session_config_model_ids_do_not_replace_available_models(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            session_new={
                "_meta": {
                    "x.ai/sessionConfig": {
                        "options": [{"id": "grok-4.5", "category": "model"}]
                    }
                }
            },
            set_results=[{"method": "session/set_model", "status": "ok"}],
        )

        self.assertFalse(prediction["slash_model"])
        self.assertEqual(prediction["model_source"], "none")
        self.assertEqual(prediction["proprietary_model_ids_sample"], [])

    def test_available_models_requires_model_id_or_id_fields(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            session_new={"models": {"availableModels": ["grok-4.5"]}},
            set_results=[{"method": "session/set_model", "status": "ok"}],
        )

        self.assertFalse(prediction["slash_model"])
        self.assertEqual(prediction["proprietary_model_ids_sample"], [])

    def test_grok_session_config_effort_does_not_require_model_catalog(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            session_new={
                "_meta": {
                    "x.ai/sessionConfig": {
                        "options": [{"id": "high", "category": "mode"}]
                    }
                }
            },
        )

        self.assertFalse(prediction["slash_model"])
        self.assertTrue(prediction["slash_effort"])
        self.assertEqual(prediction["effort_source"], "proprietary_session_config")

    def test_config_options_win_over_proprietary_planes(self) -> None:
        config_options = [
            select_option("model", category="model"),
            select_option("reasoning_effort", category="thought_level"),
        ]

        prediction = probe.predict_spur_slash_surfaces(
            config_options=config_options,
            initialize={
                "_meta": {
                    "modelState": {
                        "currentModelId": "grok-4.5",
                        "availableModels": [
                            {
                                "modelId": "grok-4.5",
                                "_meta": {"reasoningEfforts": [{"id": "high"}]},
                            }
                        ],
                    }
                }
            },
            session_new={
                "models": {
                    "currentModelId": "grok-4.5",
                    "availableModels": [{"modelId": "grok-4.5"}],
                },
                "_meta": {
                    "x.ai/sessionConfig": {
                        "options": [{"id": "high", "category": "mode"}]
                    }
                },
            },
            set_results=[{"method": "session/set_model", "status": "ok"}],
        )

        self.assertEqual(prediction["model_source"], "config_option")
        self.assertEqual(prediction["effort_source"], "config_option")
        self.assertEqual(prediction["model_config_option_id"], "model")
        self.assertEqual(prediction["effort_config_option_id"], "reasoning_effort")
        self.assertTrue(prediction["supports_set_config_option"])

    def test_empty_session_predicts_no_slash_surfaces(self) -> None:
        prediction = probe.predict_spur_slash_surfaces(
            config_options=[],
            session_new={},
            initialize={},
        )

        self.assertEqual(prediction["would_synthesize"], [])
        self.assertFalse(prediction["slash_model"])
        self.assertFalse(prediction["slash_effort"])
        self.assertEqual(prediction["model_source"], "none")
        self.assertEqual(prediction["effort_source"], "none")
        self.assertFalse(prediction["proprietary_direct_set_model"])
        self.assertEqual(prediction["proprietary_model_ids_sample"], [])


class CliTests(unittest.TestCase):
    def test_args_string_is_split_with_shell_quoting(self) -> None:
        args = probe.parse_cli(
            [
                "--command",
                "/opt/Agent CLI/bin/agent",
                "--args",
                "acp --profile 'careful worker' --flag",
            ]
        )

        self.assertEqual(
            probe.assemble_command(args.command, args.args),
            [
                "/opt/Agent CLI/bin/agent",
                "acp",
                "--profile",
                "careful worker",
                "--flag",
            ],
        )

    def test_command_is_required(self) -> None:
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            probe.parse_cli([])

    def test_unforced_nonzero_cleanup_is_a_hard_failure(self) -> None:
        self.assertEqual(
            probe.process_close_failure(return_code=7, forced=False),
            "agent exited with status 7 during probe cleanup",
        )
        self.assertIsNone(probe.process_close_failure(return_code=0, forced=False))
        self.assertIsNone(probe.process_close_failure(return_code=-15, forced=True))


class VendorHarvestTests(unittest.TestCase):
    def test_vendor_method_detection(self) -> None:
        self.assertTrue(
            probe.is_vendor_extension_method("_kiro.dev/commands/available")
        )
        self.assertTrue(probe.is_vendor_extension_method("_x.ai/sessionConfig/update"))
        self.assertFalse(probe.is_vendor_extension_method("session/update"))
        self.assertFalse(probe.is_vendor_extension_method("session/new"))
        self.assertFalse(probe.is_vendor_extension_method(""))

    def test_harvest_keeps_full_vendor_payloads_and_command_meta(self) -> None:
        notifications = [
            {
                "method": "_kiro.dev/commands/available",
                "params": {
                    "commands": [
                        {
                            "name": "/model",
                            "description": "Select model",
                            "meta": {
                                "optionsMethod": "_kiro.dev/commands/model/options",
                                "inputType": "selection",
                            },
                        },
                        {
                            "name": "/agent",
                            "description": "Select agent",
                            "meta": {
                                "optionsMethod": "_kiro.dev/commands/agent/options",
                            },
                        },
                    ]
                },
            },
            {
                "method": "_kiro.dev/commands/available",
                "params": {
                    "commands": [
                        {"name": "/model", "description": "Select model"},
                        {"name": "/compact", "description": "Compact"},
                    ]
                },
            },
            {
                "method": "_kiro.dev/metadata",
                "params": {"sessionId": "s1", "contextUsagePercentage": 8.9},
            },
            {
                "method": "session/update",
                "params": {
                    "update": {
                        "sessionUpdate": "available_commands_update",
                        "availableCommands": [{"name": "compact"}],
                    }
                },
            },
        ]

        harvest = probe.harvest_vendor_notifications(notifications)

        self.assertEqual(
            harvest["methods_seen"],
            ["_kiro.dev/commands/available", "_kiro.dev/metadata"],
        )
        self.assertEqual(harvest["counts"]["_kiro.dev/commands/available"], 2)
        self.assertEqual(len(harvest["payloads"]["_kiro.dev/metadata"]), 1)
        self.assertEqual(
            harvest["payloads"]["_kiro.dev/metadata"][0]["contextUsagePercentage"], 8.9
        )
        # Union of command names across frames; keep richest meta when present.
        names = [item["name"] for item in harvest["commands_catalog"]]
        self.assertEqual(names, ["/agent", "/compact", "/model"])
        model = next(
            item for item in harvest["commands_catalog"] if item["name"] == "/model"
        )
        self.assertEqual(
            model["meta"]["optionsMethod"], "_kiro.dev/commands/model/options"
        )
        self.assertEqual(
            harvest["options_methods"],
            [
                "_kiro.dev/commands/agent/options",
                "_kiro.dev/commands/model/options",
            ],
        )

    def test_vendor_rpc_targets_include_options_modes_and_models(self) -> None:
        session_new = {
            "sessionId": "sid-1",
            "modes": {
                "currentModeId": "kiro_default",
                "availableModes": [
                    {"id": "kiro_default"},
                    {"id": "kiro_planner"},
                ],
            },
            "models": {
                "currentModelId": "claude-sonnet-4.5",
                "availableModels": [
                    {"modelId": "claude-sonnet-4.5"},
                    {"modelId": "claude-haiku-4.5"},
                ],
            },
        }
        harvest = {
            "options_methods": ["_kiro.dev/commands/model/options"],
            "commands_catalog": [],
            "methods_seen": ["_kiro.dev/metadata"],
        }

        targets = probe.build_vendor_rpc_targets(
            session_id="sid-1",
            session_new=session_new,
            vendor_harvest=harvest,
            extra_methods=["_x.ai/sessionConfig/update"],
        )

        methods = [item["method"] for item in targets]
        self.assertIn("_kiro.dev/commands/model/options", methods)
        self.assertIn("session/set_mode", methods)
        self.assertIn("session/set_model", methods)
        self.assertIn("_x.ai/sessionConfig/update", methods)
        set_mode = next(
            item for item in targets if item["method"] == "session/set_mode"
        )
        self.assertEqual(set_mode["params"]["modeId"], "kiro_planner")
        set_model = next(
            item for item in targets if item["method"] == "session/set_model"
        )
        self.assertEqual(set_model["params"]["modelId"], "claude-haiku-4.5")

    def test_matrix_includes_vendor_harvest_fields(self) -> None:
        matrix = probe.build_matrix(
            config_options=[],
            modes={"availableModes": [{"id": "a"}]},
            spur_prediction=probe.predict_spur_synthesis([]),
            available_commands=[],
            extension_methods=["_kiro.dev/metadata"],
            session_update_variants=[],
            vendor_harvest={
                "methods_seen": ["_kiro.dev/commands/available", "_kiro.dev/metadata"],
                "commands_catalog": [{"name": "/model"}],
                "options_methods": ["_kiro.dev/commands/model/options"],
            },
            vendor_rpc_results=[
                {"method": "session/set_mode", "status": "ok"},
                {
                    "method": "_kiro.dev/commands/model/options",
                    "status": "error",
                    "error": {"code": -32601, "message": "Method not found"},
                },
            ],
        )

        self.assertEqual(
            matrix["vendor_notification_methods_seen"],
            ["_kiro.dev/commands/available", "_kiro.dev/metadata"],
        )
        self.assertEqual(matrix["vendor_commands_count"], 1)
        self.assertEqual(
            matrix["options_methods_discovered"],
            ["_kiro.dev/commands/model/options"],
        )
        self.assertEqual(matrix["vendor_rpc_ok"], ["session/set_mode"])
        self.assertEqual(
            matrix["vendor_rpc_errors"][0][0],
            "_kiro.dev/commands/model/options",
        )


class ReportTests(unittest.TestCase):
    def setUp(self) -> None:
        self.config_options = [
            select_option("model", category="model"),
            select_option("reasoning_effort", category="thought_level"),
        ]
        self.prediction = probe.predict_spur_synthesis(self.config_options)

    def test_matrix_matches_grok_report_contract(self) -> None:
        matrix = probe.build_matrix(
            config_options=self.config_options,
            modes={"currentModeId": "code", "availableModes": [{"id": "code"}]},
            spur_prediction=self.prediction,
            available_commands=[{"name": "compact"}],
            extension_methods=["_vendor/settings/update"],
            session_update_variants=["available_commands_update", "usage_update"],
        )

        self.assertEqual(
            matrix,
            {
                "config_options_advertised": True,
                "model_select_advertised": True,
                "effort_select_advertised": True,
                "available_commands_advertised": True,
                "modes_advertised": True,
                "supports_set_config_option": True,
                "spur_slash_model": True,
                "spur_slash_effort": True,
                "extension_methods_seen": ["_vendor/settings/update"],
                "session_update_variants_seen": [
                    "available_commands_update",
                    "usage_update",
                ],
                "vendor_notification_methods_seen": [],
                "vendor_commands_count": 0,
                "options_methods_discovered": [],
                "vendor_rpc_ok": [],
                "vendor_rpc_errors": [],
            },
        )

    def test_report_schema_contains_required_probe_planes(self) -> None:
        report = probe.build_report(
            probed_at="2026-07-13T01:02:03+00:00",
            cmd=["agent", "acp"],
            cwd="/tmp/worktree",
            version="agent 1.2.3",
            initialize={"protocolVersion": 1},
            session_new={
                "sessionId": "session-1",
                "configOptions": self.config_options,
            },
            notifications=[
                {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "available_commands_update",
                            "availableCommands": [{"name": "compact"}],
                            "_meta": {"vendor": {"state": "ready"}},
                        }
                    },
                },
                {
                    "method": "_vendor/settings/update",
                    "params": {"theme": "dark"},
                },
                {
                    "method": "_kiro.dev/commands/available",
                    "params": {
                        "commands": [
                            {
                                "name": "/model",
                                "meta": {
                                    "optionsMethod": "_kiro.dev/commands/model/options"
                                },
                            }
                        ]
                    },
                },
            ],
            set_results=[],
            vendor_rpc_results=[
                {
                    "method": "session/set_mode",
                    "params": {"sessionId": "session-1", "modeId": "plan"},
                    "status": "ok",
                    "response": {"result": {}},
                }
            ],
        )

        for key in (
            "probed_at",
            "cmd",
            "cwd",
            "version",
            "initialize",
            "session_new",
            "config_options_summary",
            "modes",
            "spur_prediction",
            "meta_planes",
            "available_commands",
            "set_results",
            "session_update_variants",
            "extension_methods",
            "vendor_notifications",
            "vendor_rpc_results",
            "proprietary_planes",
            "matrix",
        ):
            self.assertIn(key, report)
        self.assertEqual(report["available_commands"], [{"name": "compact"}])
        self.assertEqual(
            report["extension_methods"],
            ["_kiro.dev/commands/available", "_vendor/settings/update"],
        )
        self.assertTrue(report["meta_planes"])
        self.assertTrue(report["matrix"]["spur_slash_model"])
        self.assertEqual(
            report["vendor_notifications"]["payloads"]["_vendor/settings/update"],
            [{"theme": "dark"}],
        )
        self.assertEqual(report["matrix"]["vendor_commands_count"], 1)
        self.assertEqual(report["matrix"]["vendor_rpc_ok"], ["session/set_mode"])
        self.assertEqual(
            report["proprietary_planes"]["has_models_plane"],
            False,
        )

    def test_matrix_separates_proprietary_slash_from_config_advertisement(self) -> None:
        fixtures = (
            (
                "grok",
                {
                    "sessionId": "grok-session",
                    "configOptions": [],
                    "models": {
                        "currentModelId": "grok-4.5",
                        "availableModels": [{"modelId": "grok-4.5"}],
                    },
                    "_meta": {
                        "x.ai/sessionConfig": {
                            "options": [
                                {"id": "grok-4.5", "category": "model"},
                                {"id": "high", "category": "mode"},
                            ]
                        }
                    },
                },
                True,
            ),
            (
                "kiro",
                {
                    "sessionId": "kiro-session",
                    "configOptions": [],
                    "models": {
                        "currentModelId": "auto",
                        "availableModels": [{"modelId": "auto"}],
                    },
                },
                False,
            ),
        )

        for label, session_new, slash_effort in fixtures:
            with self.subTest(label=label):
                report = probe.build_report(
                    probed_at="2026-07-13T01:02:03+00:00",
                    cmd=[label, "acp"],
                    cwd="/tmp/worktree",
                    version=f"{label} 1.0",
                    initialize={"protocolVersion": 1},
                    session_new=session_new,
                    notifications=[],
                    set_results=[{"method": "session/set_model", "status": "ok"}],
                )

                self.assertFalse(report["matrix"]["config_options_advertised"])
                self.assertFalse(report["matrix"]["model_select_advertised"])
                self.assertFalse(report["matrix"]["effort_select_advertised"])
                self.assertTrue(report["matrix"]["spur_slash_model"])
                self.assertEqual(report["matrix"]["spur_slash_effort"], slash_effort)

    def test_grouped_select_choices_are_counted(self) -> None:
        option = {
            "id": "model",
            "type": "select",
            "options": [
                {
                    "name": "Recommended",
                    "options": [{"value": "fast", "name": "Fast"}],
                }
            ],
        }

        summary = probe.summarize_config_options([option])

        self.assertEqual(summary["options"][0]["choice_count"], 1)
        self.assertEqual(summary["options"][0]["choices"][0]["value"], "fast")

    def test_cli_exposes_probe_vendor_rpc_flag(self) -> None:
        args = probe.parse_cli(
            [
                "--command",
                "agent",
                "--probe-vendor-rpc",
                "--vendor-method",
                "_x.ai/sessionConfig/update",
                "--vendor-method",
                "_kiro.dev/metadata",
            ]
        )
        self.assertTrue(args.probe_vendor_rpc)
        self.assertEqual(
            args.vendor_method,
            ["_x.ai/sessionConfig/update", "_kiro.dev/metadata"],
        )


class EvidenceContractTests(unittest.TestCase):
    def protocol_frames(self, arguments: dict) -> list[dict]:
        frames: list[dict] = []

        def add(direction: str, message: dict) -> None:
            frames.append(
                {
                    "sequence": len(frames),
                    "direction": direction,
                    "message": message,
                }
            )

        def add_request_result(source: str, index: int, record: dict) -> None:
            method = record.get("method")
            if not isinstance(method, str):
                return
            request_id = f"{source}-{index}"
            add(
                "send",
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": record.get("params") or {},
                },
            )
            response = record.get("response")
            if isinstance(response, dict):
                add("recv", {"jsonrpc": "2.0", "id": request_id, **response})

        initialize = arguments.get("initialize")
        if isinstance(initialize, dict):
            add(
                "send",
                {
                    "jsonrpc": "2.0",
                    "id": "initialize",
                    "method": "initialize",
                    "params": {"protocolVersion": 1},
                },
            )
            add(
                "recv",
                {"jsonrpc": "2.0", "id": "initialize", "result": initialize},
            )
        authentication = arguments.get("authentication")
        if isinstance(authentication, dict):
            add_request_result("authentication", 0, authentication)
        session_new = arguments.get("session_new")
        if isinstance(session_new, dict):
            add(
                "send",
                {
                    "jsonrpc": "2.0",
                    "id": "session-new",
                    "method": "session/new",
                    "params": {"sessionId": "session-1"},
                },
            )
            add(
                "recv",
                {"jsonrpc": "2.0", "id": "session-new", "result": session_new},
            )
        for notification in arguments.get("notifications") or []:
            if isinstance(notification, dict):
                add("recv", notification)
        for index, result in enumerate(arguments.get("set_results") or []):
            if isinstance(result, dict):
                add_request_result("set", index, result)
        prompt_result = arguments.get("prompt_result")
        if isinstance(prompt_result, dict):
            add_request_result("prompt", 0, prompt_result)
        for index, result in enumerate(arguments.get("vendor_rpc_results") or []):
            if isinstance(result, dict):
                add_request_result("vendor", index, result)
        return frames

    def build_contract_report(self, **overrides: object) -> dict:
        arguments = {
            "probed_at": "2026-09-01T10:00:00+00:00",
            "cmd": ["agent", "acp"],
            "cwd": "/tmp/worktree",
            "version": "agent 9.0.0",
            "initialize": {"protocolVersion": 1},
            "session_new": {"sessionId": "session-1"},
            "notifications": [],
            "set_results": [],
        }
        arguments.update(overrides)
        if "protocol_frames" not in arguments:
            arguments["protocol_frames"] = self.protocol_frames(arguments)
        return probe.build_report(**arguments)

    def artifact(self, report: dict) -> dict:
        artifact = report.get("artifact")
        self.assertIsInstance(artifact, dict)
        assert isinstance(artifact, dict)
        return artifact

    def test_report_adds_versioned_identity_raw_claim_and_fixture_contract(
        self,
    ) -> None:
        report = self.build_contract_report(
            notifications=[
                {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "available_commands_update",
                            "availableCommands": [{"name": "future-command"}],
                        }
                    },
                }
            ]
        )

        artifact = self.artifact(report)
        self.assertEqual(artifact["schema"], "spur.acp-capability-probe")
        self.assertEqual(artifact["version"], 1)
        self.assertEqual(
            set(artifact["cli_identity"]),
            {
                "resolved_executable",
                "upstream_version",
                "argv_fingerprint",
                "environment_fingerprint",
            },
        )
        raw = artifact["raw"]
        self.assertEqual(raw["digest_algorithm"], "sha256")
        self.assertTrue(raw["frames"])
        for frame in raw["frames"]:
            digest = frame["digest"]
            self.assertRegex(digest, r"^sha256:[0-9a-f]{64}$")
            self.assertIn(digest, raw["payloads_by_digest"])
            self.assertEqual(raw["payloads_by_digest"][digest], frame["message"])
        for claim in artifact["claims"]:
            self.assertIn(claim["raw_digest"], raw["payloads_by_digest"])
        self.assertEqual(artifact["fixture"]["schema"], "spur.acp-capability-fixture")
        self.assertEqual(artifact["fixture"]["version"], 1)
        self.assertRegex(artifact["fixture"]["digest"], r"^sha256:[0-9a-f]{64}$")

    def test_fixture_replays_interleaved_protocol_frames_in_exact_wire_order(
        self,
    ) -> None:
        protocol_frames = [
            {
                "sequence": 0,
                "direction": "send",
                "message": {
                    "jsonrpc": "2.0",
                    "id": "initialize-1",
                    "method": "initialize",
                    "params": {"protocolVersion": 1},
                },
            },
            {
                "sequence": 1,
                "direction": "recv",
                "message": {
                    "jsonrpc": "2.0",
                    "id": "initialize-1",
                    "result": {
                        "configOptions": [
                            select_option(
                                "wire-model",
                                category="model",
                                values=("model-from-wire",),
                            )
                        ]
                    },
                },
            },
            {
                "sequence": 2,
                "direction": "recv",
                "message": {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"update": {"sessionUpdate": "agent_message_chunk"}},
                },
            },
            {
                "sequence": 3,
                "direction": "recv",
                "message": {
                    "jsonrpc": "2.0",
                    "id": "permission-1",
                    "method": "session/request_permission",
                    "params": {"options": []},
                },
            },
            {
                "sequence": 4,
                "direction": "send",
                "message": {
                    "jsonrpc": "2.0",
                    "id": "permission-1",
                    "result": {"outcome": {"outcome": "selected"}},
                },
            },
        ]

        fixture = self.artifact(
            self.build_contract_report(protocol_frames=protocol_frames)
        )["fixture"]
        raw = fixture["raw"]

        self.assertNotIn("observations", raw)
        self.assertEqual(
            [
                (frame["sequence"], frame["direction"], frame["message"])
                for frame in raw["frames"]
            ],
            [
                (frame["sequence"], frame["direction"], frame["message"])
                for frame in protocol_frames
            ],
        )
        for frame in raw["frames"]:
            self.assertEqual(
                raw["payloads_by_digest"][frame["digest"]], frame["message"]
            )
        wire_claim = next(
            claim
            for claim in fixture["claims"]
            if claim["capability"] == {"kind": "model", "id": "model-from-wire"}
        )
        self.assertEqual(wire_claim["raw_digest"], raw["frames"][1]["digest"])

    def test_standard_and_vendor_session_planes_have_distinct_provenance(
        self,
    ) -> None:
        report = self.build_contract_report(
            session_new={
                "sessionId": "session-1",
                "configOptions": [
                    select_option(
                        "standard-model",
                        category="model",
                        values=("standard-model-id",),
                    )
                ],
                "modes": {"availableModes": [{"id": "standard-mode-id"}]},
                "models": {
                    "availableModels": [{"modelId": "kiro-model-id"}]
                },
                "_meta": {
                    "modelState": {
                        "availableModels": [{"modelId": "meta-model-id"}]
                    }
                },
            }
        )

        provenance = {
            (claim["capability"]["kind"], claim["capability"]["id"]): claim[
                "provenance"
            ]
            for claim in self.artifact(report)["claims"]
            if claim["claim"] == "advertised"
        }
        self.assertEqual(
            provenance[("model", "standard-model-id")], "standard_advertisement"
        )
        self.assertEqual(
            provenance[("mode", "standard-mode-id")], "standard_advertisement"
        )
        self.assertEqual(
            provenance[("model", "kiro-model-id")], "vendor_advertisement"
        )
        self.assertEqual(
            provenance[("model", "meta-model-id")], "vendor_advertisement"
        )

    def test_recipe_existence_alone_does_not_claim_method_support(self) -> None:
        options_method = "_vendor/commands/future-model/options"
        report = self.build_contract_report(
            notifications=[
                {
                    "method": "_vendor/commands/available",
                    "params": {
                        "commands": [
                            {
                                "name": "/future-model",
                                "meta": {"optionsMethod": options_method},
                            }
                        ]
                    },
                }
            ]
        )

        claims = self.artifact(report)["claims"]
        recipe_claims = [
            claim
            for claim in claims
            if claim["capability"] == {"kind": "method", "id": options_method}
        ]
        self.assertEqual(recipe_claims, [])
        self.assertIn(
            ("command", "/future-model", "advertised", "vendor_advertisement"),
            {
                (
                    claim["capability"]["kind"],
                    claim["capability"]["id"],
                    claim["claim"],
                    claim["provenance"],
                )
                for claim in claims
            },
        )

    def test_dynamic_choice_ids_come_from_payload_evidence(self) -> None:
        report = self.build_contract_report(
            initialize={
                "_meta": {
                    "modelState": {
                        "availableModels": [
                            {
                                "modelId": "model-from-init-payload",
                                "_meta": {
                                    "reasoningEfforts": [
                                        {"id": "effort-from-init-payload"}
                                    ]
                                },
                            }
                        ]
                    }
                }
            },
            session_new={
                "sessionId": "session-1",
                "models": {
                    "availableModels": [{"modelId": "model-from-session-payload"}]
                },
                "modes": {"availableModes": [{"id": "mode-from-session-payload"}]},
                "configOptions": [
                    select_option(
                        "dynamic-effort",
                        category="thought_level",
                        values=("effort-from-config-payload",),
                    )
                ],
            },
            notifications=[
                {
                    "method": "session/update",
                    "params": {
                        "update": {
                            "sessionUpdate": "available_commands_update",
                            "availableCommands": [
                                {"name": "command-from-notification-payload"}
                            ],
                        }
                    },
                }
            ],
        )

        observed = {
            (claim["capability"]["kind"], claim["capability"]["id"])
            for claim in self.artifact(report)["claims"]
            if claim["claim"] == "advertised"
        }
        self.assertTrue(
            {
                ("model", "model-from-init-payload"),
                ("model", "model-from-session-payload"),
                ("effort", "effort-from-init-payload"),
                ("effort", "effort-from-config-payload"),
                ("mode", "mode-from-session-payload"),
                ("command", "command-from-notification-payload"),
            }.issubset(observed)
        )

    def test_active_probe_outcomes_and_prompt_fallback_have_distinct_claims(
        self,
    ) -> None:
        report = self.build_contract_report(
            set_results=[
                {
                    "method": "session/set_model",
                    "status": "ok",
                    "response": {"result": {}},
                },
                {
                    "method": "session/set_mode",
                    "status": "error",
                    "response": {"error": {"code": -32602, "message": "bad mode"}},
                },
            ],
            prompt_result={
                "method": "session/prompt",
                "status": "ok",
                "response": {"result": {}},
            },
        )

        claims = {
            (claim["capability"]["id"], claim["claim"], claim["provenance"])
            for claim in self.artifact(report)["claims"]
        }
        self.assertIn(
            ("session/set_model", "accepted", "accepted_active_probe"), claims
        )
        self.assertIn(("session/set_mode", "rejected", "rejected_active_probe"), claims)
        self.assertIn(("session/prompt", "prompt_fallback", "prompt_fallback"), claims)

    def test_authentication_and_timeout_are_inconclusive(self) -> None:
        report = self.build_contract_report(
            authentication={
                "method": "authenticate",
                "status": "error",
                "response": {
                    "error": {"code": -32001, "message": "Authentication required"}
                },
            },
            set_results=[
                {
                    "method": "session/set_config_option",
                    "status": "timeout",
                    "response": None,
                }
            ],
            hard_failure="transport closed while authentication was required",
        )

        inconclusive = {
            (claim["source"], claim["capability"]["id"])
            for claim in self.artifact(report)["claims"]
            if claim["claim"] == "inconclusive"
            and claim["provenance"] == "inconclusive_failure"
        }
        self.assertIn(("authentication", "authenticate"), inconclusive)
        self.assertIn(("set_result", "session/set_config_option"), inconclusive)
        self.assertIn(("hard_failure", "probe"), inconclusive)

    def test_report_and_fixture_redact_secrets_before_digesting(self) -> None:
        secrets = (
            "cli-super-secret",
            "header-super-secret",
            "payload-super-secret",
            "api-super-secret",
            "message-super-secret",
        )
        report = self.build_contract_report(
            cmd=[
                "agent",
                "--token",
                secrets[0],
                "--header",
                f"Authorization: Bearer {secrets[1]}",
            ],
            initialize={
                "token": secrets[2],
                "nested": {
                    "apiKey": secrets[3],
                    "message": f"token={secrets[4]}",
                },
            },
            hard_failure=f"authentication failed token={secrets[4]}",
        )

        encoded = probe.json.dumps(report, sort_keys=True)
        for secret in secrets:
            self.assertNotIn(secret, encoded)
        self.assertIn("<redacted>", encoded)

    def test_raw_frame_log_redacts_secrets(self) -> None:
        client = object.__new__(probe.AcpClient)
        client._log_lock = probe.threading.Lock()
        client._log_file = io.StringIO()
        client.protocol_frames = []

        client._log(
            "recv",
            {
                "authorization": "Bearer raw-frame-secret",
                "message": "token=raw-message-secret",
            },
        )

        logged = client._log_file.getvalue()
        self.assertNotIn("raw-frame-secret", logged)
        self.assertNotIn("raw-message-secret", logged)
        self.assertIn("<redacted>", logged)
        self.assertEqual(client.protocol_frames[0]["sequence"], 0)
        self.assertEqual(client.protocol_frames[0]["direction"], "recv")
        self.assertNotIn(
            "raw-frame-secret", probe.json.dumps(client.protocol_frames)
        )

    def test_fixture_material_is_independent_of_probe_timestamp(self) -> None:
        first = self.build_contract_report(probed_at="2026-09-01T10:00:00+00:00")
        second = self.build_contract_report(probed_at="2026-09-01T11:00:00+00:00")

        self.assertEqual(
            self.artifact(first)["fixture"],
            self.artifact(second)["fixture"],
        )


if __name__ == "__main__":
    unittest.main()
