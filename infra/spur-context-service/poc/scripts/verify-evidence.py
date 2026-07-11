#!/usr/bin/env python3
"""Validate sanitized offline evidence fixtures without credentials or network."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
FORBIDDEN_VALUE_KEYS = {
    "access_token",
    "authorization_code",
    "client_secret",
    "id_token",
    "pkce_verifier",
    "refresh_token",
}
SECRET_PATTERNS = (
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"),
    re.compile(r"(?i)authorization\s*:\s*bearer\s+\S+"),
)


def fail(message: str) -> None:
    raise SystemExit(f"offline evidence check failed: {message}")


def walk(value: object, path: str = "$") -> None:
    if isinstance(value, dict):
        for key, nested in value.items():
            if key.lower() in FORBIDDEN_VALUE_KEYS:
                fail(f"secret-bearing key {path}.{key} is prohibited")
            walk(nested, f"{path}.{key}")
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            walk(nested, f"{path}[{index}]")
    elif isinstance(value, str):
        for pattern in SECRET_PATTERNS:
            if pattern.search(value):
                fail(f"credential-shaped value at {path}")


def main() -> int:
    manifest = json.loads((FIXTURES / "evidence-cases.json").read_text(encoding="utf-8"))
    request = json.loads(
        (FIXTURES / "external-index-validation-only.json").read_text(encoding="utf-8")
    )
    walk(manifest)
    walk(request)

    cases = {case["id"]: case for case in manifest["cases"]}
    if len(cases) != len(manifest["cases"]):
        fail("evidence case IDs must be unique")
    if any(case.get("mode") != "offline" for case in cases.values()):
        fail("every committed evidence case must be offline")

    overlap = cases["secret_overlap_same_owner"]
    if overlap["secret_descriptors"] != ["current", "next"]:
        fail("secret overlap records descriptors only")
    if overlap["expected_owner"] != f"cognito:client:{overlap['client_fixture']}":
        fail("secret rotation must preserve client ownership")

    owner_case = cases["cross_owner_status_isolation"]
    if owner_case["job_owner"] == owner_case["poll_owner"] or owner_case["expected"] != "not_found":
        fail("cross-owner status fixture must be non-enumerating")

    if set(request) != {"tool", "args", "poc_assertions"}:
        fail("validation fixture must use the direct OAuth body contract")
    arguments = request["args"]
    assertions = request["poc_assertions"]
    if request["tool"] != "external_index":
        fail("validation fixture must call external_index")
    if set(arguments) != {"package", "revision", "source_url", "source_kind", "force"}:
        fail("validation fixture arguments must exactly match the external_index schema")
    if not arguments["source_url"].startswith("https://validation-only.invalid/"):
        fail("validation fixture must use the reserved .invalid domain")
    if assertions["queue_caps"] != {"running": 0, "queued": 0}:
        fail("validation fixture must disable queueing and dispatch")
    if not all(assertions[key] for key in ("must_not_resolve_dns", "must_not_enqueue", "must_not_dispatch")):
        fail("validation fixture must stop before outbound effects")

    print(f"offline evidence verified: {len(cases)} sanitized cases")
    return 0


if __name__ == "__main__":
    sys.exit(main())
