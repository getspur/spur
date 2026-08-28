import json
import re
import sys
from collections import Counter
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[2]
INFRA_DIR = ROOT / "infra" / "spur-context-service"

REVIEWED_NON_NOOP_ACTIONS = {
    "aws_apigatewayv2_integration.api_key[0]": ("update",),
    "aws_apigatewayv2_integration.api_key_knowledge[0]": ("create",),
    "aws_apigatewayv2_integration.code": ("create",),
    "aws_apigatewayv2_integration.lambda": ("update",),
    "aws_apigatewayv2_route.api_key_discovery[0]": ("update",),
    'aws_apigatewayv2_route.api_key_management["DELETE /auth/api-keys/{key_id}"]': (
        "update",
    ),
    'aws_apigatewayv2_route.api_key_management["GET /auth/api-keys"]': (
        "update",
    ),
    'aws_apigatewayv2_route.api_key_management["POST /auth/api-keys"]': (
        "update",
    ),
    "aws_apigatewayv2_route.api_key_mcp[0]": ("update",),
    "aws_apigatewayv2_route.api_key_mcp_knowledge[0]": ("create",),
    "aws_apigatewayv2_route.compatibility_code": ("create",),
    "aws_apigatewayv2_route.compatibility_knowledge": ("update",),
    "aws_apigatewayv2_route.login_redirect[0]": ("update",),
    "aws_apigatewayv2_route.oauth_code[0]": ("create",),
    "aws_apigatewayv2_route.oauth_knowledge[0]": ("update",),
    "aws_cloudwatch_event_target.index_queue_drainer": ("update",),
    "aws_cloudwatch_log_group.code_lambda": ("create",),
    "aws_cloudwatch_log_group.knowledge_lambda": ("create",),
    "aws_cloudwatch_log_metric_filter.api_key_route_5xx[0]": ("update",),
    "aws_cloudwatch_log_metric_filter.oauth_route_5xx[0]": ("update",),
    "aws_cloudwatch_metric_alarm.api_key_route_5xx[0]": ("update",),
    "aws_cloudwatch_metric_alarm.oauth_route_5xx[0]": ("update",),
    "aws_iam_role.code_lambda": ("create",),
    "aws_iam_role.knowledge_lambda": ("create",),
    "aws_iam_role_policy.api_key_authorizer[0]": ("update",),
    "aws_iam_role_policy.api_key_cleanup[0]": ("update",),
    "aws_iam_role_policy.code_api_key_management[0]": ("create",),
    "aws_iam_role_policy.code_lambda_dynamodb": ("create",),
    "aws_iam_role_policy.code_lambda_runtime": ("create",),
    "aws_iam_role_policy.code_lambda_sfn": ("create",),
    "aws_iam_role_policy.code_s3_read": ("create",),
    "aws_iam_role_policy.knowledge_lambda_runtime": ("create",),
    "aws_iam_role_policy.knowledge_s3_read": ("create",),
    "aws_iam_role_policy.lambda_dynamodb": ("update",),
    "aws_iam_role_policy.shared_lambda_catalog_secret": ("create",),
    "aws_iam_role_policy.source_fetcher_lambda": ("update",),
    "aws_iam_role_policy_attachment.lambda_vpc_access": ("create",),
    "aws_lambda_alias.api_key_authorizer[0]": ("update",),
    "aws_lambda_alias.knowledge_live": ("create",),
    "aws_lambda_alias.source_fetcher_live": ("update",),
    "aws_lambda_alias.worker_live": ("update",),
    "aws_lambda_function.api_key_authorizer[0]": ("update",),
    "aws_lambda_function.api_key_cleanup[0]": ("update",),
    "aws_lambda_function.code": ("create",),
    "aws_lambda_function.knowledge": ("create",),
    "aws_lambda_function.source_fetcher": ("update",),
    "aws_lambda_function.worker": ("update",),
    "aws_lambda_permission.apigw_code": ("create",),
    "aws_lambda_permission.apigw_knowledge": ("create",),
    "aws_lambda_permission.eventbridge_code": ("create",),
    "aws_s3_object.code_lambda_zip": ("create",),
    "aws_s3_object.knowledge_lambda_zip": ("create",),
    "aws_sfn_state_machine.index_build": ("update",),
}


