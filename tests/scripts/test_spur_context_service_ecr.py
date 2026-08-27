import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEPLOY_SH = ROOT / "infra" / "spur-context-service" / "deploy.sh"


def deploy_source() -> str:
    return DEPLOY_SH.read_text()


def shell_function(source: str, name: str) -> str:
    match = re.search(
        rf"(?ms)^{re.escape(name)}\(\) \{{\n(.*?)(?=^[a-zA-Z_][a-zA-Z0-9_]*\(\) \{{|\Z)",
        source,
    )
    assert match is not None, f"missing shell function: {name}"
    return match.group(1)


def test_all_ecr_repositories_are_reconciled_with_scanning_enabled():
    source = deploy_source()
    ensure_one = shell_function(source, "ensure_ecr_repository")
    ensure_all = shell_function(source, "ensure_ecr_repositories")

    assert "create-repository" in ensure_one
    assert "--image-scanning-configuration scanOnPush=true" in ensure_one
    assert "put-image-scanning-configuration" in ensure_one
    assert ensure_one.count("--image-scanning-configuration scanOnPush=true") == 2

    for repository in (
        "WORKER_ECR_REPO",
        "WORKER_LAMBDA_ECR_REPO",
        "SOURCE_FETCHER_LAMBDA_ECR_REPO",
    ):
        assert f'ensure_ecr_repository "${repository}"' in ensure_all


def test_build_tags_are_immutable_and_only_latest_is_excluded():
    source = deploy_source()
    ensure_one = shell_function(source, "ensure_ecr_repository")

    assert 'LATEST_IMAGE_TAG="latest"' in source
    assert ensure_one.count("--image-tag-mutability IMMUTABLE_WITH_EXCLUSION") == 2
    assert ensure_one.count("--image-tag-mutability-exclusion-filters") == 2
    assert ensure_one.count('filterType=WILDCARD,filter="$LATEST_IMAGE_TAG"') == 2
    assert re.findall(r"--image-tag-mutability\s+(\S+)", ensure_one) == [
        "IMMUTABLE_WITH_EXCLUSION",
        "IMMUTABLE_WITH_EXCLUSION",
    ]


def test_region_preflight_precedes_ecr_and_terraform_mutations():
    source = deploy_source()
    preflight = shell_function(source, "assert_selected_aws_region")
    main = shell_function(source, "main")

    assert "AWS_REGION" in preflight
    assert "AWS_DEFAULT_REGION" in preflight
    assert "aws configure get region" in preflight
    assert '"$selected_region" != "$AWS_REGION_VAL"' in preflight
    assert main.index("assert_selected_aws_region") < main.index(
        "ensure_ecr_repositories"
    )
    assert main.index("assert_selected_aws_region") < main.index("terraform apply")


def test_aws_cli_capability_preflight_is_read_only_and_precedes_mutations():
    source = deploy_source()
    preflight = shell_function(source, "assert_ecr_exclusion_filter_support")
    main = shell_function(source, "main")

    assert "aws ecr put-image-tag-mutability" in preflight
    assert "--image-tag-mutability IMMUTABLE_WITH_EXCLUSION" in preflight
    assert "--image-tag-mutability-exclusion-filters" in preflight
    assert "--generate-cli-skeleton output" in preflight
    assert "Upgrade AWS CLI v2" in preflight
    assert "no ECR repository was changed" in preflight

    capability_call = main.index("assert_ecr_exclusion_filter_support")
    assert capability_call < main.index("assert_selected_aws_region")
    assert capability_call < main.index("ensure_ecr_repositories")
    assert capability_call < main.index("terraform apply")


def test_duckdb_extensions_have_pinned_sha256_verification_before_install():
    source = deploy_source()
    download = shell_function(source, "download_extensions")

    extension_names = re.search(r'EXTENSIONS=\(([^\n]+)\)', source)
    assert extension_names is not None
    names = re.findall(r'"([a-z0-9_]+)"', extension_names.group(1))
    assert names == [
        "httpfs",
        "ducklake",
        "postgres_scanner",
        "sqlite_scanner",
        "aws",
        "parquet",
        "json",
        "lance",
    ]

    checksum_block = re.search(
        r"(?ms)^EXTENSION_SHA256=\(\n(.*?)^\)", source
    )
    assert checksum_block is not None
    checksums = re.findall(r'"([0-9a-f]{64})"', checksum_block.group(1))
    assert len(checksums) == len(names)
    assert len(set(checksums)) == len(checksums)

    assert 'expected_sha="${EXTENSION_SHA256[$i]}"' in download
    assert 'actual_sha="$(sha256_file "$candidate")"' in download
    assert '[[ "$actual_sha" != "$expected_sha" ]]' in download
    assert download.index('actual_sha="$(sha256_file "$candidate")"') < download.index(
        'mv "$candidate" "$dest"'
    )
    assert "curl --fail --silent --show-error --location" in download
