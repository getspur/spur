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
                {"method": "_vendor/settings/update", "params": {}},
            ],
            set_results=[],
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
            "matrix",
        ):
            self.assertIn(key, report)
        self.assertEqual(report["available_commands"], [{"name": "compact"}])
        self.assertEqual(report["extension_methods"], ["_vendor/settings/update"])
        self.assertTrue(report["meta_planes"])
        self.assertTrue(report["matrix"]["spur_slash_model"])

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


if __name__ == "__main__":
    unittest.main()
