#!/usr/bin/env python3
"""Offline contract tests for the disposable Cognito POC harness."""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path
from urllib.parse import urlsplit


POC_ROOT = Path(__file__).resolve().parents[1]
HARNESS_FILES = (
    "README.md",
    "versions.tf",
    "variables.tf",
    "locals.tf",
    "iam.tf",
    "main.tf",
    "outputs.tf",
    "terraform.tfvars.example",
    "backends/poc.s3.tfbackend.example",
    "fixtures/evidence-cases.json",
    "fixtures/external-index-validation-only.json",
    "fixtures/empty-inventory.json",
    "fixtures/sanitized-plan-log.json",
    "scripts/offline-smoke.sh",
    "scripts/inventory.sh",
    "scripts/scan-secrets.py",
    "scripts/compare-production.sh",
    "scripts/verify-evidence.py",
    "scripts/verify-teardown.sh",
    "tests/poc.tftest.hcl",
)

REQUIRED_CASES = {
    "human_pkce_s256_success",
    "human_oidc_validation_success",
    "human_wrong_verifier_failure",
    "human_reused_state_failure",
    "m2m_basic_success",
    "m2m_token_cache_success",
    "m2m_read_scope_success",
    "m2m_index_scope_success",
    "m2m_status_scope_success",
    "m2m_missing_scope_failure",
    "missing_token_failure",
    "wrong_token_failure",
    "expired_token_failure",
    "id_token_rejection",
    "denylisted_client_rejection",
    "cross_owner_status_isolation",
    "secret_overlap_same_owner",
    "iam_legacy_compatibility",
    "anonymous_internal_compatibility",
    "token_endpoint_redirect_rejection",
    "validation_only_external_index",
}


def harness_text() -> str:
    parts = []
    for relative in HARNESS_FILES:
        path = POC_ROOT / relative
        if path.is_file():
            parts.append(path.read_text(encoding="utf-8"))
    return "\n".join(parts)