def terraform_files():
    return sorted(INFRA_DIR.glob("*.tf"))


def terraform_text():
    return "\n".join(path.read_text() for path in terraform_files())


def terraform_resource_block(text, resource_type, resource_name):
    marker = f'resource "{resource_type}" "{resource_name}"'
    assert marker in text, f"missing Terraform resource: {marker}"
    start = text.index(marker)
    brace = text.index("{", start)
    depth = 0
    for offset, character in enumerate(text[brace:], start=brace):
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return text[start : offset + 1]
    raise AssertionError(f"unterminated Terraform resource: {marker}")


def backend_assignments(path):
    assignments = {}
    for line in path.read_text().splitlines():
        match = re.match(r'^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"?([^"#]+?)"?\s*$', line)
        if match:
            assignments[match.group(1)] = match.group(2).strip()
    return assignments


def moved_pairs(text):
    pairs = []
    for block in re.findall(r"(?ms)^moved\s*\{(.*?)^\}", text):
        source = re.search(r"(?m)^\s*from\s*=\s*([^\s]+)\s*$", block)
        target = re.search(r"(?m)^\s*to\s*=\s*([^\s]+)\s*$", block)
        if source and target:
            pairs.append((source.group(1), target.group(1)))
    return pairs


def managed_resource_addresses(text):
    return {
        f"{resource_type}.{resource_name}"
        for resource_type, resource_name in re.findall(
            r'resource\s+"([^"]+)"\s+"([^"]+)"', text
        )
    }


def destructive_plan_changes(plan):
    destructive = []
    for resource_change in plan.get("resource_changes", []):
        actions = resource_change.get("change", {}).get("actions", [])
        if "delete" in actions:
            destructive.append((resource_change.get("address", "<unknown>"), actions))
    return destructive


def saved_plan_action_matrix(plan):
    return Counter(
        tuple(resource_change.get("change", {}).get("actions", []))
        for resource_change in plan.get("resource_changes", [])
    )


def reviewed_non_noop_plan():
    return {
        "resource_changes": [
            {"address": address, "change": {"actions": list(actions)}}
            for address, actions in REVIEWED_NON_NOOP_ACTIONS.items()
        ]
    }


def assert_saved_plan_is_non_destructive(plan):
    destructive = destructive_plan_changes(plan)
    if destructive:
        summary = ", ".join(
            f"{address}={'/'.join(actions)}" for address, actions in destructive
        )
        raise AssertionError(f"saved plan contains delete or replacement actions: {summary}")

    resource_changes = plan.get("resource_changes", [])
    addresses = [
        resource_change.get("address", "<unknown>")
        for resource_change in resource_changes
    ]
    duplicates = sorted(
        address for address, count in Counter(addresses).items() if count > 1
    )
    if duplicates:
        raise AssertionError(f"saved plan contains duplicate addresses: {', '.join(duplicates)}")

    actual_actions = {
        resource_change.get("address", "<unknown>"): tuple(
            resource_change.get("change", {}).get("actions", [])
        )
        for resource_change in resource_changes
    }
    changed = sorted(
        address
        for address, expected in REVIEWED_NON_NOOP_ACTIONS.items()
        if address in actual_actions and actual_actions[address] != expected
    )
    missing = sorted(REVIEWED_NON_NOOP_ACTIONS.keys() - actual_actions.keys())
    unexpected = sorted(
        address
        for address, actions in actual_actions.items()
        if actions not in {("no-op",), ("read",)}
        and address not in REVIEWED_NON_NOOP_ACTIONS
    )

    mismatches = []
    if changed:
        mismatches.append(
            "action-changed non-no-op rows: "
            + ", ".join(
                f"{address} expected={'/'.join(REVIEWED_NON_NOOP_ACTIONS[address])} "
                f"actual={'/'.join(actual_actions[address])}"
                for address in changed
            )
        )
    if missing:
        mismatches.append(
            "missing reviewed non-no-op rows: "
            + ", ".join(
                f"{address}={'/'.join(REVIEWED_NON_NOOP_ACTIONS[address])}"
                for address in missing
            )
        )
    if unexpected:
        mismatches.append(
            "unexpected non-no-op rows: "
            + ", ".join(
                f"{address}={'/'.join(actual_actions[address])}"
                for address in unexpected
            )
        )
    if mismatches:
        raise AssertionError(
            "saved plan non-no-op allowlist mismatch: " + "; ".join(mismatches)
        )


