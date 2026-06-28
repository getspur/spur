#!/usr/bin/env python3
"""Staging E2E smoke for the SPUR context service.

This codifies the manual E1 run:

1. publish a tiny source tarball to S3 and presign it
2. call external_index on the staging Lambda
3. wait for the real worker to fetch, write bronze, persist silver, publish gold,
   and export a frozen serving snapshot
4. query the serving Lambda and assert expected symbols plus vector-backed
   knowledge evidence are returned without a serving Postgres catalog

The script intentionally shells out to the AWS CLI so it works locally and in
GitHub Actions without adding Python package dependencies.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid
from typing import Any


DEFAULT_REGION = "ap-southeast-5"
DEFAULT_SOURCE = "registry:crates-io"
DEFAULT_REVISION = "0.1.0"
DEFAULT_POLL_SECONDS = 15
DEFAULT_TIMEOUT_SECONDS = 30 * 60
DEFAULT_POINTER_KEY = "gold/catalog-snapshot/current.json"
VECTOR_DIMENSIONS = 768


class SmokeFailure(RuntimeError):
    pass


def main() -> int:
    region = env("AWS_REGION", env("AWS_DEFAULT_REGION", DEFAULT_REGION))
    source_bucket = require_env("SPUR_CONTEXT_SMOKE_SOURCE_BUCKET")
    data_bucket = env("SPUR_CONTEXT_SMOKE_DATA_BUCKET", source_bucket)
    lambda_name = require_env("SPUR_CONTEXT_SMOKE_LAMBDA")
    source = env("SPUR_CONTEXT_SMOKE_SOURCE", DEFAULT_SOURCE)
    revision = env("SPUR_CONTEXT_SMOKE_REVISION", DEFAULT_REVISION)
    s3_prefix = env("SPUR_CONTEXT_SMOKE_S3_PREFIX", "smoke/context-service")
    pointer_key = env("SPUR_CONTEXT_SMOKE_SNAPSHOT_POINTER_KEY", DEFAULT_POINTER_KEY)
    timeout_seconds = int(env("SPUR_CONTEXT_SMOKE_TIMEOUT_SECONDS", str(DEFAULT_TIMEOUT_SECONDS)))
    poll_seconds = int(env("SPUR_CONTEXT_SMOKE_POLL_SECONDS", str(DEFAULT_POLL_SECONDS)))

    run_id = env("SPUR_CONTEXT_SMOKE_RUN_ID", uuid.uuid4().hex[:12])
    package = env("SPUR_CONTEXT_SMOKE_PACKAGE", f"spur-context-smoke-{run_id}")

    print(f"[context-smoke] run_id={run_id}")
    print(f"[context-smoke] package={package} revision={revision}")
    print(f"[context-smoke] lambda={lambda_name} region={region}")

    verify_serving_uses_frozen_s3_catalog(lambda_name, region, pointer_key)
    caller_arn = env("SPUR_CONTEXT_SMOKE_CALLER_ARN", caller_identity_arn(region))

    with tempfile.TemporaryDirectory(prefix="spur-context-smoke-") as tmp:
        tmp_path = pathlib.Path(tmp)
        archive = write_fixture_tarball(tmp_path, package, revision)
        source_key = upload_fixture_source(
            archive=archive,
            bucket=source_bucket,
            prefix=s3_prefix,
            run_id=run_id,
            region=region,
        )
        source_url = presign_fixture_source(source_bucket, source_key, region)

        invoker = LambdaInvoker(lambda_name=lambda_name, region=region, caller_arn=caller_arn)
        index_response = invoker.call_tool(
            "external_index",
            {
                "source": source,
                "package": package,
                "revision": revision,
                "source_url": source_url,
                "source_kind": "tarball",
                "force": True,
            },
        )
        if index_response.get("status") == "complete":
            raise SmokeFailure("external_index returned a warm catalog hit; expected real worker")
        job_id = require_json_string(index_response, "job_id")
        print(f"[context-smoke] job_id={job_id}")

        status = wait_for_complete_job(
            invoker=invoker,
            job_id=job_id,
            timeout_seconds=timeout_seconds,
            poll_seconds=poll_seconds,
        )
        assert_nonzero_embeddings(status)
        assert_medallion_objects(
            bucket=data_bucket,
            source=source,
            package=package,
            revision=revision,
            pointer_key=pointer_key,
            region=region,
        )
        assert_serving_queries(invoker, source, package, revision, run_id)

        print("[context-smoke] ok")
        if env("SPUR_CONTEXT_SMOKE_CLEANUP_SOURCE", "0") == "1":
            run(["aws", "s3", "rm", f"s3://{source_bucket}/{source_key}", "--region", region])
    return 0


class LambdaInvoker:
    """Invoke the deployed Lambda handler with an API Gateway-like event.

    The script uses `aws lambda invoke` rather than importing boto3.
    """

    def __init__(self, *, lambda_name: str, region: str, caller_arn: str) -> None:
        self.lambda_name = lambda_name
        self.region = region
        self.caller_arn = caller_arn

    def call_tool(self, tool: str, args: dict[str, Any]) -> dict[str, Any]:
        body = json.dumps({"tool": tool, "args": args}, separators=(",", ":"))
        event = {
            "body": body,
            "isBase64Encoded": False,
            "requestContext": {
                "authorizer": {
                    "iam": {
                        "userArn": self.caller_arn,
                        "callerId": self.caller_arn,
                    }
                }
            },
        }
        with tempfile.NamedTemporaryFile(prefix=f"{tool}-", suffix=".json", delete=False) as out:
            out_path = pathlib.Path(out.name)
        try:
            metadata = run_json(
                [
                    "aws",
                    "lambda",
                    "invoke",
                    "--function-name",
                    self.lambda_name,
                    "--cli-binary-format",
                    "raw-in-base64-out",
                    "--payload",
                    json.dumps(event, separators=(",", ":")),
                    str(out_path),
                    "--region",
                    self.region,
                ]
            )
            if metadata.get("FunctionError"):
                payload = out_path.read_text()
                raise SmokeFailure(f"Lambda FunctionError for {tool}: {payload}")
            envelope = json.loads(out_path.read_text())
            status_code = int(envelope.get("statusCode", 0))
            response_body = json.loads(envelope.get("body") or "{}")
            if status_code >= 400:
                raise SmokeFailure(
                    f"{tool} returned HTTP {status_code}: {json.dumps(response_body, sort_keys=True)}"
                )
            if "error" in response_body:
                raise SmokeFailure(
                    f"{tool} returned tool error: {json.dumps(response_body['error'], sort_keys=True)}"
                )
            return response_body
        finally:
            out_path.unlink(missing_ok=True)


def verify_serving_uses_frozen_s3_catalog(lambda_name: str, region: str, pointer_key: str) -> None:
    config = run_json(
        [
            "aws",
            "lambda",
            "get-function-configuration",
            "--function-name",
            lambda_name,
            "--region",
            region,
        ]
    )
    variables = config.get("Environment", {}).get("Variables", {})
    catalog_uri = str(variables.get("SPUR_CATALOG_S3_URI") or "")
    ingest_catalog = str(variables.get("SPUR_CATALOG_DSN") or "")
    if not catalog_uri.startswith("s3://"):
        raise SmokeFailure(
            "serving Lambda must set SPUR_CATALOG_S3_URI to an S3 frozen snapshot URI"
        )
    if "postgres" in ingest_catalog.lower():
        raise SmokeFailure(
            "serving Lambda must not use SPUR_CATALOG_DSN/Postgres; Postgres is ingest-only"
        )
    if env("SPUR_CONTEXT_SMOKE_ALLOW_NON_POINTER_SNAPSHOT", "0") != "1":
        expected_suffix = "/" + pointer_key.strip("/")
        if not catalog_uri.endswith(expected_suffix):
            raise SmokeFailure(
                f"serving Lambda SPUR_CATALOG_S3_URI must end with {expected_suffix}, got {catalog_uri}"
            )
    print(f"[context-smoke] serving catalog={catalog_uri}")


def caller_identity_arn(region: str) -> str:
    return run(
        [
            "aws",
            "sts",
            "get-caller-identity",
            "--query",
            "Arn",
            "--output",
            "text",
            "--region",
            region,
        ]
    )


def write_fixture_tarball(root: pathlib.Path, package: str, revision: str) -> pathlib.Path:
    fixture_dir = root / f"{package}-{revision}"
    src_dir = fixture_dir / "src"
    src_dir.mkdir(parents=True)
    (fixture_dir / "Cargo.toml").write_text(
        "\n".join(
            [
                "[package]",
                f'name = "{package}"',
                f'version = "{revision}"',
                'edition = "2021"',
                "",
                "[lib]",
                'path = "src/lib.rs"',
                "",
            ]
        ),
        encoding="utf-8",
    )
    (src_dir / "lib.rs").write_text(
        "\n".join(
            [
                "pub struct E1Fixture {",
                "    pub value: u32,",
                "}",
                "",
                "pub fn e1_expected_symbol() -> u32 {",
                "    e1_helper() + 41",
                "}",
                "",
                "fn e1_helper() -> u32 {",
                "    1",
                "}",
                "",
            ]
        ),
        encoding="utf-8",
    )
    (fixture_dir / "README.md").write_text(
        "# Context service smoke fixture\n\nContains e1_expected_symbol.\n",
        encoding="utf-8",
    )

    archive = root / "source.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for path in fixture_dir.rglob("*"):
            tar.add(path, arcname=path.relative_to(fixture_dir.parent))
    return archive


def upload_fixture_source(
    *, archive: pathlib.Path, bucket: str, prefix: str, run_id: str, region: str
) -> str:
    key = f"{prefix.strip('/')}/{run_id}/source.tar.gz"
    run(["aws", "s3", "cp", str(archive), f"s3://{bucket}/{key}", "--region", region])
    print(f"[context-smoke] uploaded source=s3://{bucket}/{key}")
    return key


def presign_fixture_source(bucket: str, key: str, region: str) -> str:
    # Uses `aws s3 presign` so the real worker fetches through the normal URL path.
    return run(
        [
            "aws",
            "s3",
            "presign",
            f"s3://{bucket}/{key}",
            "--expires-in",
            "3600",
            "--region",
            region,
        ]
    )


def wait_for_complete_job(
    *,
    invoker: LambdaInvoker,
    job_id: str,
    timeout_seconds: int,
    poll_seconds: int,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_status: dict[str, Any] | None = None
    while time.monotonic() < deadline:
        status = invoker.call_tool("external_index_status", {"job_id": job_id})
        last_status = status
        state = status.get("status")
        stage = status.get("stage", "<none>")
        print(f"[context-smoke] status={state} stage={stage}")
        if state == "complete":
            return status
        if state == "failed":
            raise SmokeFailure(f"index job failed: {json.dumps(status, sort_keys=True)}")
        time.sleep(poll_seconds)
    raise SmokeFailure(f"timed out waiting for job {job_id}: {json.dumps(last_status)}")


def assert_nonzero_embeddings(status: dict[str, Any]) -> None:
    row_counts = status.get("row_counts") or {}
    symbol_embeddings = int(row_counts.get("symbol_embeddings") or 0)
    if symbol_embeddings <= 0:
        raise SmokeFailure(
            f"expected non-zero symbol_embeddings row count, got {json.dumps(row_counts)}"
        )
    print(f"[context-smoke] symbol_embeddings={symbol_embeddings}")


def assert_medallion_objects(
    *,
    bucket: str,
    source: str,
    package: str,
    revision: str,
    pointer_key: str,
    region: str,
) -> None:
    bronze_key = "bronze/{source}/{package}/{revision}/source.tar.gz".format(
        source=source, package=package, revision=revision
    )
    silver_prefix = "silver/{source}/{package}/{revision}/".format(
        source=source, package=package, revision=revision
    )
    assert_s3_object(bucket, bronze_key, region)
    assert_s3_prefix(bucket, silver_prefix, region)
    assert_s3_object(bucket, pointer_key, region)
    print("[context-smoke] medallion objects present")


def assert_serving_queries(
    invoker: LambdaInvoker,
    source: str,
    package: str,
    revision: str,
    run_id: str,
) -> None:
    search = invoker.call_tool(
        "external_code_search",
        {
            "source": source,
            "package": package,
            "revision": revision,
            "query": "e1_expected_symbol",
            "symbol_kind": "function",
            "limit": 5,
        },
    )
    candidates = search.get("candidates") or []
    if not candidates:
        raise SmokeFailure(f"external_code_search returned no candidates: {json.dumps(search)}")
    selector = require_json_string(candidates[0], "uri")

    source_response = invoker.call_tool(
        "external_code_read",
        {
            "source": source,
            "selector": selector,
            "context_lines": 0,
        },
    )
    if "e1_expected_symbol" not in str(source_response.get("source") or ""):
        raise SmokeFailure(f"external_code_read returned unexpected source: {source_response}")

    query_vec = [0.0] * VECTOR_DIMENSIONS
    query_vec[0] = 1.0
    knowledge = invoker.call_tool(
        "external_knowledge_context",
        {
            "source": source,
            "package": package,
            "revision": revision,
            "query": f"vector-only-{run_id}",
            "scope": "code",
            "limit": 3,
            "query_vec": query_vec,
        },
    )
    evidence = knowledge.get("primary_evidence") or []
    if not any(str(item.get("grounding", "")).startswith("hybrid") for item in evidence):
        raise SmokeFailure(
            "external_knowledge_context did not return vector-backed evidence: "
            + json.dumps(knowledge, sort_keys=True)
        )
    print("[context-smoke] serving queries returned expected symbol and hybrid evidence")


def assert_s3_object(bucket: str, key: str, region: str) -> None:
    run(
        [
            "aws",
            "s3api",
            "head-object",
            "--bucket",
            bucket,
            "--key",
            key,
            "--region",
            region,
        ]
    )


def assert_s3_prefix(bucket: str, prefix: str, region: str) -> None:
    result = run_json(
        [
            "aws",
            "s3api",
            "list-objects-v2",
            "--bucket",
            bucket,
            "--prefix",
            prefix,
            "--max-items",
            "1",
            "--region",
            region,
        ]
    )
    if not result.get("Contents"):
        raise SmokeFailure(f"expected at least one object under s3://{bucket}/{prefix}")


def require_json_string(value: dict[str, Any], key: str) -> str:
    result = value.get(key)
    if not isinstance(result, str) or not result:
        raise SmokeFailure(f"expected non-empty string field `{key}` in {json.dumps(value)}")
    return result


def run_json(args: list[str]) -> dict[str, Any]:
    output = run(args)
    try:
        return json.loads(output or "{}")
    except json.JSONDecodeError as error:
        raise SmokeFailure(f"command did not return JSON: {' '.join(args)}\n{output}") from error


def run(args: list[str]) -> str:
    printable = " ".join(args)
    completed = subprocess.run(args, text=True, capture_output=True, check=False)
    if completed.returncode != 0:
        raise SmokeFailure(
            f"command failed ({completed.returncode}): {printable}\n"
            f"stdout:\n{completed.stdout}\n"
            f"stderr:\n{completed.stderr}"
        )
    return completed.stdout.strip()


def require_env(name: str) -> str:
    value = env(name)
    if not value:
        raise SmokeFailure(f"{name} is required")
    return value


def env(name: str, default: str | None = None) -> str:
    value = os.environ.get(name)
    if value is None or value.strip() == "":
        return default or ""
    return value.strip()


if __name__ == "__main__":
    if shutil.which("aws") is None:
        print("[context-smoke] aws CLI is required", file=sys.stderr)
        raise SystemExit(2)
    try:
        raise SystemExit(main())
    except SmokeFailure as error:
        print(f"[context-smoke] FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