class PocHarnessStaticTests(unittest.TestCase):
    def test_required_harness_files_exist(self) -> None:
        missing = [relative for relative in HARNESS_FILES if not (POC_ROOT / relative).is_file()]
        self.assertEqual([], missing)

    def test_evidence_manifest_covers_the_approved_matrix(self) -> None:
        manifest = json.loads(
            (POC_ROOT / "fixtures/evidence-cases.json").read_text(encoding="utf-8")
        )
        case_ids = {case["id"] for case in manifest["cases"]}
        self.assertEqual(REQUIRED_CASES, case_ids)
        self.assertTrue(all(case["mode"] == "offline" for case in manifest["cases"]))

    def test_external_index_fixture_is_validation_only(self) -> None:
        fixture = json.loads(
            (POC_ROOT / "fixtures/external-index-validation-only.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual("external_index", fixture["params"]["name"])
        self.assertEqual(
            {"package", "revision", "source_url", "source_kind", "force"},
            set(fixture["params"]["arguments"]),
        )
        self.assertEqual(0, fixture["poc_assertions"]["queue_caps"]["running"])
        self.assertEqual(0, fixture["poc_assertions"]["queue_caps"]["queued"])
        self.assertTrue(
            fixture["params"]["arguments"]["source_url"].startswith(
                "https://validation-only.invalid/"
            )
        )

    def test_external_index_fixture_is_rejected_by_allowlist_before_dns(self) -> None:
        fixture = json.loads(
            (POC_ROOT / "fixtures/external-index-validation-only.json").read_text(
                encoding="utf-8"
            )
        )
        main = (POC_ROOT / "main.tf").read_text(encoding="utf-8")
        match = re.search(
            r'SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS\s*=\s*"([^"]*)"', main
        )
        self.assertIsNotNone(match)

        allowed_domains = [
            value.strip().lstrip(".").rstrip(".").lower()
            for value in match.group(1).split(",")
            if value.strip()
        ]
        source_host = urlsplit(
            fixture["params"]["arguments"]["source_url"]
        ).hostname

        self.assertEqual(["poc-no-source.invalid"], allowed_domains)
        self.assertEqual("validation-only.invalid", source_host)
        self.assertFalse(
            any(
                source_host == domain or source_host.endswith(f".{domain}")
                for domain in allowed_domains
            )
        )
        self.assertEqual(
            "source_url: source_url domain is not allow-listed",
            fixture["poc_assertions"]["expected_reason"],
        )
        self.assertTrue(fixture["poc_assertions"]["must_not_resolve_dns"])

    def test_validation_lambda_bootstraps_only_a_local_ephemeral_catalog(self) -> None:
        main = (POC_ROOT / "main.tf").read_text(encoding="utf-8")
        self.assertRegex(
            main,
            re.compile(
                r'SPUR_CATALOG_DSN\s*=\s*"ducklake:sqlite:/tmp/\$\{local\.name_prefix\}\.ducklake"'
            ),
        )
        self.assertNotIn("SPUR_CATALOG_S3_URI", main)

    def test_root_has_a_separate_backend_and_no_parent_module_reference(self) -> None:
        versions = (POC_ROOT / "versions.tf").read_text(encoding="utf-8")
        text = harness_text()
        self.assertIn('backend "s3"', versions)
        self.assertNotRegex(text, r"source\s*=\s*\"\.\./")
        self.assertNotIn("terraform_remote_state", text)

    def test_harness_contains_no_real_ids_or_production_references(self) -> None:
        text = harness_text()
        self.assertNotRegex(text, r"\b[0-9]{12}\b")
        self.assertNotRegex(text, r"AKIA[0-9A-Z]{16}")
        self.assertNotIn("wiilearn-spur", text)
        self.assertNotIn("spur-context-service/default/terraform.tfstate", text)
        self.assertNotIn("065285885105", text)

    def test_poc_contract_uses_unique_tags_and_zero_dispatch_caps(self) -> None:
        text = harness_text()
        for required in (
            "PocId",
            "CostCenter",
            "ManagedBy",
            "Environment",
            "SPUR_INDEX_MAX_RUNNING_JOBS_GLOBAL",
            "SPUR_INDEX_MAX_QUEUED_JOBS_GLOBAL",
        ):
            self.assertIn(required, text)
        self.assertRegex(text, re.compile(r'index_max_running_jobs_global\s*=\s*0'))
        self.assertRegex(text, re.compile(r'index_max_queued_jobs_global\s*=\s*0'))

    def test_secret_scanner_accepts_sanitized_evidence_and_rejects_credentials(self) -> None:
        scanner = POC_ROOT / "scripts/scan-secrets.py"
        clean = subprocess.run(
            [str(scanner), str(POC_ROOT / "fixtures/sanitized-plan-log.json")],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, clean.returncode, clean.stderr)

        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8") as unsafe:
            unsafe.write('{"message":"Authorization: Bearer eyJfixture.payload.signature"}')
            unsafe.flush()
            rejected = subprocess.run(
                [str(scanner), unsafe.name], check=False, capture_output=True, text=True
            )
        self.assertNotEqual(0, rejected.returncode)
        self.assertIn("credential-shaped", rejected.stderr)

    def test_teardown_verifier_accepts_the_reviewed_poc_id(self) -> None:
        verified = subprocess.run(
            [
                str(POC_ROOT / "scripts/verify-teardown.sh"),
                "fixture-empty",
                str(POC_ROOT / "fixtures/empty-inventory.json"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, verified.returncode, verified.stderr)

    def test_teardown_verifier_rejects_a_mismatched_poc_id(self) -> None:
        rejected = subprocess.run(
            [
                str(POC_ROOT / "scripts/verify-teardown.sh"),
                "different-poc",
                str(POC_ROOT / "fixtures/empty-inventory.json"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(0, rejected.returncode)
        self.assertIn("inventory poc_id does not match", rejected.stderr)

    def test_teardown_verifier_rejects_an_invalid_expected_poc_id(self) -> None:
        rejected = subprocess.run(
            [
                str(POC_ROOT / "scripts/verify-teardown.sh"),
                "invalid'suffix",
                str(POC_ROOT / "fixtures/empty-inventory.json"),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(0, rejected.returncode)
        self.assertIn("invalid POC suffix", rejected.stderr)

    def test_teardown_verifier_rejects_non_array_categories(self) -> None:
        clean_inventory = json.loads(
            (POC_ROOT / "fixtures/empty-inventory.json").read_text(encoding="utf-8")
        )
        for malformed_value in (None, ""):
            with self.subTest(malformed_value=malformed_value):
                malformed_inventory = {**clean_inventory, "lambda_aliases": malformed_value}
                with tempfile.NamedTemporaryFile(
                    mode="w", encoding="utf-8"
                ) as inventory_file:
                    json.dump(malformed_inventory, inventory_file)
                    inventory_file.flush()
                    rejected = subprocess.run(
                        [
                            str(POC_ROOT / "scripts/verify-teardown.sh"),
                            "fixture-empty",
                            inventory_file.name,
                        ],
                        check=False,
                        capture_output=True,
                        text=True,
                    )
                self.assertNotEqual(0, rejected.returncode)
                self.assertIn("inventory categories must be arrays", rejected.stderr)

    def test_inventory_rejects_an_invalid_suffix_before_aws(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fake_bin = Path(temporary_directory)
            aws_marker = fake_bin / "aws-called"
            fake_aws = fake_bin / "aws"
            fake_aws.write_text(
                '#!/bin/sh\n: > "$AWS_CALLED_MARKER"\nexit 99\n', encoding="utf-8"
            )
            fake_aws.chmod(0o755)
            fake_jq = fake_bin / "jq"
            fake_jq.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            fake_jq.chmod(0o755)

            rejected = subprocess.run(
                [
                    str(POC_ROOT / "scripts/inventory.sh"),
                    "fixture-profile",
                    "us-east-1",
                    "invalid'suffix",
                ],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "AWS_CALLED_MARKER": str(aws_marker),
                    "PATH": str(fake_bin),
                },
            )
            aws_was_called = aws_marker.exists()

        self.assertNotEqual(0, rejected.returncode)
        self.assertIn("invalid POC suffix", rejected.stderr)
        self.assertFalse(aws_was_called, "invalid suffix reached the AWS CLI")


if __name__ == "__main__":
    unittest.main()