def test_staging_backend_keeps_the_versioned_legacy_key_canonical():
    legacy = backend_assignments(INFRA_DIR / "backends" / "default.s3.tfbackend")
    staging = backend_assignments(INFRA_DIR / "backends" / "staging.s3.tfbackend")

    for field in ("bucket", "key", "region", "dynamodb_table", "encrypt"):
        if staging.get(field) != legacy.get(field):
            pytest.fail(
                f"staging backend field {field!r} must reuse the canonical legacy backend"
            )


def test_exact_legacy_route_moves_have_single_owners():
    pairs = moved_pairs(terraform_text())
    expected = {
        (
            "aws_apigatewayv2_route.default",
            "aws_apigatewayv2_route.compatibility_knowledge",
        ),
        (
            "aws_apigatewayv2_route.oauth",
            "aws_apigatewayv2_route.oauth_knowledge",
        ),
    }

    assert set(pairs) == expected
    assert len(pairs) == len(expected)
    assert len({source for source, _ in pairs}) == len(expected)
    assert len({target for _, target in pairs}) == len(expected)


def test_legacy_and_data_resource_addresses_remain_owned():
    addresses = managed_resource_addresses(terraform_text())
    retained_legacy = {
        "aws_cloudwatch_log_group.lambda",
        "aws_cloudwatch_log_group.sfn",
        "aws_iam_role.worker_lambda",
        "aws_iam_role_policy.lambda_xray",
        "aws_iam_role_policy.sfn_observability",
        "aws_iam_role_policy.worker_lambda_dynamodb",
        "aws_iam_role_policy.worker_lambda_observability",
        "aws_iam_role_policy.worker_lambda_s3",
        "aws_iam_role_policy.worker_lambda_vpc",
        "aws_iam_role_policy.worker_lambda_vpc_function_deny",
        "aws_lambda_function.service",
    }
    data_resources = {
        "aws_s3_bucket.data",
        "aws_s3_bucket_versioning.data",
        "aws_s3_bucket_lifecycle_configuration.data",
        "aws_s3_bucket_ownership_controls.data",
        "aws_dynamodb_table.index_jobs",
        "aws_dynamodb_table.catalog_leases",
        "aws_dynamodb_table.api_keys",
        "aws_db_subnet_group.catalog",
        "aws_rds_cluster.catalog",
        "aws_rds_cluster_instance.catalog_writer",
    }

    assert retained_legacy <= addresses
    assert data_resources <= addresses


