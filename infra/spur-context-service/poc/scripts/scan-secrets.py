#!/usr/bin/env python3
"""Reject credential-shaped values in sanitized POC plan/log evidence."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


PATTERNS = (
    re.compile(r"AKIA[0-9A-Z]{16}"),
    re.compile(r"(?i)authorization\s*[:=]\s*(bearer|basic)\s+[A-Za-z0-9._~+/=-]+"),
    re.compile(r"eyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{3,}\.[A-Za-z0-9_-]{3,}"),
    re.compile(r"-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    re.compile(r"(?i)(client_secret|access_token|refresh_token|id_token|authorization_code|pkce_verifier)\s*[:=]\s*[\"'][^\"']{4,}[\"']"),
    re.compile(r"spur_live_[a-z2-7]{26}_[a-z2-7]{52}"),
)
CREDENTIAL_KEYS = {
    "access_token",
    "authorization",
    "authorization_code",
    "client_secret",
    "id_token",
    "password",
    "pkce_verifier",
    "refresh_token",
    "secret_access_key",
}
SAFE_MARKERS = {
    "",
    "[redacted]",
    "<redacted>",
    "(known after apply)",
    "sensitive",
}


def paths(arguments: list[str]) -> list[Path]:
    found: list[Path] = []
    for argument in arguments:
        path = Path(argument)
        if path.is_dir():
            found.extend(candidate for candidate in path.rglob("*") if candidate.is_file())
        elif path.is_file():
            found.append(path)
        else:
            raise ValueError(f"evidence path does not exist: {path}")
    return found


def inspect_json(value: object, source: Path, location: str = "$") -> list[str]:
    failures: list[str] = []
    if isinstance(value, dict):
        for key, nested in value.items():
            nested_location = f"{location}.{key}"
            if key.lower() in CREDENTIAL_KEYS and isinstance(nested, str) and nested not in SAFE_MARKERS:
                failures.append(f"{source}:{nested_location}: credential-bearing field is not redacted")
            failures.extend(inspect_json(nested, source, nested_location))
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            failures.extend(inspect_json(nested, source, f"{location}[{index}]"))
    return failures


def inspect(path: Path) -> list[str]:
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return [f"{path}: evidence must be UTF-8 text (use terraform show -json for plans)"]

    failures = [f"{path}: credential-shaped content matched {pattern.pattern!r}" for pattern in PATTERNS if pattern.search(text)]
    if path.suffix == ".json":
        try:
            value = json.loads(text)
        except json.JSONDecodeError as error:
            failures.append(f"{path}: invalid JSON: {error}")
        else:
            failures.extend(inspect_json(value, path))
    return failures


def main(arguments: list[str]) -> int:
    if not arguments:
        print(f"usage: {Path(sys.argv[0]).name} PLAN_OR_LOG_JSON [...]", file=sys.stderr)
        return 2
    try:
        evidence_paths = paths(arguments)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2

    failures = [failure for path in evidence_paths for failure in inspect(path)]
    if failures:
        for failure in failures:
            print(f"credential-shaped evidence rejected: {failure}", file=sys.stderr)
        return 1
    print(f"secret scan passed: {len(evidence_paths)} sanitized text artifact(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
