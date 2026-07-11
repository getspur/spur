#[test]
fn lean_lambda_sources_and_features_exclude_service_and_duckdb() {
    let authorizer = include_str!("../src/bin/api_key_authorizer.rs");
    let cleanup = include_str!("../src/bin/api_key_cleanup.rs");
    for (name, source) in [("authorizer", authorizer), ("cleanup", cleanup)] {
        for forbidden in [
            "spur_context_service",
            "duckdb",
            "mod catalog",
            "crate::catalog",
            "mod mcp",
            "crate::mcp",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} binary must not link {forbidden}"
            );
        }
    }

    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("api-key-authorizer = [\"dep:lambda_runtime\"]"));
    assert!(manifest.contains("api-key-cleanup = [\"dep:lambda_runtime\"]"));
    assert!(manifest.contains("required-features = [\"api-key-authorizer\"]"));
    assert!(manifest.contains("required-features = [\"api-key-cleanup\"]"));
    assert!(manifest.contains(
        "duckdb = { version = \"=1.10504.0\", features = [\"bundled\"], optional = true }"
    ));
}

#[test]
fn repository_packager_emits_deterministic_terraform_artifacts() {
    let script = include_str!("../../../scripts/package-context-api-key-lambdas.sh");

    assert!(script.contains("scripts/spur-cargo"));
    assert!(script.contains("SPUR_REMOTE=0"));
    assert!(script.contains("zigbuild"));
    assert!(script.contains("--target aarch64-unknown-linux-musl"));
    assert!(script.contains("--locked"));
    assert!(script.contains("touch -t 198001010000"));
    assert!(script.contains("zip -X"));
    assert!(script.contains("target/lambda/spur-context-api-key-authorizer.zip"));
    assert!(script.contains("target/lambda/spur-context-api-key-cleanup.zip"));
}