def test_legacy_packages_and_permissions_are_not_reassigned():
    text = terraform_text()
    legacy_object = terraform_resource_block(text, "aws_s3_object", "lambda_zip")
    knowledge_object = terraform_resource_block(
        text, "aws_s3_object", "knowledge_lambda_zip"
    )
    legacy_apigw = terraform_resource_block(text, "aws_lambda_permission", "apigw")
    knowledge_apigw = terraform_resource_block(
        text, "aws_lambda_permission", "apigw_knowledge"
    )
    legacy_drainer = terraform_resource_block(
        text, "aws_lambda_permission", "eventbridge_drainer"
    )
    code_drainer = terraform_resource_block(
        text, "aws_lambda_permission", "eventbridge_code"
    )

    assert "var.lambda_zip_path" in legacy_object
    assert "knowledge_lambda_zip_path" not in legacy_object
    assert "local.knowledge_lambda_zip_path" in knowledge_object
    assert "aws_lambda_function.service.function_name" in legacy_apigw
    assert "aws_lambda_function.knowledge.function_name" in knowledge_apigw
    assert "aws_lambda_alias.knowledge_live.name" in knowledge_apigw
    assert "aws_lambda_function.service.function_name" in legacy_drainer
    assert "aws_lambda_function.code.function_name" in code_drainer


def test_split_routes_and_outputs_never_target_legacy_serving():
    main_tf = (INFRA_DIR / "main.tf").read_text()
    api_keys_tf = (INFRA_DIR / "api_keys.tf").read_text()
    outputs_tf = (INFRA_DIR / "outputs.tf").read_text()

    for route_name in (
        "compatibility_code",
        "compatibility_knowledge",
        "oauth_code",
        "oauth_knowledge",
    ):
        route = terraform_resource_block(
            main_tf, "aws_apigatewayv2_route", route_name
        )
        assert "aws_lambda_function.service" not in route

    for route_name in ("api_key_mcp", "api_key_mcp_knowledge"):
        route = terraform_resource_block(
            api_keys_tf, "aws_apigatewayv2_route", route_name
        )
        assert "aws_lambda_function.service" not in route

    for output_name, resource_name in (
        ("code_lambda_function_name", "code"),
        ("knowledge_lambda_function_name", "knowledge"),
        ("oauth_api_urls", None),
        ("api_key_mcp_urls", None),
    ):
        assert f'output "{output_name}"' in outputs_tf
        if resource_name:
            assert f"aws_lambda_function.{resource_name}." in outputs_tf


def test_split_serving_concurrency_is_knowledge_only_and_at_most_one():
    main_tf = (INFRA_DIR / "main.tf").read_text()
    variables_tf = (INFRA_DIR / "variables.tf").read_text()
    code = terraform_resource_block(main_tf, "aws_lambda_function", "code")
    knowledge_warm = terraform_resource_block(
        main_tf, "aws_lambda_provisioned_concurrency_config", "knowledge_warm"
    )
    warm_variable = variables_tf.split(
        'variable "concurrent_warm_instances"', 1
    )[1].split("\n}", 1)[0]

    assert "reserved_concurrent_executions" not in code
    assert "provisioned_concurrency" not in code
    assert "aws_lambda_function.knowledge.function_name" in knowledge_warm
    assert "var.concurrent_warm_instances <= 1" in warm_variable


def test_network_is_reused_without_new_singleton_endpoint_owners():
    text = terraform_text()
    network_tf = (INFRA_DIR / "network.tf").read_text()
    endpoints_tf = (INFRA_DIR / "vpc_endpoints.tf").read_text()

    assert 'data "aws_vpc" "selected"' in network_tf
    assert 'data "aws_subnets" "worker"' in network_tf
    assert 'data "aws_route_tables" "worker"' in network_tf
    for forbidden in (
        "aws_vpc",
        "aws_subnet",
        "aws_route_table",
        "aws_nat_gateway",
        "aws_internet_gateway",
    ):
        assert not re.search(rf'resource\s+"{forbidden}"', text)

    endpoint_owners = re.findall(
        r'resource\s+"aws_vpc_endpoint"\s+"([^"]+)"', endpoints_tf
    )
    assert endpoint_owners == ["gateway", "interface"]
    assert 'for_each = var.create_vpc_endpoints ? local.gateway_vpc_endpoint_services : {}' in endpoints_tf
    assert 'for_each = var.create_vpc_endpoints ? local.interface_vpc_endpoint_services : {}' in endpoints_tf


