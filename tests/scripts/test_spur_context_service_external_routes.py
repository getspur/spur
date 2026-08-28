import base64
import importlib.util
from pathlib import Path
import sys

import pytest


ROOT = Path(__file__).resolve().parents[2]
SMOKE_PATH = ROOT / "infra" / "spur-context-service" / "smoke-staging-e2e.py"


def load_smoke_module():
    spec = importlib.util.spec_from_file_location("spur_context_smoke", SMOKE_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_split_config_declares_both_serving_lambdas_and_log_groups():
    smoke = load_smoke_module()

    expected_fields = {
        "code_lambda_name",
        "knowledge_lambda_name",
        "code_log_group",
        "knowledge_log_group",
    }
    assert expected_fields <= set(smoke.SmokeConfig.__dataclass_fields__)


def test_external_tool_routes_are_complete_and_exact():
    smoke = load_smoke_module()
    assert hasattr(smoke, "serving_backend_for_tool")

    code_tools = {
        "external_index",
        "external_index_status",
        "external_catalog",
        "external_code_search",
        "external_code_read",
        "external_code_callers",
        "external_code_callees",
    }
    for tool in code_tools:
        assert smoke.serving_backend_for_tool(tool) == "code"
    assert smoke.serving_backend_for_tool("external_knowledge_context") == "knowledge"
    with pytest.raises(smoke.SmokeFailure, match="no serving backend"):
        smoke.serving_backend_for_tool("unknown_external_tool")


class FixtureInvoker:
    def __init__(self):
        self.calls = []

    def call_tool(self, tool, args):
        self.calls.append((tool, args))
        selector = "pkg-symbol://registry:crates-io/demo/1.0.0/abc123"
        responses = {
            "external_catalog": {
                "level": "revisions",
                "catalog_generation": 7,
                "rows": [{"revision": "1.0.0", "refs": []}],
            },
            "external_code_search": {
                "catalog_generation": 7,
                "candidates": [
                    {
                        "uri": selector,
                        "selector": "pkg:demo@1.0.0::demo::target_symbol",
                        "stable_symbol_id": "abc123",
                        "package": "demo",
                        "revision": "1.0.0",
                    }
                ],
            },
            "external_code_read": {
                "catalog_generation": 7,
                "stable_symbol_id": "abc123",
                "selector": "pkg:demo@1.0.0::demo::target_symbol",
                "package": "demo",
                "revision": "1.0.0",
                "source": "pub fn target_symbol() {}",
            },
            "external_code_callers": {
                "catalog_generation": 7,
                "stable_symbol_id": "abc123",
                "callers": [],
                "counts_by_kind": {"calls": 0, "unresolved": 0},
                "unresolved_sample": [],
            },
            "external_code_callees": {
                "catalog_generation": 7,
                "stable_symbol_id": "abc123",
                "callees": [],
                "counts_by_kind": {"calls": 0, "unresolved": 0},
                "unresolved_sample": [],
            },
            "external_knowledge_context": {
                "catalog_generation": 7,
                "answerable": True,
                "primary_evidence": [
                    {
                        "grounding": "hybrid-code",
                        "stable_symbol_id": "abc123",
                        "uri": selector,
                    }
                ],
            },
        }
        return responses[tool]


def test_serving_chain_reuses_search_identity_for_read_edges_and_knowledge():
    smoke = load_smoke_module()
    invoker = FixtureInvoker()

    smoke.assert_serving_queries(
        invoker,
        "registry:crates-io",
        "demo",
        "1.0.0",
        "fixture-run",
        "target_symbol",
        expect_embeddings=True,
    )

    assert [tool for tool, _ in invoker.calls] == [
        "external_catalog",
        "external_code_search",
        "external_code_read",
        "external_code_callers",
        "external_code_callees",
        "external_knowledge_context",
    ]
    selector = invoker.calls[1][1]["query"]
    assert selector == "target_symbol"
    stable_uri = "pkg-symbol://registry:crates-io/demo/1.0.0/abc123"
    assert invoker.calls[2][1]["selector"] == stable_uri
    assert invoker.calls[3][1]["selector"] == stable_uri
    assert invoker.calls[4][1]["selector"] == stable_uri
    assert invoker.calls[5][1]["package"] == "demo"
    assert invoker.calls[5][1]["revision"] == "1.0.0"


def test_lambda_log_tail_yields_exact_runtime_request_id():
    smoke = load_smoke_module()
    assert hasattr(smoke, "lambda_request_id_from_log_tail")

    request_id = "11111111-2222-4333-8444-555555555555"
    log_tail = base64.b64encode(
        (
            f"START RequestId: {request_id} Version: $LATEST\n"
            f"END RequestId: {request_id}\n"
        ).encode()
    ).decode()
    assert smoke.lambda_request_id_from_log_tail(log_tail) == request_id
    with pytest.raises(smoke.SmokeFailure, match="exactly one Lambda START"):
        smoke.lambda_request_id_from_log_tail("")
