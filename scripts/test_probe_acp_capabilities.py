#!/usr/bin/env python3
"""Unit tests for the generic ACP capability probe (no live agent required)."""

from __future__ import annotations

import io
import unittest
from contextlib import redirect_stderr

import probe_acp_capabilities as probe


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

    def test_grok_current_model_state_predicts_effort_without_session_mode(self) -> None:
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
                                "_meta": {
                                    "reasoningEfforts": [{"id": "high"}]
                                },
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
        self.assertEqual(
            prediction["effort_config_option_id"], "reasoning_effort"
        )
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
        self.assertTrue(probe.is_vendor_extension_method("_kiro.dev/commands/available"))
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
        model = next(item for item in harvest["commands_catalog"] if item["name"] == "/model")
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
        set_mode = next(item for item in targets if item["method"] == "session/set_mode")
        self.assertEqual(set_mode["params"]["modeId"], "kiro_planner")
        set_model = next(item for item in targets if item["method"] == "session/set_model")
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
                    set_results=[
                        {"method": "session/set_model", "status": "ok"}
                    ],
                )

                self.assertFalse(report["matrix"]["config_options_advertised"])
                self.assertFalse(report["matrix"]["model_select_advertised"])
                self.assertFalse(report["matrix"]["effort_select_advertised"])
                self.assertTrue(report["matrix"]["spur_slash_model"])
                self.assertEqual(
                    report["matrix"]["spur_slash_effort"], slash_effort
                )

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


if __name__ == "__main__":
    unittest.main()