@pytest.mark.parametrize(
    "actions",
    (["delete"], ["delete", "create"], ["create", "delete"]),
)
def test_saved_plan_guard_rejects_delete_and_replacement_sequences(actions):
    plan = {
        "resource_changes": [
            {"address": "aws_example.legacy", "change": {"actions": actions}}
        ]
    }

    with pytest.raises(AssertionError, match="delete or replacement"):
        assert_saved_plan_is_non_destructive(plan)


def test_saved_plan_guard_accepts_non_destructive_action_matrix():
    plan = reviewed_non_noop_plan()
    plan["resource_changes"].extend(
        [
            {"address": "aws_example.keep", "change": {"actions": ["no-op"]}},
            {"address": "data.aws_example.read", "change": {"actions": ["read"]}},
        ]
    )

    assert_saved_plan_is_non_destructive(plan)
    assert saved_plan_action_matrix(plan) == {
        ("no-op",): 1,
        ("create",): 26,
        ("update",): 27,
        ("read",): 1,
    }


def test_reviewed_non_noop_allowlist_has_expected_action_counts():
    assert Counter(REVIEWED_NON_NOOP_ACTIONS.values()) == {
        ("create",): 26,
        ("update",): 27,
    }


def test_saved_plan_guard_rejects_unexpected_create_address():
    plan = reviewed_non_noop_plan()
    plan["resource_changes"].append(
        {
            "address": "aws_example.unexpected_create",
            "change": {"actions": ["create"]},
        }
    )

    with pytest.raises(
        AssertionError,
        match=r"unexpected.*aws_example\.unexpected_create=create",
    ):
        assert_saved_plan_is_non_destructive(plan)


def test_saved_plan_guard_rejects_unexpected_update_address():
    plan = reviewed_non_noop_plan()
    plan["resource_changes"].append(
        {
            "address": "aws_example.unexpected_update",
            "change": {"actions": ["update"]},
        }
    )

    with pytest.raises(
        AssertionError,
        match=r"unexpected.*aws_example\.unexpected_update=update",
    ):
        assert_saved_plan_is_non_destructive(plan)


def test_saved_plan_guard_rejects_reviewed_address_with_changed_action():
    plan = reviewed_non_noop_plan()
    changed_address = "aws_apigatewayv2_integration.api_key_knowledge[0]"
    for resource_change in plan["resource_changes"]:
        if resource_change["address"] == changed_address:
            resource_change["change"]["actions"] = ["update"]
            break

    with pytest.raises(
        AssertionError,
        match=r"action-changed.*api_key_knowledge\[0\].*expected=create actual=update",
    ):
        assert_saved_plan_is_non_destructive(plan)


def test_saved_plan_guard_rejects_missing_reviewed_non_noop_address():
    plan = reviewed_non_noop_plan()
    missing_address = "aws_apigatewayv2_integration.api_key_knowledge[0]"
    plan["resource_changes"] = [
        resource_change
        for resource_change in plan["resource_changes"]
        if resource_change["address"] != missing_address
    ]

    with pytest.raises(
        AssertionError,
        match=r"missing.*api_key_knowledge\[0\]=create",
    ):
        assert_saved_plan_is_non_destructive(plan)


def test_saved_plan_guard_rejects_duplicate_address():
    plan = reviewed_non_noop_plan()
    plan["resource_changes"].append(plan["resource_changes"][0].copy())

    with pytest.raises(
        AssertionError,
        match=r"duplicate.*aws_apigatewayv2_integration\.api_key\[0\]",
    ):
        assert_saved_plan_is_non_destructive(plan)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test_spur_context_service_reconciliation.py PLAN.json")
    saved_plan = json.loads(Path(sys.argv[1]).read_text())
    assert_saved_plan_is_non_destructive(saved_plan)
    matrix = saved_plan_action_matrix(saved_plan)
    print(
        json.dumps(
            {"/".join(actions): count for actions, count in sorted(matrix.items())},
            sort_keys=True,
        )
    )
