#!/usr/bin/env python3
"""Unit tests for scripts/solve_topology_conformance pure helpers (no spur binary required)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import solve_topology_conformance as stc


def _pkg(name: str, deps: list[tuple[str, str | None, bool, list[str]]]) -> dict:
    return {
        "name": name,
        "dependencies": [
            {"name": d, "kind": kind, "optional": optional, "features": features}
            for d, kind, optional, features in deps
        ],
    }


def _base_request() -> dict:
    return {
        "vars": [
            {"type": "bool", "name": "m_acp"},
            {"type": "bool", "name": "tui_m"},
            {"type": "bool", "name": "local_injected"},
        ],
        "constraints": [
            {"id": "mentions_acp_neutral_direct", "soft": False,
             "expr": {"kind": "op", "op": "not", "args": [{"kind": "var", "name": "m_acp"}]}},
            {"id": "s1_pref", "soft": True, "weight": 10,
             "expr": {"kind": "var", "name": "tui_m"}},
            {"id": "cex_any_derived_property_fails", "soft": False,
             "expr": {"kind": "var", "name": "tui_m"}},
        ],
        "objectives": [],
        "persist": True,
    }


class ObserveEdgesTests(unittest.TestCase):
    def test_current_head_observes_only_existing_sources(self) -> None:
        metadata = {"packages": [
            _pkg("spur-graph", [("spur-mcp", None, False, [])]),
            _pkg("spur-mcp", [("spur-acp", None, False, [])]),
            _pkg("spur-tui", [("spur-acp", None, False, []), ("spur-graph", None, False, [])]),
        ]}
        observed = stc.observe_edges(metadata)
        self.assertTrue(observed["g_mcp"])
        self.assertFalse(observed["g_acp"])
        self.assertTrue(observed["mcp_acp"])
        self.assertFalse(observed["tui_m"])
        self.assertFalse(observed["tui_code"])
        # spur-mentions / spur-commands / spur-utilities do not exist yet:
        # their outgoing edges are undecided, not false.
        for var in ("m_acp", "m_g", "c_acp", "c_m", "u_m", "u_c"):
            self.assertNotIn(var, observed)

    def test_feature_and_optional_edges(self) -> None:
        metadata = {"packages": [
            _pkg("spur-tui", [("spur-mentions", None, False, ["code"]),
                              ("spur-commands", None, False, [])]),
            _pkg("spur-mentions", [("spur-graph", None, True, []),
                                   ("spur-acp", "dev", False, [])]),
            _pkg("spur-commands", [("spur-acp", None, False, [])]),
            _pkg("spur-utilities", [("spur-mentions", None, False, []),
                                    ("spur-commands", None, False, [])]),
        ]}
        observed = stc.observe_edges(metadata)
        self.assertTrue(observed["tui_m"])
        self.assertTrue(observed["tui_c"])
        self.assertTrue(observed["tui_code"])
        self.assertFalse(observed["tui_u"])
        self.assertTrue(observed["m_g"], "optional dep behind `code` is still an edge")
        self.assertFalse(observed["m_acp"], "dev-dependencies are not build edges")
        self.assertTrue(observed["c_acp"])
        self.assertFalse(observed["c_m"])
        self.assertTrue(observed["u_m"])
        self.assertTrue(observed["u_c"])
        self.assertFalse(observed["u_acp"])

    def test_renamed_dependency_uses_package_name(self) -> None:
        metadata = {"packages": [
            {"name": "spur-mentions", "dependencies": [
                {"name": "spur-acp", "rename": "acp", "kind": None,
                 "optional": False, "features": []}]},
        ]}
        self.assertTrue(stc.observe_edges(metadata)["m_acp"])


class ObserveLocalInjectedTests(unittest.TestCase):
    def test_production_reference_means_not_injected(self) -> None:
        text = "use super::spur_local::SpurLocalSource;\nfn f() { SpurLocalSource::entries(); }\n"
        self.assertFalse(stc.observe_local_injected(text))

    def test_layer_without_production_reference_is_injected(self) -> None:
        text = (
            "pub struct LocalLayer;\nfn f(l: LocalLayer) {}\n"
            "#[cfg(test)]\nmod tests { use SpurLocalSource; }\n"
        )
        self.assertTrue(stc.observe_local_injected(text))

    def test_neither_reference_is_undecided(self) -> None:
        self.assertIsNone(stc.observe_local_injected("fn f() {}\n"))


class SelectFixedTests(unittest.TestCase):
    def test_incremental_fixes_only_present_facts(self) -> None:
        observed = {"tui_m": False, "m_acp": True, "g_mcp": True}
        self.assertEqual(stc.select_fixed(observed, "incremental"),
                         {"m_acp": True, "g_mcp": True})

    def test_final_fixes_everything_observed(self) -> None:
        observed = {"tui_m": False, "m_acp": True}
        self.assertEqual(stc.select_fixed(observed, "final"), observed)

    def test_unknown_mode_rejected(self) -> None:
        with self.assertRaises(ValueError):
            stc.select_fixed({}, "sometimes")


class BuildRequestTests(unittest.TestCase):
    def test_drops_soft_and_counterexample_and_appends_facts(self) -> None:
        request = stc.build_request(_base_request(), {"m_acp": False}, {"tui_m": True})
        ids = [c["id"] for c in request["constraints"]]
        self.assertEqual(ids, ["mentions_acp_neutral_direct", "obs_m_acp", "assume_tui_m"])
        self.assertEqual(request["constraints"][1]["expr"],
                         {"kind": "op", "op": "not", "args": [{"kind": "var", "name": "m_acp"}]})
        self.assertEqual(request["constraints"][2]["expr"], {"kind": "var", "name": "tui_m"})
        self.assertTrue(all(not c.get("soft") for c in request["constraints"]))
        self.assertEqual(request["vars"], _base_request()["vars"])

    def test_does_not_mutate_base(self) -> None:
        base = _base_request()
        stc.build_request(base, {"m_acp": True}, {})
        self.assertEqual(base, _base_request())

    def test_unknown_variable_rejected(self) -> None:
        with self.assertRaises(KeyError):
            stc.build_request(_base_request(), {"nope": True}, {})


class ParseAssumeTests(unittest.TestCase):
    def test_parses_bool_assignment(self) -> None:
        self.assertEqual(stc.parse_assume("m_acp=true"), ("m_acp", True))
        self.assertEqual(stc.parse_assume("tui_m=False"), ("tui_m", False))

    def test_rejects_malformed(self) -> None:
        for bad in ("m_acp", "m_acp=yes", "=true"):
            with self.assertRaises(ValueError):
                stc.parse_assume(bad)


class InterpretTests(unittest.TestCase):
    def test_status_to_exit_code(self) -> None:
        self.assertEqual(stc.exit_code({"status": "sat"}), 0)
        self.assertEqual(stc.exit_code({"status": "unsat"}), 1)
        for status in ("unknown", "timeout", "error", "ended"):
            self.assertEqual(stc.exit_code({"status": status}), 2,
                             f"{status} is inconclusive, never pass or fail")


if __name__ == "__main__":
    unittest.main()
